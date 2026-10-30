use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};

use anyhow::Context;
use futures::FutureExt;

use rand::{RngExt, seq::IteratorRandom};
use size_format::SizeFormatterBinary as SF;
use tracing::{info, trace, warn};

use crate::{
    api::TorrentIdOrHash,
    bitv::BitV,
    bitv_factory::BitVFactory,
    chunk_tracker::{ChunkTracker, compute_selected_pieces},
    file_ops::FileOps,
    type_aliases::{BF, FileStorage},
};

const MAX_FASTRESUME_CHECKS: usize = 64;

use super::{ManagedTorrentShared, TorrentMetadata, paused::TorrentStatePaused};

pub struct TorrentStateInitializing {
    pub(crate) files: FileStorage,
    pub(crate) shared: Arc<ManagedTorrentShared>,
    pub(crate) metadata: Arc<TorrentMetadata>,
    pub(crate) only_files: Option<Vec<usize>>,
    pub(crate) checked_bytes: AtomicU64,
    // FluxDM patch: fast-resume samples have a smaller checking denominator.
    sample_total: AtomicU64,
    pause_requested: AtomicBool,
    check_running: AtomicBool,
    // FluxDM patch: interrupt checks waiting for a hashing slot too.
    pub(crate) pause_notify: tokio::sync::Notify,
    previously_errored: bool,
}

impl TorrentStateInitializing {
    pub fn new(
        shared: Arc<ManagedTorrentShared>,
        metadata: Arc<TorrentMetadata>,
        only_files: Option<Vec<usize>>,
        files: FileStorage,
        previously_errored: bool,
    ) -> Self {
        Self {
            shared,
            metadata,
            only_files,
            files,
            checked_bytes: AtomicU64::new(0),
            sample_total: AtomicU64::new(u64::MAX),
            pause_requested: AtomicBool::new(false),
            check_running: AtomicBool::new(false),
            pause_notify: tokio::sync::Notify::new(),
            previously_errored,
        }
    }

    // FluxDM patch: include boundary pieces, exclude unselected and padding-only pieces.
    fn selected_pieces(&self) -> BF {
        compute_selected_pieces(self.metadata.lengths(),
            |i| self.only_files.as_ref().map(|f| f.contains(&i)).unwrap_or(true),
            &self.metadata.file_infos)
    }

    pub fn checking_total_bytes(&self) -> u64 {
        let sample = self.sample_total.load(Ordering::Relaxed);
        if sample != u64::MAX { return sample; }
        let selected = self.selected_pieces();
        self.metadata.lengths().iter_piece_infos()
            .filter(|p| selected[p.piece_index.get() as usize]).map(|p| p.len as u64).sum()
    }

    pub fn get_checked_bytes(&self) -> u64 {
        self.checked_bytes
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn request_pause(&self) {
        self.pause_requested.store(true, Ordering::Relaxed);
        self.pause_notify.notify_one();
    }

    pub(crate) fn clear_pause_request(&self) {
        self.pause_requested.store(false, Ordering::Relaxed);
        self.pause_notify.notified().now_or_never();
    }

    pub(crate) fn is_pause_requested(&self) -> bool {
        self.pause_requested.load(Ordering::Relaxed)
    }

    pub(crate) fn try_start_check(&self) -> bool {
        self.check_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub(crate) fn finish_check(&self) {
        self.check_running.store(false, Ordering::Release);
    }

    // FluxDM patch: pause waits for hashing to release storage before another operation starts.
    pub(crate) fn is_check_running(&self) -> bool {
        self.check_running.load(Ordering::Acquire)
    }

    async fn validate_fastresume(
        &self,
        bitv_factory: &dyn BitVFactory,
        have_pieces: Option<Box<dyn BitV>>,
    ) -> Option<Box<dyn BitV>> {
        let hp = have_pieces?;
        let actual = hp.as_bytes().len();
        let expected = self.metadata.lengths().piece_bitfield_bytes();
        if actual != expected {
            warn!(
                actual,
                expected,
                "the bitfield loaded isn't of correct length, ignoring it, will do full check"
            );
            return None;
        }

        // FluxDM patch: stale unselected claims must not trigger disk reads.
        let mut hp = hp;
        let selected = self.selected_pieces();
        for i in 0..hp.as_slice().len() {
            if !selected[i] { hp.as_slice_mut().set(i, false); }
        }
        let is_broken = self
            .shared
            .spawner
            .block_in_place_with_semaphore(|| {
                let fo = crate::file_ops::FileOps::new(
                    &self.metadata.info,
                    &self.files,
                    &self.metadata.file_infos,
                );

                let mut to_validate = BF::from_boxed_slice(
                    vec![0u8; self.metadata.lengths().piece_bitfield_bytes()].into_boxed_slice(),
                );
                let mut queue = hp.as_slice().to_owned();

                // Validate at least one piece from each file, if we claim we have it.
                for fi in self.metadata.file_infos.iter() {
                    let prange = fi.piece_range_usize();
                    let offset = prange.start;
                    for piece_id in hp
                        .as_slice()
                        .get(fi.piece_range_usize())
                        .into_iter()
                        .flat_map(|s| s.iter_ones())
                        .map(|pid| pid + offset)
                        .take(1)
                    {
                        to_validate.set(piece_id, true);
                        queue.set(piece_id, false);
                    }
                }

                // For all the remaining pieces we claim we have, validate them with decreasing probability.
                let queue = queue
                    .iter_ones()
                    .sample(&mut rand::rng(), MAX_FASTRESUME_CHECKS);

                for (tmp_id, piece_id) in queue.into_iter().enumerate() {
                    let denom: u32 = (tmp_id + 1).min(50).try_into().unwrap();
                    if rand::rng().random_ratio(1, denom) {
                        to_validate.set(piece_id, true);
                    }
                }

                let total = to_validate.iter_ones().filter_map(|i| self.metadata.lengths().validate_piece_index(i as u32))
                    .map(|i| self.metadata.lengths().piece_length(i) as u64).sum();
                self.sample_total.store(total, Ordering::Relaxed);
                for piece_id in to_validate
                    .iter_ones()
                    .filter_map(|id| {
                        self.metadata
                            .lengths()
                            .validate_piece_index(id.try_into().ok()?)
                    })
                {
                    if self.pause_requested.load(Ordering::Relaxed) { return true; }
                    if !matches!(fo.check_piece(piece_id), Ok(true)) {
                        return true;
                    }

                    self.checked_bytes.fetch_add(self.metadata.lengths().piece_length(piece_id) as u64, Ordering::Relaxed);
                }

                false
            })
            .await;

        if is_broken {
            // FluxDM patch: interruption is not corruption; keep the durable resume map.
            if self.is_pause_requested() { return None; }
            warn!(
                id = ?self.shared.id,
                info_hash = ?self.shared.info_hash,
                "data corrupted, ignoring fastresume data"
            );
            if let Err(e) = bitv_factory.clear(self.shared.id.into()).await {
                warn!(id=?self.shared.id, info_hash = ?self.shared.info_hash, "error clearing bitfield: {e:#}");
            }
            self.checked_bytes.store(0, Ordering::Relaxed);
            self.sample_total.store(u64::MAX, Ordering::Relaxed);
            return None;
        }

        Some(hp)
    }

    pub async fn check(&self) -> anyhow::Result<TorrentStatePaused> {
        let id: TorrentIdOrHash = self.shared.info_hash.into();
        let bitv_factory = self
            .shared
            .session
            .upgrade()
            .context("session is dead")?
            .bitv_factory
            .clone();
        let have_pieces = if self.previously_errored {
            if let Err(e) = bitv_factory.clear(id).await {
                warn!(id=?self.shared.id, info_hash = ?self.shared.info_hash, error=?e, "error clearing bitfield");
            }
            None
        } else {
            bitv_factory
                .load(id)
                .await
                .context("error loading have_pieces")?
        };

        let have_pieces = self.validate_fastresume(&*bitv_factory, have_pieces).await;
        anyhow::ensure!(!self.is_pause_requested(), "initial check paused");

        let have_pieces = match have_pieces {
            Some(h) => h,
            None => {
                info!("Doing initial checksum validation, this might take a while...");
                let have_pieces = self
                    .shared
                    .spawner
                    .block_in_place_with_semaphore(|| {
                        FileOps::new(&self.metadata.info, &self.files, &self.metadata.file_infos)
                            .initial_check(&self.checked_bytes, &self.pause_requested, &self.selected_pieces())
                    })
                    .await?;
                bitv_factory
                    .store_initial_check(id, have_pieces)
                    .await
                    .context("error storing initial check bitfield")?
            }
        };

        let selected_pieces = compute_selected_pieces(
            self.metadata.lengths(),
            |idx| {
                self.only_files
                    .as_ref()
                    .map(|o| o.contains(&idx))
                    .unwrap_or(true)
            },
            &self.metadata.file_infos,
        );

        let chunk_tracker = ChunkTracker::new(
            have_pieces.into_dyn(),
            selected_pieces,
            *self.metadata.lengths(),
            &self.metadata.file_infos,
        )
        .context("error creating chunk tracker")?;

        let hns = chunk_tracker.get_hns();

        info!(
            torrent=?self.shared.id,
            "Initial check results: have {}, needed {}, total selected {}",
            SF::new(hns.have_bytes),
            SF::new(hns.needed_bytes),
            SF::new(hns.selected_bytes)
        );

        // Ensure file lengths are correct, and reopen read-only.
        self.shared
            .spawner
            .block_in_place_with_semaphore(|| {
                for (idx, fi) in self.metadata.file_infos.iter().enumerate() {
                    if self
                        .only_files
                        .as_ref()
                        .map(|v| v.contains(&idx))
                        .unwrap_or(true)
                    {
                        let now = Instant::now();
                        if fi.attrs.padding {
                            continue;
                        }
                        if let Err(err) = self.files.ensure_file_length(idx, fi.len) {
                            warn!(
                                id=?self.shared.id, info_hash = ?self.shared.info_hash,
                                "Error setting length for file {:?} to {}: {:#?}",
                                fi.relative_filename, fi.len, err
                            );
                        } else {
                            trace!(
                                "Set length for file {:?} to {} in {:?}",
                                fi.relative_filename,
                                SF::new(fi.len),
                                now.elapsed()
                            );
                        }
                    }
                }
                Ok::<_, anyhow::Error>(())
            })
            .await?;

        let paused = TorrentStatePaused {
            shared: self.shared.clone(),
            metadata: self.metadata.clone(),
            files: self.files.take()?,
            chunk_tracker,
            streams: Arc::new(Default::default()),
        };
        // FluxDM patch: a paused torrent keeps its files open too - hold the finished ones read-only
        // so other programs can open them ("file is in use" otherwise).
        for (i, fi) in paused.metadata.file_infos.iter().enumerate() {
            if !fi.attrs.padding && paused.chunk_tracker.is_file_finished(fi) {
                paused.files.on_file_completed(i)?;
            }
        }
        Ok(paused)
    }
}
