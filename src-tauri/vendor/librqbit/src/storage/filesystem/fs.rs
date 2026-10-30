use super::opened_file::OpenedFile;
use crate::storage::{StorageFactory, TorrentStorage};
use crate::{
    storage::{StorageFactoryExt, filesystem::opened_file::OurFileExt},
    torrent_state::{ManagedTorrentShared, TorrentMetadata},
};
use anyhow::Context;
use parking_lot::RwLock;
use std::{
    fs::OpenOptions,
    io::IoSlice,
    path::{Path, PathBuf},
};
use tracing::warn;

// FluxDM patch: one sparse file per hash, including when torrents share an output folder.
pub fn partfile_path(output: &Path, hash: impl std::fmt::Display) -> PathBuf {
    output.join(".fluxdm.parts").join(hash.to_string())
}

#[derive(Default, Clone, Copy)]
pub struct FilesystemStorageFactory { pub mark_unfinished: bool }
impl StorageFactory for FilesystemStorageFactory {
    type Storage = FilesystemStorage;
    fn create(
        &self,
        shared: &ManagedTorrentShared,
        _metadata: &TorrentMetadata,
    ) -> anyhow::Result<FilesystemStorage> {
        Ok(FilesystemStorage {
            output_folder: shared.options.output_folder.clone(),
            opened_files: Default::default(),
            parts_path: partfile_path(&shared.options.output_folder, shared.info_hash.as_string()),
            parts: OpenedFile::new_dummy(),
            routing: RwLock::new(Routing::default()),
            infos: Vec::new(),
            mark_unfinished: self.mark_unfinished,
        })
    }
    fn clone_box(&self) -> crate::storage::BoxStorageFactory {
        self.boxed()
    }
}

#[derive(Default, Clone)]
struct Routing {
    selection: Option<Vec<usize>>,
    wanted: Vec<bool>,
}

pub struct FilesystemStorage {
    pub(crate) output_folder: PathBuf,
    pub(crate) opened_files: Vec<OpenedFile>,
    parts_path: PathBuf,
    parts: OpenedFile,
    routing: RwLock<Routing>,
    infos: Vec<crate::file_info::FileInfo>,
    mark_unfinished: bool,
}

/// Append, rather than replace, the original extension.
pub fn marked_path(path: &Path) -> PathBuf {
    let mut name = String::new();
    let mut units = 0;
    for c in path.file_name().unwrap_or_default().to_string_lossy().chars() {
        units += c.len_utf16();
        if units > 251 { break; }
        name.push(c);
    }
    name.push_str(".!fx");
    path.with_file_name(name)
}

impl FilesystemStorage {
    fn physical_path(&self, id: usize) -> PathBuf {
        let normal = self.output_folder.join(&self.infos[id].relative_filename);
        let marked = marked_path(&normal);
        if self.mark_unfinished && (marked.exists() || !normal.exists()) { marked } else { normal }
    }

    #[allow(dead_code)]
    pub(crate) fn take_fs(&self) -> anyhow::Result<Self> {
        let routing = self.routing.write();
        Ok(Self {
            opened_files: self
                .opened_files
                .iter()
                .map(|f| f.take_clone())
                .collect::<anyhow::Result<Vec<_>>>()?,
            output_folder: self.output_folder.clone(),
            parts_path: self.parts_path.clone(),
            parts: self.parts.take_clone()?,
            routing: RwLock::new(routing.clone()),
            infos: self.infos.clone(),
            mark_unfinished: self.mark_unfinished,
        })
    }

    // Caller holds the routing lock, excluding readers/writers during migration.
    fn open_parts(&self) -> anyhow::Result<()> {
        if self.parts.lock_read().is_ok() {
            return Ok(());
        }
        let parent = self.parts_path.parent().context("no part-file parent")?;
        std::fs::create_dir_all(parent)?;
        #[cfg(windows)]
        super::sparse::hide_directory(parent)?;
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.parts_path)?;
        #[cfg(windows)]
        {
            super::sparse::mark_file_sparse(&f);
        }
        self.parts
            .replace(OpenedFile::new(self.parts_path.clone(), f));
        Ok(())
    }

    fn open_real(&self, id: usize, overwrite: bool) -> anyhow::Result<OpenedFile> {
        let path = self.physical_path(id);
        std::fs::create_dir_all(path.parent().context("no file parent")?)?;
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(overwrite)
            .create_new(!overwrite)
            .truncate(false)
            .open(&path)?;
        Ok(OpenedFile::new(path, f))
    }

    fn demote(&self, id: usize) -> anyhow::Result<()> {
        let path = self.physical_path(id);
        if !path.try_exists()? {
            return Ok(());
        }
        self.open_parts()?;
        // Sync the retained copy before removing the old name. A failed copy keeps the source.
        let source = std::fs::File::open(&path)?;
        copy_sparse(
            &source,
            0,
            &*self.parts.lock_read()?,
            self.infos[id].offset_in_torrent,
            source.metadata()?.len().min(self.infos[id].len),
            false,
        )?;
        self.parts.lock_read()?.sync_data()?;
        drop(source);
        self.opened_files[id].replace(OpenedFile::new_dummy());
        if let Err(e) = std::fs::remove_file(&path) {
            self.opened_files[id].replace(self.open_real(id, true)?);
            return Err(e.into());
        }
        Ok(())
    }

    fn promote(&self, id: usize) -> anyhow::Result<()> {
        let info = &self.infos[id];
        let path = self.physical_path(id);
        if path.try_exists()? {
            return Ok(());
        }
        std::fs::create_dir_all(path.parent().context("no file parent")?)?;
        // Atomic publication keeps a crash during migration from exposing a partial real file
        // while saved have bits still claim its boundary pieces.
        let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        #[cfg(windows)]
        {
            super::sparse::mark_file_sparse(temp.as_file());
        }
        if self.parts_path.try_exists()? {
            self.open_parts()?;
            let source = self.parts.lock_read()?;
            let len = source
                .metadata()?
                .len()
                .saturating_sub(info.offset_in_torrent)
                .min(info.len);
            copy_sparse(
                &source,
                info.offset_in_torrent,
                temp.as_file(),
                0,
                len,
                true,
            )?;
            temp.as_file_mut().set_len(info.len)?;
        }
        temp.as_file().sync_data()?;
        temp.persist_noclobber(&path).map_err(|e| e.error)?;
        Ok(())
    }
}

// FluxDM patch: bounded-memory migration, leaving zero runs as holes on a new sparse target.
fn copy_sparse(
    source: &std::fs::File,
    from: u64,
    target: &std::fs::File,
    to: u64,
    len: u64,
    new_target: bool,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 1024 * 1024];
    let mut pos = 0;
    while pos < len {
        let n = (len - pos).min(buf.len() as u64) as usize;
        source.pread_exact(from + pos, &mut buf[..n])?;
        // Write zeros too: an existing part-file range may contain older nonzero data.
        if !new_target || buf[..n].iter().any(|b| *b != 0) {
            target.pwrite_all(to + pos, &buf[..n])?;
        }
        pos += n as u64;
    }
    Ok(())
}

impl TorrentStorage for FilesystemStorage {
    fn set_file_selection(&self, selection: Option<&[usize]>) -> anyhow::Result<()> {
        let mut routing = self.routing.write();
        for (id, info) in self.infos.iter().enumerate() {
            if info.attrs.padding {
                continue;
            }
            let wanted = selection.is_none_or(|s| s.contains(&id));
            if wanted == routing.wanted[id] {
                continue;
            }
            if wanted {
                self.promote(id)?;
                self.opened_files[id].replace(self.open_real(id, true)?);
            } else {
                self.demote(id)?;
            }
            routing.wanted[id] = wanted;
        }
        routing.selection = selection.map(|s| s.to_vec());
        Ok(())
    }
    fn pread_exact(&self, id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let routing = self.routing.read();
        let info = self.infos.get(id).context("no such file")?;
        if routing.wanted[id] {
            self.opened_files[id].lock_read()?.pread_exact(offset, buf)
        } else {
            self.parts
                .lock_read()?
                .pread_exact(info.offset_in_torrent + offset, buf)
        }
    }
    fn pwrite_all(&self, id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        let routing = self.routing.read();
        let info = self.infos.get(id).context("no such file")?;
        let (f, offset) = if routing.wanted[id] {
            (&self.opened_files[id], offset)
        } else {
            (&self.parts, info.offset_in_torrent + offset)
        };
        #[cfg(windows)]
        return f.try_mark_sparse()?.pwrite_all(offset, buf);
        #[cfg(not(windows))]
        return f.lock_read()?.pwrite_all(offset, buf);
    }
    fn pwrite_all_vectored(
        &self,
        id: usize,
        offset: u64,
        bufs: [IoSlice<'_>; 2],
    ) -> anyhow::Result<usize> {
        // Hold the same routing generation across both buffers.
        let routing = self.routing.read();
        let info = self.infos.get(id).context("no such file")?;
        let (f, offset) = if routing.wanted[id] {
            (&self.opened_files[id], offset)
        } else {
            (&self.parts, info.offset_in_torrent + offset)
        };
        #[cfg(windows)]
        return f.try_mark_sparse()?.pwrite_all_vectored(offset, bufs);
        #[cfg(not(windows))]
        return f.lock_read()?.pwrite_all_vectored(offset, bufs);
    }
    fn remove_partfile(&self) -> anyhow::Result<()> {
        let _routing = self.routing.write();
        self.parts.replace(OpenedFile::new_dummy());
        match std::fs::remove_file(&self.parts_path) {
            Ok(()) => {
                let _ = std::fs::remove_dir(self.parts_path.parent().unwrap());
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
    fn remove_file(&self, id: usize, filename: &Path) -> anyhow::Result<()> {
        let _routing = self.routing.write();
        if let Some(f) = self.opened_files.get(id) {
            f.replace(OpenedFile::new_dummy());
        }
        let normal = self.output_folder.join(filename);
        let marked = marked_path(&normal);
        let path = if self.mark_unfinished && marked.exists() { marked } else { normal };
        match std::fs::remove_file(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => Ok(r?),
        }
    }
    fn on_file_completed(&self, id: usize) -> anyhow::Result<()> {
        let routing = self.routing.read();
        if !routing.wanted[id] {
            return Ok(());
        }
        let f = self.opened_files.get(id).context("no such file")?;
        let normal = self.output_folder.join(&self.infos[id].relative_filename);
        f.publish(&normal)?;
        f.reopen_read_only()
    }
    fn ensure_file_length(&self, id: usize, len: u64) -> anyhow::Result<()> {
        let routing = self.routing.read();
        if !routing.wanted[id] {
            return Ok(());
        }
        let f = self.opened_files.get(id).context("no such file")?;
        #[cfg(windows)]
        f.try_mark_sparse()?;
        Ok(f.lock_read()?.set_len(len)?)
    }
    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        Ok(Box::new(self.take_fs()?))
    }
    fn remove_directory_if_empty(&self, path: &Path) -> anyhow::Result<()> {
        let path = self.output_folder.join(path);
        if !path.is_dir() {
            anyhow::bail!("cannot remove dir: {path:?} is not a directory")
        }
        if std::fs::read_dir(&path)?.count() == 0 {
            std::fs::remove_dir(&path).with_context(|| format!("error removing {path:?}"))
        } else {
            warn!("did not remove {path:?} as it was not empty");
            Ok(())
        }
    }
    fn init(
        &mut self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        self.infos = metadata.file_infos.iter().cloned().collect();
        // Reserve the internal directory, so torrent paths cannot overwrite retained bytes.
        anyhow::ensure!(
            !self.infos.iter().any(
                |i| i.relative_filename.components().next().is_some_and(|c| c
                    .as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(".fluxdm.parts"))
            ),
            "torrent uses the reserved .fluxdm.parts directory"
        );
        // Reserve both names: a torrent must never confuse a real filename with a marker.
        let names: std::collections::HashSet<_> = self.infos.iter().map(|i| i.relative_filename.to_string_lossy().to_lowercase()).collect();
        if self.mark_unfinished {
            let markers: std::collections::HashSet<_> = self.infos.iter().map(|i| marked_path(&i.relative_filename).to_string_lossy().to_lowercase()).collect();
            anyhow::ensure!(markers.len() == self.infos.len(), "torrent unfinished marker filenames collide");
            anyhow::ensure!(!self.infos.iter().any(|i| names.contains(&marked_path(&i.relative_filename).to_string_lossy().to_lowercase())), "torrent filenames collide with unfinished markers");
        }
        self.opened_files = self.infos.iter().map(|_| OpenedFile::new_dummy()).collect();
        let mut routing = self.routing.write();
        routing.wanted = self
            .infos
            .iter()
            .enumerate()
            .map(|(id, _)| routing.selection.as_ref().is_none_or(|s| s.contains(&id)))
            .collect();
        if self.parts_path.try_exists()? {
            self.open_parts()?;
        }
        for (id, info) in self.infos.iter().enumerate() {
            if info.attrs.padding {
                continue;
            }
            if routing.wanted[id] {
                let normal = self.output_folder.join(&info.relative_filename);
                let marked = marked_path(&normal);
                anyhow::ensure!(!self.mark_unfinished || !(normal.try_exists()? && marked.try_exists()?), "both finished and unfinished torrent filenames exist");
                if self.mark_unfinished && normal.try_exists()? {
                    super::sparse::rename_new(&normal, &marked)?;
                }
                let existed = self.physical_path(id).try_exists()?;
                if !existed && self.parts_path.try_exists()? {
                    self.promote(id)?;
                }
                self.opened_files[id].replace(self.open_real(
                    id,
                    shared.options.allow_overwrite || !existed && self.parts_path.try_exists()?,
                )?);
            } else {
                self.open_parts()?;
                self.demote(id)?;
            }
        }
        Ok(())
    }
}
