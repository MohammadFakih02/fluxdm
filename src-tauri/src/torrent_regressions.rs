//! Local torrent regressions: no external trackers, discovery or downloads.
use super::*;
use anyhow::Context;
use librqbit::storage::{StorageFactory, StorageFactoryExt, TorrentStorage};
use librqbit::{ManagedTorrentState, TorrentStatsState};
use sha1::Digest;

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        let p = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("torrent-regression-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn torrent(files: &[(&str, usize)], trackers: &[String]) -> (Vec<u8>, Vec<u8>) {
    let data: Vec<u8> = (0..files.iter().map(|(_, n)| n).sum::<usize>())
        .map(|i| (i % 251) as u8)
        .collect();
    let pieces: Vec<u8> = data
        .chunks(16384)
        .flat_map(|c| sha1::Sha1::digest(c).to_vec())
        .collect();
    let mut b = b"d".to_vec();
    if !trackers.is_empty() {
        b.extend_from_slice(b"13:announce-listl");
        for t in trackers {
            b.extend_from_slice(format!("l{}:{}e", t.len(), t).as_bytes());
        }
        b.extend_from_slice(b"e");
    }
    b.extend_from_slice(b"4:infod5:filesl");
    for (name, len) in files {
        b.extend_from_slice(format!("d6:lengthi{len}e4:pathl{}:{name}ee", name.len()).as_bytes());
    }
    b.extend_from_slice(
        format!(
            "e4:name4:test12:piece lengthi16384e6:pieces{}:",
            pieces.len()
        )
        .as_bytes(),
    );
    b.extend_from_slice(&pieces);
    b.extend_from_slice(b"ee");
    (b, data)
}

fn options(persist: Option<PathBuf>) -> SessionOptions {
    SessionOptions {
        dht: None,
        disable_local_service_discovery: true,
        persistence: persist.map(|folder| SessionPersistenceConfig::Json {
            folder: Some(folder),
        }),
        fastresume: true,
        ..Default::default()
    }
}
async fn settled(h: &ManagedTorrentHandle) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while matches!(h.stats().state, TorrentStatsState::Initializing { .. }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("check did not finish");
    assert!(
        !matches!(h.stats().state, TorrentStatsState::Error),
        "{:?}",
        h.stats().error
    );
}

#[derive(Clone)]
struct ReadSpy {
    data: Arc<Vec<u8>>,
    offsets: Arc<Vec<usize>>,
    reads: Arc<Mutex<Vec<(usize, usize)>>>,
    delay: bool,
    read_gate: Option<(
        Arc<std::sync::atomic::AtomicBool>,
        Arc<std::sync::atomic::AtomicBool>,
        Arc<std::sync::atomic::AtomicBool>,
    )>,
    finish_gate: Option<(
        Arc<std::sync::atomic::AtomicBool>,
        Arc<std::sync::atomic::AtomicBool>,
    )>,
}
impl StorageFactory for ReadSpy {
    type Storage = Self;
    fn create(
        &self,
        _: &librqbit::ManagedTorrentShared,
        _: &librqbit::TorrentMetadata,
    ) -> anyhow::Result<Self> {
        Ok(self.clone())
    }
    fn clone_box(&self) -> librqbit::storage::BoxStorageFactory {
        self.clone().boxed()
    }
}
impl TorrentStorage for ReadSpy {
    fn init(
        &mut self,
        _: &librqbit::ManagedTorrentShared,
        _: &librqbit::TorrentMetadata,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    fn pread_exact(&self, file: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        if let Some((armed, entered, release)) = &self.read_gate {
            if armed.load(Ordering::Acquire) {
                entered.store(true, Ordering::Release);
                while !release.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        if self.delay {
            std::thread::sleep(Duration::from_millis(2));
        }
        let start = self.offsets[file] + offset as usize;
        self.reads.lock().push((start, buf.len()));
        buf.copy_from_slice(&self.data[start..start + buf.len()]);
        Ok(())
    }
    fn pwrite_all(&self, _: usize, _: u64, _: &[u8]) -> anyhow::Result<()> {
        Err(std::io::Error::from_raw_os_error(112)).context("writing a torrent chunk")
    }
    fn remove_file(&self, _: usize, _: &Path) -> anyhow::Result<()> {
        Ok(())
    }
    fn remove_directory_if_empty(&self, _: &Path) -> anyhow::Result<()> {
        Ok(())
    }
    fn ensure_file_length(&self, _: usize, _: u64) -> anyhow::Result<()> {
        Ok(())
    }
    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        if let Some((_, entered, release)) = &self.read_gate {
            anyhow::ensure!(
                !entered.load(Ordering::Acquire) || release.load(Ordering::Acquire),
                "storage transferred before the active callback finished"
            );
        }
        Ok(Box::new(self.clone()))
    }
    fn on_file_completed(&self, _: usize) -> anyhow::Result<()> {
        if let Some((entered, release)) = &self.finish_gate {
            entered.store(true, Ordering::Release);
            while !release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn selected_check_reads_boundaries_but_never_unselected_pieces() {
    let dir = TestDir::new();
    let files = [("skip-a", 32868), ("wanted", 32768), ("skip-b", 49152)];
    let (bytes, data) = torrent(&files, &[]);
    let reads = Arc::new(Mutex::new(Vec::new()));
    let spy = ReadSpy {
        data: Arc::new(data),
        offsets: Arc::new(vec![0, 32868, 65636]),
        reads: reads.clone(),
        delay: false,
        finish_gate: None,
        read_gate: None,
    };
    let s = Session::new_with_opts(dir.0.clone(), options(None))
        .await
        .unwrap();
    let h = s
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                paused: true,
                only_files: Some(vec![1]),
                storage_factory: Some(spy.boxed()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    settled(&h).await;
    assert!(h.stats().finished);
    let log = reads.lock().clone();
    assert_eq!(log.iter().map(|(_, n)| n).sum::<usize>(), 3 * 16384);
    assert!(log
        .iter()
        .all(|(offset, len)| *offset >= 32768 && offset + len <= 81920));
    assert!(log.iter().any(|(offset, _)| *offset < 32868));
    assert!(log.iter().any(|(offset, len)| offset + len > 65636));
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_during_check_never_starts_the_torrent() {
    let dir = TestDir::new();
    let (bytes, data) = torrent(&[("file", 4 * 1024 * 1024)], &[]);
    let reads = Arc::new(Mutex::new(Vec::new()));
    let spy = ReadSpy {
        data: Arc::new(data),
        offsets: Arc::new(vec![0]),
        reads: reads.clone(),
        delay: true,
        finish_gate: None,
        read_gate: None,
    };
    let s = Session::new_with_opts(dir.0.clone(), options(None))
        .await
        .unwrap();
    let h = s
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                storage_factory: Some(spy.boxed()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while reads.lock().is_empty() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    s.pause(&h).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(h.is_paused());
    assert!(!matches!(h.stats().state, TorrentStatsState::Live));
    let n = reads.lock().len();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(reads.lock().len(), n, "paused check kept reading");
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn force_announce_wakes_each_tracker_without_checking_or_replacing_live_state() {
    use axum::{routing::get, Router};
    use std::sync::atomic::AtomicUsize;
    let dir = TestDir::new();
    let calls = Arc::new([
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    ]);
    let mut app = Router::new();
    for i in 0..3 {
        let calls = calls.clone();
        app = app.route(
            &format!("/{i}"),
            get(move || {
                let calls = calls.clone();
                async move {
                    let attempt = calls[i].fetch_add(1, Ordering::Relaxed);
                    let status = if i == 2 && attempt == 0 {
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR
                    } else {
                        axum::http::StatusCode::OK
                    };
                    (
                        status,
                        b"d8:intervali3600e12:min intervali3600e5:peers0:e".to_vec(),
                    )
                }
            }),
        );
    }
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let udp = tokio::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let udp_addr = udp.local_addr().unwrap();
    let udp_calls = Arc::new(AtomicUsize::new(0));
    let udp_count = udp_calls.clone();
    let udp_server = tokio::spawn(async move {
        let mut buf = [0; 2048];
        loop {
            let (n, peer) = udp.recv_from(&mut buf).await.unwrap();
            assert!(n >= 16);
            let action = u32::from_be_bytes(buf[8..12].try_into().unwrap());
            let mut reply = Vec::from(&buf[8..16]);
            match action {
                0 => reply.extend_from_slice(&123u64.to_be_bytes()),
                1 => {
                    udp_count.fetch_add(1, Ordering::Relaxed);
                    for value in [3600u32, 0, 1] {
                        reply.extend_from_slice(&value.to_be_bytes());
                    }
                }
                _ => panic!("unexpected tracker action"),
            }
            udp.send_to(&reply, peer).await.unwrap();
        }
    });
    let urls = vec![
        format!("http://{addr}/0"),
        format!("http://{addr}/1"),
        format!("http://{addr}/2"),
        format!("udp://{udp_addr}/announce"),
    ];
    let (bytes, data) = torrent(&[("file", 16384 * 4)], &urls);
    std::fs::write(dir.0.join("file"), data).unwrap();
    let s = Session::new_with_opts(dir.0.clone(), options(None))
        .await
        .unwrap();
    let h = s
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(dir.0.to_string_lossy().into_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    settled(&h).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while calls.iter().any(|c| c.load(Ordering::Relaxed) == 0)
            || udp_calls.load(Ordering::Relaxed) == 0
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let live = h.live().unwrap();
    let before = h.stats().progress_bytes;
    h.force_announce().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while calls.iter().any(|c| c.load(Ordering::Relaxed) < 2)
            || udp_calls.load(Ordering::Relaxed) < 2
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("manual announce waited for the tracker interval");
    assert!(Arc::ptr_eq(&live, &h.live().unwrap()));
    assert_eq!(h.stats().progress_bytes, before);
    s.pause(&h).await.unwrap();
    assert!(h
        .force_announce()
        .unwrap_err()
        .to_string()
        .contains("Start it first"));
    s.stop().await;
    server.abort();
    udp_server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn paused_restart_and_move_use_resume_samples_recheck_hashes_all_and_rejects_corruption() {
    let dir = TestDir::new();
    let out = dir.0.join("out");
    let moved = dir.0.join("moved");
    let db = dir.0.join("session");
    std::fs::create_dir_all(&out).unwrap();
    let (bytes, data) = torrent(&[("file", 16384 * 128)], &[]);
    std::fs::write(out.join("file"), &data).unwrap();
    let s = Session::new_with_opts(out.clone(), options(Some(db.clone())))
        .await
        .unwrap();
    let h = s
        .add_torrent(
            AddTorrent::from_bytes(bytes.clone()),
            Some(AddTorrentOptions {
                overwrite: true,
                paused: true,
                output_folder: Some(out.to_string_lossy().into_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    settled(&h).await;
    assert!(h.stats().finished);
    s.pause(&h).await.unwrap(); // Already paused still saves its intent.
    let hash = h.info_hash();
    s.stop().await;
    drop(h);
    drop(s);
    let s = Session::new_with_opts(out.clone(), options(Some(db.clone())))
        .await
        .unwrap();
    let h = s.get(hash.into()).unwrap();
    let init = h.with_state(|state| match state {
        ManagedTorrentState::Initializing(i) => Some(i.clone()),
        _ => None,
    });
    settled(&h).await;
    assert!(h.is_paused());
    assert!(h.stats().finished);
    if let Some(i) = init {
        assert!(
            i.get_checked_bytes() < data.len() as u64,
            "restart ran a full check"
        );
    }
    s.detach_for_move(&h).await.unwrap();
    drop(h);
    std::fs::rename(&out, &moved).unwrap();
    let h = s
        .add_torrent(
            AddTorrent::from_bytes(bytes.clone()),
            Some(AddTorrentOptions {
                overwrite: true,
                paused: true,
                output_folder: Some(moved.to_string_lossy().into_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    let init = h.with_state(|state| match state {
        ManagedTorrentState::Initializing(i) => Some(i.clone()),
        _ => None,
    });
    settled(&h).await;
    assert!(h.stats().finished);
    if let Some(i) = init {
        assert!(
            i.get_checked_bytes() < data.len() as u64,
            "move ran a full check"
        );
    }
    s.delete(hash.into(), false).await.unwrap();
    drop(h);
    let h = s
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                overwrite: true,
                paused: true,
                output_folder: Some(moved.to_string_lossy().into_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    let init = h
        .with_state(|state| match state {
            ManagedTorrentState::Initializing(i) => Some(i.clone()),
            _ => None,
        })
        .unwrap();
    settled(&h).await;
    assert_eq!(
        init.get_checked_bytes(),
        data.len() as u64,
        "explicit recheck did not hash all selected bytes"
    );
    s.stop().await;
    drop(h);
    drop(s);
    std::fs::write(moved.join("file"), vec![255; data.len()]).unwrap();
    let s = Session::new_with_opts(moved, options(Some(db)))
        .await
        .unwrap();
    let h = s.get(hash.into()).unwrap();
    settled(&h).await;
    assert!(h.is_paused());
    assert!(!h.stats().finished);
    assert_eq!(
        h.stats().progress_bytes,
        0,
        "failed hash was accepted as valid resume data"
    );
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn small_swarm_pause_and_completion_flush_resume_bits_before_exit() {
    let dir = TestDir::new();
    let seed_out = dir.0.join("seed");
    let leech_out = dir.0.join("leech");
    let db = dir.0.join("resume");
    std::fs::create_dir_all(&seed_out).unwrap();
    let (bytes, data) = torrent(&[("file", 16384 * 32)], &[]);
    std::fs::write(seed_out.join("file"), &data).unwrap();
    let mut opts = options(None);
    opts.listen = Some(ListenerOptions {
        mode: ListenerMode::TcpOnly,
        listen_addr: (std::net::Ipv4Addr::LOCALHOST, 0).into(),
        ..Default::default()
    });
    let seed = Session::new_with_opts(seed_out.clone(), opts)
        .await
        .unwrap();
    let sh = seed
        .add_torrent(
            AddTorrent::from_bytes(bytes.clone()),
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(seed_out.to_string_lossy().into_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    settled(&sh).await;
    let client = Session::new_with_opts(leech_out.clone(), options(Some(db.clone())))
        .await
        .unwrap();
    let h = client
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(leech_out.to_string_lossy().into_owned()),
                initial_peers: Some(vec![seed.listen_addr().unwrap()]),
                ratelimits: LimitsConfig {
                    download_bps: NonZeroU32::new(64 * 1024),
                    upload_bps: None,
                },
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    settled(&h).await;
    tokio::time::timeout(Duration::from_secs(20), async {
        while h.stats().progress_bytes < 16384 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("local swarm did not transfer a piece");
    client.pause(&h).await.unwrap();
    let progress = h.stats().progress_bytes;
    assert!(progress > 0 && progress < data.len() as u64);
    let bitfile = std::fs::read_dir(&db)
        .unwrap()
        .map(|p| p.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "bitv"))
        .unwrap();
    let bits = std::fs::read(&bitfile).unwrap();
    assert_eq!(
        bits.iter().map(|b| b.count_ones() as u64).sum::<u64>() * 16384,
        progress,
        "pause returned before small-transfer resume data was durable"
    );
    for _ in 0..20 {
        client.unpause(&h).await.unwrap();
        client.pause(&h).await.unwrap();
        assert!(h.is_paused());
        assert!(matches!(h.stats().state, TorrentStatsState::Paused));
    }
    h.set_ratelimits(LimitsConfig::default());
    client.unpause(&h).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), h.wait_until_completed())
        .await
        .unwrap_or_else(|e| {
            panic!(
                "swarm resume timeout: {e}; stats: {:?}; peers: {:?}",
                h.stats(),
                serde_json::to_value(
                    h.live()
                        .unwrap()
                        .per_peer_stats_snapshot(Default::default())
                )
                .unwrap()
            )
        })
        .unwrap();
    assert_eq!(
        std::fs::read(&bitfile).unwrap(),
        vec![255; 4],
        "completion did not flush all pieces"
    );
    assert_eq!(std::fs::read(leech_out.join("file")).unwrap(), data);
    client.stop().await;
    seed.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_at_check_completion_preserves_paused_intent() {
    use std::sync::atomic::AtomicBool;
    let dir = TestDir::new();
    let (bytes, data) = torrent(&[("file", 16384)], &[]);
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let spy = ReadSpy {
        data: Arc::new(data),
        offsets: Arc::new(vec![0]),
        reads: Default::default(),
        delay: false,
        finish_gate: Some((entered.clone(), release.clone())),
        read_gate: None,
    };
    let s = Session::new_with_opts(dir.0.clone(), options(None))
        .await
        .unwrap();
    let h = s
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                storage_factory: Some(spy.boxed()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    let arrived = tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
    if arrived.is_err() {
        release.store(true, Ordering::Release);
        panic!("check completion hook not reached");
    }
    let session = s.clone();
    let handle = h.clone();
    let pausing = tokio::spawn(async move { session.pause(&handle).await.unwrap() });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !h.is_paused() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        !pausing.is_finished(),
        "pause returned while storage was still in use"
    );
    release.store(true, Ordering::Release);
    pausing.await.unwrap();
    settled(&h).await;
    assert!(h.is_paused());
    assert!(matches!(h.stats().state, TorrentStatsState::Paused));
    s.stop().await;
}

async fn local_add(
    s: &Arc<Session>,
    bytes: Vec<u8>,
    out: &Path,
    selection: Option<Vec<usize>>,
    peers: Vec<std::net::SocketAddr>,
    paused: bool,
) -> ManagedTorrentHandle {
    let h = s
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(out.to_string_lossy().into_owned()),
                only_files: selection,
                initial_peers: Some(peers),
                paused,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    settled(&h).await;
    h
}
fn listening_options(persist: Option<PathBuf>) -> SessionOptions {
    let mut opts = options(persist);
    opts.listen = Some(ListenerOptions {
        mode: ListenerMode::TcpOnly,
        listen_addr: (std::net::Ipv4Addr::LOCALHOST, 0).into(),
        ..Default::default()
    });
    opts
}
async fn completed(h: &ManagedTorrentHandle) {
    tokio::time::timeout(Duration::from_secs(20), h.wait_until_completed())
        .await
        .unwrap_or_else(|_| {
            panic!(
                "local swarm timed out in {:?}: {:?}",
                h.output_folder(),
                h.stats()
            )
        })
        .unwrap();
    assert!(h.stats().finished);
}

#[tokio::test(flavor = "multi_thread")]
async fn partfile_boundary_swarm_seeds_restarts_promotes_and_removes() {
    use librqbit::storage::filesystem::partfile_path;
    use std::io::{Read, Seek, SeekFrom};
    let dir = TestDir::new();
    let seed_out = dir.0.join("seed");
    let out = dir.0.join("selected");
    let db = dir.0.join("resume");
    std::fs::create_dir_all(&seed_out).unwrap();
    let files = [("skip-a", 20000), ("wanted", 18000), ("skip-b", 24000)];
    let (bytes, data) = torrent(&files, &[]);
    let mut offset = 0;
    for (name, len) in files {
        std::fs::write(seed_out.join(name), &data[offset..offset + len]).unwrap();
        offset += len;
    }
    let seed = Session::new_with_opts(seed_out.clone(), listening_options(None))
        .await
        .unwrap();
    let sh = local_add(&seed, bytes.clone(), &seed_out, None, vec![], false).await;
    assert!(sh.stats().finished);
    let client = Session::new_with_opts(out.clone(), listening_options(Some(db.clone())))
        .await
        .unwrap();
    let h = local_add(
        &client,
        bytes.clone(),
        &out,
        Some(vec![1]),
        vec![seed.listen_addr().unwrap()],
        false,
    )
    .await;
    completed(&h).await;
    assert!(!out.join("skip-a").exists() && !out.join("skip-b").exists());
    assert_eq!(
        std::fs::read(out.join("wanted")).unwrap(),
        data[20000..38000]
    );
    let parts = partfile_path(&out, h.info_hash().as_string());
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        assert_ne!(
            std::fs::metadata(parts.parent().unwrap())
                .unwrap()
                .file_attributes()
                & 2,
            0
        );
    }
    let mut f = std::fs::File::open(&parts).unwrap();
    for range in [16384..20000, 38000..49152] {
        let mut buf = vec![0; range.len()];
        f.seek(SeekFrom::Start(range.start as u64)).unwrap();
        f.read_exact(&mut buf).unwrap();
        assert_eq!(buf, data[range]);
    }
    drop(f);
    // The original seed is absent from this peer list: boundary bytes must upload from parts.
    let second_out = dir.0.join("second");
    let second = Session::new_with_opts(second_out.clone(), options(None))
        .await
        .unwrap();
    let h2 = local_add(
        &second,
        bytes.clone(),
        &second_out,
        Some(vec![1]),
        vec![client.listen_addr().unwrap()],
        false,
    )
    .await;
    completed(&h2).await;
    assert_eq!(
        std::fs::read(second_out.join("wanted")).unwrap(),
        data[20000..38000]
    );
    assert!(!second_out.join("skip-a").exists() && !second_out.join("skip-b").exists());
    second.delete(h2.info_hash().into(), true).await.unwrap();
    assert!(!partfile_path(&second_out, h2.info_hash().as_string()).exists());
    assert!(!second_out.join("wanted").exists());
    second.stop().await;
    client.pause(&h).await.unwrap();
    client.stop().await;
    drop(h);
    drop(client);
    // Paused restart retains selected boundary pieces and their part-file.
    let client = Session::new_with_opts(out.clone(), listening_options(Some(db)))
        .await
        .unwrap();
    let hash = sh.info_hash();
    let h = client.get(hash.into()).unwrap();
    settled(&h).await;
    assert!(h.is_paused() && h.stats().finished);
    assert!(!out.join("skip-a").exists() && !out.join("skip-b").exists());
    client
        .update_only_files(&h, &std::collections::HashSet::from([0, 1, 2]))
        .await
        .unwrap();
    assert_eq!(
        &std::fs::read(out.join("skip-a")).unwrap()[16384..],
        &data[16384..20000]
    );
    assert_eq!(
        &std::fs::read(out.join("skip-b")).unwrap()[..11152],
        &data[38000..49152]
    );
    // Initial peers are deliberately not persisted; reconnect the fixture explicitly.
    client.detach_for_move(&h).await.unwrap();
    let h = local_add(
        &client,
        bytes,
        &out,
        Some(vec![0, 1, 2]),
        vec![seed.listen_addr().unwrap()],
        false,
    )
    .await;
    completed(&h).await;
    assert_eq!(std::fs::read(out.join("skip-a")).unwrap(), data[..20000]);
    assert_eq!(std::fs::read(out.join("skip-b")).unwrap(), data[38000..]);
    // Live selection changes preserve completed bytes, including read-only file handles.
    client
        .update_only_files(&h, &std::collections::HashSet::from([1]))
        .await
        .unwrap();
    assert!(!out.join("skip-a").exists() && !out.join("skip-b").exists());
    client
        .update_only_files(&h, &std::collections::HashSet::from([0, 1, 2]))
        .await
        .unwrap();
    assert_eq!(std::fs::read(out.join("skip-a")).unwrap(), data[..20000]);
    assert_eq!(std::fs::read(out.join("skip-b")).unwrap(), data[38000..]);
    client.delete(hash.into(), false).await.unwrap();
    assert!(!parts.exists());
    assert_eq!(
        std::fs::read(out.join("wanted")).unwrap(),
        data[20000..38000]
    );
    client.stop().await;
    seed.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn partfile_storage_vectored_take_recheck_move_and_shared_folder() {
    use librqbit::storage::filesystem::{partfile_path, FilesystemStorageFactory};
    use std::io::IoSlice;
    let dir = TestDir::new();
    let out = dir.0.join("data");
    std::fs::create_dir_all(&out).unwrap();
    let (bytes, data) = torrent(&[("skip", 8000), ("wanted", 8384)], &[]);
    std::fs::write(out.join("skip"), &data[..8000]).unwrap();
    std::fs::write(out.join("wanted"), &data[8000..]).unwrap();
    let s = Session::new_with_opts(out.clone(), options(None))
        .await
        .unwrap();
    let h = local_add(&s, bytes.clone(), &out, Some(vec![1]), vec![], true).await;
    assert!(h.stats().finished);
    assert!(
        !out.join("skip").exists(),
        "preexisting unwanted bytes were not migrated"
    );
    let parts = partfile_path(&out, h.info_hash().as_string());
    let storage = h
        .with_metadata(|m| {
            FilesystemStorageFactory::default().create_and_init_selected(h.shared(), m, Some(&[1]))
        })
        .unwrap()
        .unwrap();
    storage.ensure_file_length(0, 8000).unwrap();
    assert!(!out.join("skip").exists());
    assert_eq!(
        storage
            .pwrite_all_vectored(
                0,
                12,
                [IoSlice::new(&data[12..19]), IoSlice::new(&data[19..33])]
            )
            .unwrap(),
        21
    );
    storage.on_file_completed(0).unwrap();
    // Failed promotion must keep the retained bytes readable and allow a later retry.
    std::fs::create_dir(out.join("skip")).unwrap();
    assert!(storage.set_file_selection(None).is_err());
    let mut retained = vec![0; 8000];
    storage.pread_exact(0, 0, &mut retained).unwrap();
    assert_eq!(retained, data[..8000]);
    std::fs::remove_dir(out.join("skip")).unwrap();
    let taken = storage.take().unwrap();
    assert!(storage.pread_exact(0, 0, &mut [0; 8]).is_err());
    let mut buf = vec![0; 8000];
    taken.pread_exact(0, 0, &mut buf).unwrap();
    assert_eq!(buf, data[..8000]);
    drop(taken);
    drop(storage);
    // A different torrent in the same output has its own sparse file.
    let (other, _) = torrent(&[("other-skip", 9000), ("other-wanted", 7384)], &[]);
    let other = local_add(&s, other, &out, Some(vec![1]), vec![], true).await;
    let other_parts = partfile_path(&out, other.info_hash().as_string());
    assert!(parts.exists() && other_parts.exists() && parts != other_parts);
    let (reserved, _) = torrent(&[(".fluxdm.parts", 16384)], &[]);
    assert!(s
        .add_torrent(
            AddTorrent::from_bytes(reserved),
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(out.to_string_lossy().into_owned()),
                ..Default::default()
            })
        )
        .await
        .is_err());
    assert!(
        parts.exists(),
        "a reserved torrent filename replaced retained data"
    );
    s.delete(other.info_hash().into(), true).await.unwrap();
    assert!(parts.exists() && !other_parts.exists());
    // Explicit recheck clears resume bits, retaining the boundary bytes.
    let hash = h.info_hash();
    s.detach(hash.into()).await.unwrap();
    let h = local_add(&s, bytes.clone(), &out, Some(vec![1]), vec![], true).await;
    assert!(h.stats().finished);
    // Location changes carry parts along with the real files.
    s.detach_for_move(&h).await.unwrap();
    let moved = dir.0.join("moved");
    std::fs::rename(&out, &moved).unwrap();
    let h = local_add(&s, bytes, &moved, Some(vec![1]), vec![], true).await;
    assert!(h.stats().finished && !moved.join("skip").exists());
    s.delete(hash.into(), false).await.unwrap();
    assert!(!partfile_path(&moved, hash.as_string()).exists());
    assert!(moved.join("wanted").exists());
    s.stop().await;
}

#[tokio::test]
async fn partfile_single_file_move_and_unloaded_removal_keep_outputs() {
    use librqbit::storage::filesystem::partfile_path;
    let dir = TestDir::new();
    let out = dir.0.join("old");
    let dest = dir.0.join("new");
    let bytes =
        b"d4:infod6:lengthi3e4:name4:file12:piece lengthi16384e6:pieces20:abcdefghijklmnopqrstee";
    let meta = parse(bytes).unwrap();
    assert!(!meta.multi);
    let cached = dir.0.join("test.torrent");
    std::fs::write(&cached, bytes).unwrap();
    let parts = partfile_path(&out, &meta.hash);
    std::fs::create_dir_all(parts.parent().unwrap()).unwrap();
    std::fs::write(&parts, b"retained").unwrap();
    std::fs::write(out.join("file"), b"abc").unwrap();
    let other = parts.parent().unwrap().join("unrelated");
    std::fs::write(&other, b"other").unwrap();
    let mut item = Item {
        torrent_file: cached.to_string_lossy().into_owned(),
        save_folder: out.to_string_lossy().into_owned(),
        file_name: "file".into(),
        is_torrent: true,
        ..Default::default()
    };
    let targets = move_targets(&item, dest.to_str().unwrap());
    assert_eq!(targets.len(), 2);
    move_paths(&targets).unwrap();
    let moved_parts = partfile_path(&dest, &meta.hash);
    assert_eq!(std::fs::read(&moved_parts).unwrap(), b"retained");
    assert!(!parts.exists());
    assert_eq!(std::fs::read(&other).unwrap(), b"other");
    item.save_folder = dest.to_string_lossy().into_owned();
    let mgr = Manager::new();
    Service::default().remove(&mgr, &item, false).await;
    assert!(!moved_parts.exists());
    assert_eq!(std::fs::read(dest.join("file")).unwrap(), b"abc");
    assert_eq!(std::fs::read(other).unwrap(), b"other");
}

#[tokio::test(flavor = "multi_thread")]
async fn rapid_check_pause_resume_twenty_times_settles_and_stops_reading() {
    let dir = TestDir::new();
    let (bytes, data) = torrent(&[("file", 1024 * 1024)], &[]);
    let reads = Arc::new(Mutex::new(Vec::new()));
    let spy = ReadSpy {
        data: Arc::new(data),
        offsets: Arc::new(vec![0]),
        reads: reads.clone(),
        delay: true,
        finish_gate: None,
        read_gate: None,
    };
    let s = Session::new_with_opts(dir.0.clone(), options(None))
        .await
        .unwrap();
    let h = s
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                storage_factory: Some(spy.boxed()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    for _ in 0..20 {
        s.pause(&h).await.unwrap();
        assert!(h.is_paused());
        let n = reads.lock().len();
        tokio::time::sleep(Duration::from_millis(3)).await;
        assert_eq!(reads.lock().len(), n);
        s.unpause(&h).await.unwrap();
    }
    settled(&h).await;
    assert!(matches!(h.stats().state, TorrentStatsState::Live));
    assert!(h.stats().finished);
    s.pause(&h).await.unwrap();
    assert!(matches!(h.stats().state, TorrentStatsState::Paused));
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn manager_torrent_operations_and_rapid_clicks_leave_one_paused_runner() {
    let dir = TestDir::new();
    let (bytes, _) = torrent(&[("file", 1024 * 1024)], &[]);
    let meta = parse(&bytes).unwrap();
    let file = dir.0.join("info.torrent");
    std::fs::write(&file, bytes).unwrap();
    let mgr = Manager::new();
    let s = Session::new_with_opts(dir.0.clone(), options(None))
        .await
        .unwrap();
    *mgr.torrents.session.lock().await = Some(s.clone());
    *mgr.torrents.live.lock() = Some(s.clone());
    let it = Item {
        is_torrent: true,
        torrent_file: file.to_string_lossy().into_owned(),
        info_hash: meta.hash,
        save_folder: dir.0.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let id = it.id.clone();
    mgr.st.lock().items.push(it);
    let gate = mgr.torrents.operation(&id).lock_owned().await;
    mgr.start(&id);
    assert!(!mgr.is_running(&id));
    assert!(mgr.item(&id).unwrap().note.contains("still finishing"));
    let m = mgr.clone();
    let paused_id = id.clone();
    let pausing = tokio::spawn(async move { m.pause(&paused_id).await });
    tokio::task::yield_now().await;
    assert!(!pausing.is_finished(), "pause bypassed the operation guard");
    drop(gate);
    pausing.await.unwrap();
    for _ in 0..20 {
        mgr.start(&id);
        let m = mgr.clone();
        let paused_id = id.clone();
        let pausing = tokio::spawn(async move { m.pause(&paused_id).await });
        mgr.start(&id);
        pausing.await.unwrap();
    }
    mgr.pause(&id).await;
    assert!(!mgr.is_running(&id));
    assert_eq!(mgr.item(&id).unwrap().status, DlStatus::Paused);
    if let Some(h) = mgr.torrents.find(&mgr.item(&id).unwrap().info_hash) {
        assert!(h.is_paused());
    }
    mgr.exiting.store(true, Ordering::Relaxed);
    mgr.start(&id);
    assert!(!mgr.is_running(&id));
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_wanted_files_pause_before_storage_can_recreate_them() {
    let dir = TestDir::new();
    let (bytes, _) = torrent(&[("wanted", 32768), ("skipped", 16384)], &[]);
    let meta = parse(&bytes).unwrap();
    let file = dir.0.join("info.torrent");
    std::fs::write(&file, bytes).unwrap();
    let mgr = Manager::new();
    let it = Item {
        is_torrent: true,
        torrent_file: file.to_string_lossy().into_owned(),
        info_hash: meta.hash.clone(),
        save_folder: dir.0.to_string_lossy().into_owned(),
        downloaded: 16384,
        skipped_files: vec![1],
        ..Default::default()
    };
    let id = it.id.clone();
    mgr.st.lock().items.push(it.clone());
    let run = Run::test_run(&id);
    super::run(mgr.clone(), run).await;
    let paused = mgr.item(&id).unwrap();
    assert_eq!(paused.status, DlStatus::Paused);
    assert_eq!(paused.downloaded, 16384);
    assert_eq!(paused.error_message, FILES_MISSING);
    assert!(paused.status_text().contains(FILES_MISSING));
    assert!(!dir.0.join("test/wanted").exists());
    std::fs::create_dir_all(dir.0.join("test")).unwrap();
    std::fs::write(dir.0.join("test/wanted"), vec![0; 32768]).unwrap();
    assert!(
        !wanted_files_missing(&it, &meta),
        "unselected file must not prevent resume"
    );
    std::fs::write(dir.0.join("test/wanted"), [0]).unwrap();
    assert!(
        wanted_files_missing(&it, &meta),
        "truncated wanted data must prevent resume"
    );
    let checking = Item {
        is_torrent: true,
        status: DlStatus::Verifying,
        verify_progress: 37.0,
        ..Default::default()
    };
    assert_eq!(checking.status_text(), "Checking 37%");
}

#[tokio::test(flavor = "multi_thread")]
async fn deferred_restore_keeps_missing_files_absent_and_reuses_resume_bits() {
    let dir = TestDir::new();
    let state = dir.0.join("state");
    let (bytes, data) = torrent(&[("wanted", 2 * 1024 * 1024)], &[]);
    std::fs::create_dir_all(dir.0.join("test")).unwrap();
    std::fs::write(dir.0.join("test/wanted"), &data).unwrap();
    let s = Session::new_with_opts(dir.0.clone(), options(Some(state.clone())))
        .await
        .unwrap();
    let h = local_add(&s, bytes.clone(), &dir.0.join("test"), None, vec![], true).await;
    settled(&h).await;
    s.pause(&h).await.unwrap();
    s.stop().await;
    drop(h);
    drop(s);
    std::fs::remove_file(dir.0.join("test/wanted")).unwrap();
    let mut opts = options(Some(state.clone()));
    opts.defer_persistence_restore = true;
    let s = Session::new_with_opts(dir.0.clone(), opts).await.unwrap();
    assert_eq!(s.with_torrents(|t| t.count()), 0);
    assert!(!dir.0.join("test/wanted").exists());
    std::fs::write(dir.0.join("test/wanted"), &data).unwrap();
    let h = s
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                paused: true,
                overwrite: true,
                output_folder: Some(dir.0.join("test").to_string_lossy().into_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    let init = h.with_state(|st| match st {
        ManagedTorrentState::Initializing(i) => Some(i.clone()),
        _ => None,
    });
    settled(&h).await;
    if let Some(init) = init {
        assert!(init.get_checked_bytes() < data.len() as u64);
    }
    assert!(h.stats().finished);
    s.pause(&h).await.unwrap();
    let db: Value =
        serde_json::from_slice(&std::fs::read(state.join("session.json")).unwrap()).unwrap();
    assert_eq!(db["torrents"].as_object().unwrap().len(), 1);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_recheck_and_exit_do_not_start_or_recreate_missing_files() {
    let mgr = Manager::new();
    let it = Item {
        is_torrent: true,
        status: DlStatus::Paused,
        ..Default::default()
    };
    let id = it.id.clone();
    mgr.st.lock().items.push(it);
    mgr.torrents.begin_check(&id);
    mgr.torrents.cancel_check(&id);
    assert_eq!(
        mgr.torrents.recheck(&mgr, &id).await.unwrap_err().kind,
        ErrKind::Cancelled
    );
    assert!(mgr.torrents.checks.lock().is_empty());
    mgr.exiting.store(true, Ordering::Relaxed);
    assert_eq!(
        mgr.torrents.recheck(&mgr, &id).await.unwrap_err().kind,
        ErrKind::Cancelled
    );
    assert!(mgr.torrents.session_now().is_none());
    assert_eq!(mgr.item(&id).unwrap().status, DlStatus::Paused);
}

#[tokio::test(flavor = "multi_thread")]
async fn pausing_magnet_resolution_returns_paused_without_stale_connecting_text() {
    let dir = TestDir::new();
    let mgr = Manager::new();
    let s = Session::new_with_opts(dir.0.clone(), options(None))
        .await
        .unwrap();
    *mgr.torrents.session.lock().await = Some(s.clone());
    *mgr.torrents.live.lock() = Some(s.clone());
    let it = Item {
        is_torrent: true,
        url: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
        save_folder: dir.0.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let id = it.id.clone();
    mgr.st.lock().items.push(it);
    let run = Run::test_run(&id);
    let m = mgr.clone();
    let r = run.clone();
    let task = tokio::spawn(async move { super::run(m, r).await });
    tokio::task::yield_now().await;
    run.cancel.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    let it = mgr.item(&id).unwrap();
    assert_eq!(it.status, DlStatus::Paused);
    assert!(it.error_message.is_empty());
    assert_eq!(it.status_text(), "Paused");
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn pause_interrupts_a_check_waiting_for_the_initialization_slot() {
    use std::sync::atomic::AtomicBool;
    let dir = TestDir::new();
    let (bytes, data) = torrent(&[("file", 16384)], &[]);
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let spy = ReadSpy {
        data: Arc::new(data),
        offsets: Arc::new(vec![0]),
        reads: Default::default(),
        delay: false,
        finish_gate: Some((entered.clone(), release.clone())),
        read_gate: None,
    };
    let mut opts = options(None);
    opts.concurrent_init_limit = Some(1);
    let s = Session::new_with_opts(dir.0.clone(), opts).await.unwrap();
    let first = s
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                storage_factory: Some(spy.boxed()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (bytes, _) = torrent(&[("another", 32768)], &[]);
    let second = s
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                overwrite: true,
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(2), s.pause(&second)).await;
    release.store(true, Ordering::Release);
    result
        .expect("pause waited for another torrent's check")
        .unwrap();
    assert!(second.is_paused());
    settled(&first).await;
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn exit_during_active_check_stops_hashing_and_keeps_paused_intent() {
    let dir = TestDir::new();
    let (bytes, data) = torrent(&[("file", 4 * 1024 * 1024)], &[]);
    let reads = Arc::new(Mutex::new(Vec::new()));
    let spy = ReadSpy {
        data: Arc::new(data),
        offsets: Arc::new(vec![0]),
        reads: reads.clone(),
        delay: true,
        finish_gate: None,
        read_gate: None,
    };
    let s = Session::new_with_opts(dir.0.clone(), options(None))
        .await
        .unwrap();
    let h = s
        .add_torrent(
            AddTorrent::from_bytes(bytes),
            Some(AddTorrentOptions {
                storage_factory: Some(spy.boxed()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while reads.lock().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let svc = Service::default();
    *svc.session.lock().await = Some(s.clone());
    *svc.live.lock() = Some(s.clone());
    tokio::time::timeout(Duration::from_secs(2), svc.shutdown())
        .await
        .expect("exit waited for the full disk check");
    assert!(h.is_paused());
    let count = reads.lock().len();
    assert!(
        count < 256,
        "exit finished the full check instead of interrupting it"
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(
        reads.lock().len(),
        count,
        "hashing continued after shutdown"
    );
    assert!(!matches!(h.stats().state, TorrentStatsState::Live));
}

#[tokio::test(flavor = "multi_thread")]
async fn swarm_pause_drains_upload_callbacks_before_storage_handoff() {
    use std::sync::atomic::AtomicBool;
    let dir = TestDir::new();
    let (bytes, data) = torrent(&[("file", 16384)], &[]);
    let armed = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let spy = ReadSpy {
        data: Arc::new(data),
        offsets: Arc::new(vec![0]),
        reads: Default::default(),
        delay: false,
        finish_gate: None,
        read_gate: Some((armed.clone(), entered.clone(), release.clone())),
    };
    let seed = Session::new_with_opts(dir.0.join("seed"), listening_options(None))
        .await
        .unwrap();
    let sh = seed
        .add_torrent(
            AddTorrent::from_bytes(bytes.clone()),
            Some(AddTorrentOptions {
                storage_factory: Some(spy.boxed()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    settled(&sh).await;
    armed.store(true, Ordering::Release);
    let client = Session::new_with_opts(dir.0.join("leech"), options(None))
        .await
        .unwrap();
    let h = local_add(
        &client,
        bytes,
        &dir.0.join("leech"),
        None,
        vec![seed.listen_addr().unwrap()],
        false,
    )
    .await;
    let blocked = tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    if blocked.is_err() {
        release.store(true, Ordering::Release);
    }
    blocked.expect("local swarm never entered the upload read");
    let s = seed.clone();
    let paused = sh.clone();
    let pausing = tokio::spawn(async move { s.pause(&paused).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    let returned_early = pausing.is_finished();
    release.store(true, Ordering::Release);
    tokio::time::timeout(Duration::from_secs(3), pausing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        !returned_early,
        "pause returned before the upload callback finished"
    );
    assert!(sh.is_paused());
    client.pause(&h).await.unwrap();
    client.stop().await;
    seed.stop().await;
}


#[tokio::test(flavor = "multi_thread")]
async fn windows_torrent_paths_match_engine_storage_and_stay_in_save_folder() {
    let dir = TestDir::new();
    let (bytes, _) = torrent(&[("a:b.txt", 1), ("C:x", 1), ("CON.txt", 1), ("name. ", 1), ("x?y", 1)], &[]);
    let meta = parse(&bytes).unwrap();
    let expected = ["a_b.txt", "C_x", "_CON.txt", "name", "x_y"];
    assert_eq!(meta.files.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), expected);
    let s = Session::new_with_opts(dir.0.clone(), options(None)).await.unwrap();
    let h = s.add_torrent(AddTorrent::from_bytes(bytes), Some(AddTorrentOptions { paused: true,
        output_folder: Some(dir.0.to_string_lossy().into_owned()), ..Default::default() })).await.unwrap().into_handle().unwrap();
    settled(&h).await;
    for (i, file) in h.metadata.load().as_ref().unwrap().file_infos.iter().enumerate() {
        assert_eq!(file.relative_filename.to_str().unwrap(), expected[i]);
        let path = dir.0.join(&file.relative_filename);
        assert!(path.starts_with(&dir.0)); assert!(path.is_file(), "{}", expected[i]);
    }
    s.stop().await;
    for name in ["NUL", "aux.txt", "COM1.zip", "LPT9", "..", ".", " "] {
        let safe = librqbit::safe_path::component(name);
        assert!(!safe.is_empty()); assert!(!safe.ends_with(['.', ' '])); assert_ne!(safe, name);
    }
    let long = librqbit::safe_path::component(&"\u{1f600}".repeat(200));
    assert!(long.encode_utf16().count() <= 255);
    assert!(librqbit::safe_path::component("<>:\"/\\|?*\0").chars().all(|c| c == '_'));
}

#[tokio::test(flavor = "multi_thread")]
async fn sanitized_aliases_get_numbered_names_in_fluxdm_and_the_engine() {
    let dir = TestDir::new();
    let (bytes, _) = torrent(&[("a:b", 1), ("a?b", 1)], &[]);
    let meta = parse(&bytes).unwrap();
    let names: Vec<String> = meta.files.iter().map(|(p, _)| Path::new(p).file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["a_b", "a_b (2)"]);
    let s = Session::new_with_opts(dir.0.clone(), options(None)).await.unwrap();
    let h = s.add_torrent(AddTorrent::from_bytes(bytes), Some(AddTorrentOptions { paused: true, ..Default::default() })).await.unwrap().into_handle().unwrap();
    let engine: Vec<String> = h.with_metadata(|m| m.file_infos.iter().map(|f| f.relative_filename.file_name().unwrap().to_string_lossy().into_owned()).collect()).unwrap();
    assert_eq!(engine, names);
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn recheck_slot_wait_can_be_cancelled_and_check_timeout_releases_slot() {
    let mgr = Manager::new();
    let it = Item { is_torrent: true, ..Default::default() }; let id = it.id.clone();
    mgr.st.lock().items = vec![it];
    let gate = mgr.torrents.recheck_gate.lock().await;
    mgr.torrents.begin_check(&id);
    let task_mgr = mgr.clone(); let task_id = id.clone();
    let task = tokio::spawn(async move { task_mgr.torrents.recheck(&task_mgr, &task_id).await });
    tokio::task::yield_now().await;
    assert!(!task.is_finished()); mgr.torrents.cancel_check(&id);
    assert_eq!(tokio::time::timeout(Duration::from_secs(1), task).await.unwrap().unwrap().unwrap_err().kind, ErrKind::Cancelled);
    drop(gate);
    let dir = TestDir::new(); let (bytes, _) = torrent(&[("file", 1)], &[]);
    let path = dir.0.join("test.torrent"); std::fs::write(&path, bytes).unwrap();
    mgr.with_item(&id, |i| i.torrent_file = path.to_string_lossy().into_owned());
    // Hold session initialization so the check reaches its timeout without touching storage.
    let session_gate = mgr.torrents.session.lock().await;
    let result = mgr.torrents.recheck_with_timeout(&mgr, &id, Duration::from_millis(20)).await;
    assert_eq!(result.unwrap_err().kind, ErrKind::Timeout);
    assert!(mgr.torrents.recheck_gate.try_lock().is_ok());
    assert!(mgr.torrents.checks.lock().is_empty());
    drop(session_gate);
}

#[test]
fn torrent_disk_full_classification_preserves_anyhow_context() {
    for code in [39, 112] {
        let e = anyhow::Error::new(std::io::Error::from_raw_os_error(code)).context("writing piece").context("storage failed");
        let e = anyhow_err(e); assert_eq!(e.kind, ErrKind::DiskFull); assert!(!e.retryable());
    }
    assert_ne!(anyhow_err(anyhow::anyhow!("remote disk is full")).kind, ErrKind::DiskFull);
}


#[tokio::test(flavor = "multi_thread")]
async fn local_swarm_disk_full_write_reaches_typed_engine_error() {
    let dir = TestDir::new(); let out = dir.0.join("seed"); std::fs::create_dir_all(&out).unwrap();
    let (bytes, data) = torrent(&[("file", 16384)], &[]); std::fs::write(out.join("file"), &data).unwrap();
    let mut opts = options(None); opts.listen = Some(ListenerOptions {
        mode: ListenerMode::TcpOnly, listen_addr: (std::net::Ipv4Addr::LOCALHOST, 0).into(), ..Default::default() });
    let seed = Session::new_with_opts(out.clone(), opts).await.unwrap();
    let sh = seed.add_torrent(AddTorrent::from_bytes(bytes.clone()), Some(AddTorrentOptions {
        overwrite: true, output_folder: Some(out.to_string_lossy().into_owned()), ..Default::default()
    })).await.unwrap().into_handle().unwrap(); settled(&sh).await;
    let client = Session::new_with_opts(dir.0.join("client"), options(None)).await.unwrap();
    let spy = ReadSpy { data: Arc::new(vec![0; data.len()]), offsets: Arc::new(vec![0]),
        reads: Arc::new(Mutex::new(Vec::new())), delay: false, read_gate: None, finish_gate: None };
    let h = client.add_torrent(AddTorrent::from_bytes(bytes), Some(AddTorrentOptions {
        storage_factory: Some(spy.boxed()), initial_peers: Some(vec![seed.listen_addr().unwrap()]), ..Default::default()
    })).await.unwrap().into_handle().unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !matches!(h.stats().state, TorrentStatsState::Error) { tokio::time::sleep(Duration::from_millis(10)).await; }
    }).await.expect("write error did not reach engine state");
    assert!(h.storage_full(), "{:?}", h.stats().error);
    assert_eq!(h.stats().progress_bytes, 0);
    client.stop().await; seed.stop().await;
}


#[tokio::test(flavor = "multi_thread")]
async fn sanitized_paths_match_missing_file_details_move_and_delete_operations() {
    let dir = TestDir::new(); let original = dir.0.join("original"); let moved = dir.0.join("moved");
    let (bytes, _) = torrent(&[("a:b.txt", 1), ("C:x", 1), ("CON.txt", 1), ("name. ", 1), ("x?y", 1)], &[]);
    let meta = parse(&bytes).unwrap(); let cached = dir.0.join("info.torrent"); std::fs::write(&cached, bytes).unwrap();
    let mut it = Item { is_torrent: true, torrent_file: cached.to_string_lossy().into_owned(),
        save_folder: original.to_string_lossy().into_owned(), file_name: meta.name.clone(), ..Default::default() };
    let out = PathBuf::from(output_folder(&it, &meta)); std::fs::create_dir_all(&out).unwrap();
    for (p, _) in &meta.files { std::fs::write(out.join(p), [7]).unwrap(); }
    assert!(!wanted_files_missing(&it, &meta));
    let mgr = Manager::new(); mgr.st.lock().items = vec![it.clone()];
    let details = mgr.torrents.details(&it);
    let text = details.to_string(); assert!(text.contains("a_b.txt")); assert!(!text.contains("a:b.txt"));
    mgr.torrents.remove(&mgr, &it, false).await;
    assert!(out.join("_CON.txt").exists());
    mgr.torrents.move_data(&mgr, &it.id, &moved.to_string_lossy()).await.unwrap();
    it.save_folder = moved.to_string_lossy().into_owned();
    assert!(!wanted_files_missing(&it, &meta)); assert!(!out.exists());
    let moved_out = PathBuf::from(output_folder(&it, &meta));
    std::fs::write(moved_out.join("unrelated"), b"keep").unwrap();
    mgr.torrents.remove(&mgr, &it, true).await;
    for (p, _) in &meta.files { assert!(!moved_out.join(p).exists()); }
    assert!(moved_out.join("unrelated").exists());
}


#[tokio::test(flavor = "multi_thread")]
async fn timed_out_recheck_keeps_global_slot_until_blocked_hash_read_drains() {
    use std::sync::atomic::AtomicBool;
    let dir = TestDir::new(); let (bytes, data) = torrent(&[("file", 16384)], &[]);
    let meta = parse(&bytes).unwrap(); let cached = dir.0.join("info.torrent"); std::fs::write(&cached, bytes).unwrap();
    let armed = Arc::new(AtomicBool::new(true)); let entered = Arc::new(AtomicBool::new(false)); let release = Arc::new(AtomicBool::new(false));
    let spy = ReadSpy { data: Arc::new(data), offsets: Arc::new(vec![0]), reads: Arc::new(Mutex::new(Vec::new())),
        delay: false, finish_gate: None, read_gate: Some((armed, entered.clone(), release.clone())) };
    let mut opts = options(None); opts.default_storage_factory = Some(spy.boxed());
    let session = Session::new_with_opts(dir.0.clone(), opts).await.unwrap();
    let mgr = Manager::new();
    *mgr.torrents.session.lock().await = Some(session.clone()); *mgr.torrents.live.lock() = Some(session.clone());
    let it = Item { is_torrent: true, info_hash: meta.hash, torrent_file: cached.to_string_lossy().into_owned(),
        save_folder: dir.0.to_string_lossy().into_owned(), ..Default::default() }; let id = it.id.clone();
    mgr.st.lock().items = vec![it];
    let task_mgr = mgr.clone(); let task_id = id.clone();
    let task = tokio::spawn(async move { task_mgr.torrents.recheck_with_timeout(&task_mgr, &task_id, Duration::from_secs(2)).await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !entered.load(Ordering::Acquire) { tokio::time::sleep(Duration::from_millis(5)).await; }
    }).await.expect("recheck never entered gated read");
    let result = tokio::time::timeout(Duration::from_secs(5), task).await;
    // Always release the blocking read, even if timeout handling regresses.
    let slot_held = mgr.torrents.recheck_gate.try_lock().is_err(); release.store(true, Ordering::Release);
    assert_eq!(result.unwrap().unwrap().unwrap_err().kind, ErrKind::Timeout); assert!(slot_held);
    let guard = tokio::time::timeout(Duration::from_secs(5), mgr.torrents.recheck_gate.lock()).await.unwrap(); drop(guard);
    assert!(mgr.torrents.find(&mgr.item(&id).unwrap().info_hash).unwrap().is_paused());
    session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn staged_torrent_completion_moves_selected_data_and_preserves_sharing() {
    let dir = TestDir::new();
    let stage = dir.0.join("unfinished"); let destination = dir.0.join("finished");
    std::fs::create_dir_all(stage.join("test")).unwrap();
    let (bytes, data) = torrent(&[("wanted", 20000), ("skipped", 10000)], &[]);
    let source = dir.0.join("source.torrent"); std::fs::write(&source, &bytes).unwrap();
    std::fs::write(stage.join("test/wanted"), &data[..20000]).unwrap();
    std::fs::write(stage.join("test/skipped"), &data[20000..]).unwrap();
    let mgr = Manager::new();
    let it = Item { is_torrent: true, url: source.to_string_lossy().into_owned(), torrent_file: source.to_string_lossy().into_owned(),
        save_folder: destination.to_string_lossy().into_owned(), incomplete_folder: stage.to_string_lossy().into_owned(),
        skipped_files: vec![1], file_name: "test".into(), ..Default::default() };
    let it: Item = serde_json::from_value(serde_json::to_value(&it).unwrap()).unwrap();
    assert_eq!(Path::new(it.storage_folder()), stage);
    mgr.st.lock().items.push(it.clone());
    let s = Session::new_with_opts(stage.clone(), options(None)).await.unwrap();
    *mgr.torrents.session.lock().await = Some(s.clone()); *mgr.torrents.live.lock() = Some(s.clone());
    let run = Run::test_run(&it.id); let mut handle = None;
    go(&mgr, &run, &mut handle).await.unwrap();
    let completed = mgr.item(&it.id).unwrap(); assert_eq!(completed.status, DlStatus::Completed);
    assert!(completed.incomplete_folder.is_empty()); assert_eq!(completed.note, "seeding");
    assert_eq!(std::fs::read(destination.join("test/wanted")).unwrap(), &data[..20000]);
    assert!(!destination.join("test/skipped").exists()); assert!(!stage.join("test/wanted").exists());
    let h = handle.unwrap(); settled(&h).await; assert!(h.stats().finished);
    assert_eq!(PathBuf::from(completed.full_path()), destination.join("test"));
    s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn socks5_peer_transfer_decodes_credentials_and_local_bypass_connects_directly() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = TestDir::new(); let seed_out = dir.0.join("seed"); std::fs::create_dir_all(&seed_out).unwrap();
    let (bytes, data) = torrent(&[("file", 32768)], &[]); std::fs::write(seed_out.join("file"), &data).unwrap();
    let mut opts = options(None); opts.listen = Some(ListenerOptions { mode: ListenerMode::TcpOnly, listen_addr: (std::net::Ipv4Addr::LOCALHOST, 0).into(), ..Default::default() });
    let seed = Session::new_with_opts(seed_out, opts).await.unwrap();
    let sh = seed.add_torrent(AddTorrent::from_bytes(bytes.clone()), Some(AddTorrentOptions { overwrite: true, ..Default::default() })).await.unwrap().into_handle().unwrap();
    settled(&sh).await;
    for bypass in [false, true] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(); let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let proxy = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = [0; 2]; socket.read_exact(&mut head).await.unwrap(); assert_eq!(head[0], 5);
            let mut methods = vec![0; head[1] as usize]; socket.read_exact(&mut methods).await.unwrap(); assert!(methods.contains(&2)); socket.write_all(&[5, 2]).await.unwrap();
            socket.read_exact(&mut head).await.unwrap(); assert_eq!(head[0], 1);
            let mut user = vec![0; head[1] as usize]; socket.read_exact(&mut user).await.unwrap();
            let len = socket.read_u8().await.unwrap(); let mut password = vec![0; len as usize]; socket.read_exact(&mut password).await.unwrap();
            assert_eq!(user, b"name@proxy"); assert_eq!(password, b"pass:/ word"); socket.write_all(&[1, 0]).await.unwrap();
            let mut request = [0; 10]; socket.read_exact(&mut request).await.unwrap(); assert_eq!(&request[..4], &[5, 1, 0, 1]);
            let peer = std::net::SocketAddr::from(([request[4], request[5], request[6], request[7]], u16::from_be_bytes([request[8], request[9]])));
            let mut upstream = tokio::net::TcpStream::connect(peer).await.unwrap();
            socket.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 1]).await.unwrap(); let _ = tx.send(());
            let _ = tokio::io::copy_bidirectional(&mut socket, &mut upstream).await;
        });
        let out = dir.0.join(if bypass { "direct" } else { "proxied" });
        let mut opts = options(None); opts.connect = Some(librqbit::ConnectionOptions { proxy_url: Some(format!("socks5://name%40proxy:pass%3A%2F%20word@127.0.0.1:{port}")), proxy_bypass_local: bypass, ..Default::default() });
        let client = Session::new_with_opts(out.clone(), opts).await.unwrap();
        let h = client.add_torrent(AddTorrent::from_bytes(bytes.clone()), Some(AddTorrentOptions { overwrite: true, initial_peers: Some(vec![seed.listen_addr().unwrap()]), ..Default::default() })).await.unwrap().into_handle().unwrap();
        tokio::time::timeout(Duration::from_secs(15), async { while !h.stats().finished { tokio::time::sleep(Duration::from_millis(10)).await; } }).await.unwrap();
        assert_eq!(std::fs::read(out.join("file")).unwrap(), data);
        if bypass { assert!(tokio::time::timeout(Duration::from_millis(30), rx).await.is_err()); } else { rx.await.unwrap(); }
        client.stop().await; proxy.abort();
    }
    seed.stop().await;
}

#[test]
fn r14c_settings_defaults_and_item_snapshots_roundtrip() {
    let legacy: Settings = serde_json::from_str("{}").unwrap();
    assert_eq!(legacy.torrent_seed_minutes, 0);
    assert_eq!(legacy.torrent_seed_action, "Stop");
    assert!(legacy.torrent_always_trackers.is_empty() && legacy.torrent_skip_patterns.is_empty());
    assert!(!legacy.torrent_mark_unfinished);
    let mgr = Manager::new();
    {
        let mut st = mgr.st.lock();
        st.settings.torrent_always_trackers = "https://tracker.invalid/announce\nhttps://tracker.invalid/announce".into();
        st.settings.torrent_skip_patterns = "*.txt\n*sample*".into();
        st.settings.torrent_mark_unfinished = true;
        st.settings.torrent_seed_minutes = 60;
        st.settings.torrent_seed_action = "Remove".into();
    }
    let item = mgr.create_torrent_item("magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567", None, None, None);
    assert_eq!(item.extra_trackers, vec!["https://tracker.invalid/announce"]);
    assert!(item.torrent_mark_unfinished && item.torrent_skip_patterns.contains("*.txt"));
    let explicit = mgr.create_torrent_item(&item.url, None, None, Some(vec![]));
    assert!(explicit.torrent_skip_patterns.is_empty(), "explicit choices override global defaults");
    let saved: Settings = serde_json::from_value(serde_json::to_value(mgr.settings()).unwrap()).unwrap();
    assert_eq!(saved.torrent_seed_minutes, 60); assert_eq!(saved.torrent_seed_action, "Remove");
    assert_eq!(saved.torrent_always_trackers, mgr.settings().torrent_always_trackers);
    let item: Item = serde_json::from_value(serde_json::to_value(item).unwrap()).unwrap();
    assert!(item.torrent_mark_unfinished && item.torrent_skip_patterns.contains("*.txt"));
}

#[test]
fn r14c_tracker_validation_and_filename_patterns() {
    assert_eq!(tracker_lines("\nhttps://tracker.invalid/a\r\nudp://tracker.invalid:80/announce\nhttps://tracker.invalid/a ").unwrap().len(), 2);
    for bad in ["not a URL", "file:///data", "ftp://tracker.invalid", "https:///", "http://user:secret@tracker.invalid/a"] { assert!(tracker_lines(bad).is_err(), "{bad}"); }
    for name in ["ReadMe.TXT", "dir/readme.txt", "dir\\trailer-SAMPLE.mkv", "link.url", "a1.nfo"] {
        assert!(excluded(name, " *.txt \r\n*.url\n*sample*\na?.nfo\n"), "{name}");
    }
    for name in ["video.mkv", "sampled/video.mp4", "a12.nfo", "text.txt.zip"] { assert!(!excluded(name, "*.txt\n*.url\n*sample*\na?.nfo"), "{name}"); }
    assert!(!excluded("any", "\n\n"));
    assert!(excluded("Arabic-مرحبا.txt", "*.txt"));
}

#[test]
fn r14c_seed_limits_are_either_ratio_or_active_minutes() {
    let mut s = Settings { torrent_seed_ratio: 2.0, torrent_seed_minutes: 5, ..Default::default() };
    assert!(!seed_limit_reached(&s, 1.0, 299.9));
    assert!(seed_limit_reached(&s, 1.0, 300.0));
    assert!(seed_limit_reached(&s, 2.0, 0.0));
    s.torrent_seed_minutes = 0;
    assert!(!seed_limit_reached(&s, 1.0, 1e9));
    s.torrent_seed_ratio = 0.0;
    assert!(seed_limit_reached(&s, 0.0, 0.0));
}

#[tokio::test(flavor = "multi_thread")]
async fn r14c_seed_tick_stops_or_removes_without_deleting_files_and_excludes_pause() {
    for action in ["Stop", "Remove"] {
        let dir = TestDir::new(); let out = dir.0.join("files"); std::fs::create_dir_all(&out).unwrap();
        let (bytes, data) = torrent(&[("file", 16384)], &[]); std::fs::write(out.join("file"), &data).unwrap();
        let cached = dir.0.join("info.torrent"); std::fs::write(&cached, &bytes).unwrap();
        let session = Session::new_with_opts(out.clone(), options(None)).await.unwrap();
        let h = local_add(&session, bytes, &out, None, vec![], false).await;
        let mgr = Manager::new();
        *mgr.torrents.session.lock().await = Some(session.clone()); *mgr.torrents.live.lock() = Some(session.clone());
        let item = Item { is_torrent: true, status: DlStatus::Completed, torrent_file: cached.to_string_lossy().into_owned(), info_hash: h.info_hash().as_string(), save_folder: out.to_string_lossy().into_owned(), downloaded: 16384, total_size: 16384, seed_seconds: 58.0, ..Default::default() };
        let id = item.id.clone();
        { let mut st = mgr.st.lock(); st.items = vec![item]; st.settings.torrent_seed_ratio = 100.0; st.settings.torrent_seed_minutes = 1; st.settings.torrent_seed_action = action.into(); }
        mgr.torrents.seed_clock.lock().insert(id.clone(), Instant::now() - Duration::from_secs(20));
        session.pause(&h).await.unwrap(); mgr.torrents.tick(&mgr).await;
        assert_eq!(mgr.item(&id).unwrap().seed_seconds, 58.0);
        assert!(!mgr.torrents.seed_clock.lock().contains_key(&id));
        session.unpause(&h).await.unwrap();
        mgr.torrents.tick(&mgr).await;
        assert_eq!(mgr.item(&id).unwrap().seed_seconds, 58.0);
        mgr.torrents.seed_clock.lock().insert(id.clone(), Instant::now() - Duration::from_secs(3));
        mgr.torrents.tick(&mgr).await;
        if action == "Stop" {
            assert!(h.is_paused());
            let saved: Item = serde_json::from_value(serde_json::to_value(mgr.item(&id).unwrap()).unwrap()).unwrap();
            assert!(saved.seed_seconds >= 61.0); assert!(!saved.seeding);
        } else { assert!(mgr.item(&id).is_none()); assert!(mgr.torrents.find(&h.info_hash().as_string()).is_none()); }
        assert_eq!(std::fs::read(out.join("file")).unwrap(), data);
        session.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn r14c_marked_files_publish_individually_survive_restart_selection_and_move() {
    use librqbit::storage::filesystem::{FilesystemStorageFactory, marked_path};
    let dir = TestDir::new(); let out = dir.0.join("files");
    let (bytes, data) = torrent(&[("first.txt", 16384), ("second.bin", 16384), ("skip.url", 8000)], &[]);
    let session = Session::new_with_opts(out.clone(), options(None)).await.unwrap();
    let factory = FilesystemStorageFactory { mark_unfinished: true };
    let h = session.add_torrent(AddTorrent::from_bytes(bytes.clone()), Some(AddTorrentOptions { output_folder: Some(out.to_string_lossy().into_owned()), overwrite: true, paused: true, only_files: Some(vec![0,1]), storage_factory: Some(factory.boxed()), ..Default::default() })).await.unwrap().into_handle().unwrap();
    settled(&h).await;
    assert!(marked_path(&out.join("first.txt")).exists()); assert!(!out.join("first.txt").exists());
    assert!(!marked_path(&out.join("skip.url")).exists());
    let storage = h.with_metadata(|m| factory.create_and_init_selected(h.shared(), m, Some(&[0,1]))).unwrap().unwrap();
    storage.pwrite_all(0, 0, &data[..16384]).unwrap(); storage.on_file_completed(0).unwrap();
    assert_eq!(std::fs::read(out.join("first.txt")).unwrap(), &data[..16384]);
    assert!(!marked_path(&out.join("first.txt")).exists());
    assert!(marked_path(&out.join("second.bin")).exists());
    // An unexpected destination must never be overwritten; retry publication after resolving it.
    storage.pwrite_all(1, 0, &data[16384..32768]).unwrap();
    std::fs::write(out.join("second.bin"), b"unrelated").unwrap();
    assert!(storage.on_file_completed(1).is_err());
    assert_eq!(std::fs::read(out.join("second.bin")).unwrap(), b"unrelated");
    std::fs::remove_file(out.join("second.bin")).unwrap();
    let storage = storage.take().unwrap();
    storage.on_file_completed(1).unwrap();
    assert!(!marked_path(&out.join("second.bin")).exists());
    storage.set_file_selection(Some(&[0])).unwrap(); assert!(!out.join("second.bin").exists());
    storage.set_file_selection(Some(&[0,1])).unwrap(); assert!(marked_path(&out.join("second.bin")).exists());
    drop(storage);
    session.detach_for_move(&h).await.unwrap();
    let moved = dir.0.join("moved"); std::fs::rename(&out, &moved).unwrap();
    let h = session.add_torrent(AddTorrent::from_bytes(bytes), Some(AddTorrentOptions { output_folder: Some(moved.to_string_lossy().into_owned()), overwrite: true, paused: true, only_files: Some(vec![0,1]), storage_factory: Some(factory.boxed()), ..Default::default() })).await.unwrap().into_handle().unwrap();
    settled(&h).await; assert!(h.stats().finished);
    for name in ["first.txt", "second.bin"] { assert!(moved.join(name).exists()); assert!(!marked_path(&moved.join(name)).exists()); }
    session.delete(h.info_hash().into(), true).await.unwrap();
    assert!(!moved.join("first.txt").exists() && !moved.join("second.bin").exists()); session.stop().await;
}

#[tokio::test]
async fn r14c_marker_aliases_are_rejected_only_when_marking_is_enabled() {
    use librqbit::storage::filesystem::FilesystemStorageFactory;
    let dir = TestDir::new(); let (bytes, _) = torrent(&[("file", 16384), ("file.!fx", 16384)], &[]);
    let s = Session::new_with_opts(dir.0.clone(), options(None)).await.unwrap();
    let response = s.add_torrent(AddTorrent::from_bytes(bytes.clone()), Some(AddTorrentOptions { overwrite: true, paused: true, storage_factory: Some(FilesystemStorageFactory { mark_unfinished: true }.boxed()), ..Default::default() })).await;
    assert!(response.is_err());
    let h = local_add(&s, bytes, &dir.0, None, vec![], true).await;
    assert!(dir.0.join("file").exists() && dir.0.join("file.!fx").exists());
    s.delete(h.info_hash().into(), true).await.unwrap(); s.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn r14c_new_torrent_and_magnet_keep_original_and_extra_trackers() {
    let dir = TestDir::new(); let seed_out = dir.0.join("seed"); std::fs::create_dir_all(&seed_out).unwrap();
    let original = "http://127.0.0.1:9/original".to_string(); let extra = "http://127.0.0.1:9/extra".to_string();
    let (bytes, data) = torrent(&[("file", 16384)], std::slice::from_ref(&original));
    std::fs::write(seed_out.join("file"), &data).unwrap();
    let mut opts = listening_options(None); opts.disable_trackers = true;
    let seed = Session::new_with_opts(seed_out.clone(), opts).await.unwrap();
    let sh = local_add(&seed, bytes.clone(), &seed_out, None, vec![], false).await;
    for magnet in [false, true] {
        let mut opts = options(None); opts.disable_trackers = true;
        let client = Session::new_with_opts(dir.0.join(if magnet { "magnet" } else { "torrent" }), opts).await.unwrap();
        let source = if magnet { AddTorrent::from_url(format!("magnet:?xt=urn:btih:{}&tr=http%3A%2F%2F127.0.0.1%3A9%2Foriginal", sh.info_hash().as_string())) } else { AddTorrent::from_bytes(bytes.clone()) };
        let h = tokio::time::timeout(Duration::from_secs(10), client.add_torrent(source, Some(AddTorrentOptions { overwrite: true, trackers: Some(vec![extra.clone(), original.clone()]), initial_peers: Some(vec![seed.listen_addr().unwrap()]), ..Default::default() }))).await.unwrap().unwrap().into_handle().unwrap();
        assert!(h.shared().trackers.contains(&url::Url::parse(&original).unwrap()));
        assert!(h.shared().trackers.contains(&url::Url::parse(&extra).unwrap()));
        assert_eq!(h.shared().trackers.len(), 2);
        client.stop().await;
    }
    seed.stop().await;
}

#[tokio::test]
async fn r14c_marked_single_file_moves_and_unloaded_delete_use_saved_paths() {
    use librqbit::storage::filesystem::marked_path;
    let dir = TestDir::new(); let out = dir.0.join("old"); let dest = dir.0.join("new"); std::fs::create_dir_all(&out).unwrap();
    let bytes = b"d4:infod6:lengthi3e4:name4:file12:piece lengthi16384e6:pieces20:abcdefghijklmnopqrstee";
    let meta = parse(bytes).unwrap(); let cache = dir.0.join("info.torrent"); std::fs::write(&cache, bytes).unwrap();
    std::fs::write(marked_path(&out.join("file")), b"abc").unwrap();
    let mgr = Manager::new();
    let mut it = Item { is_torrent: true, torrent_mark_unfinished: true, torrent_file: cache.to_string_lossy().into_owned(), info_hash: meta.hash.clone(), save_folder: out.to_string_lossy().into_owned(), downloaded: 3, ..Default::default() };
    assert!(!wanted_files_missing(&it, &meta));
    mgr.st.lock().items = vec![it.clone()];
    mgr.torrents.move_data(&mgr, &it.id, &dest.to_string_lossy()).await.unwrap();
    it.save_folder = dest.to_string_lossy().into_owned();
    assert!(!wanted_files_missing(&it, &meta)); assert!(!out.join("file.!fx").exists());
    std::fs::write(dest.join("unrelated"), b"keep").unwrap();
    mgr.torrents.remove(&mgr, &it, true).await;
    assert!(!dest.join("file.!fx").exists()); assert!(dest.join("unrelated").exists());
}

#[test]
fn r14c_marker_fits_maximum_windows_filename_and_retains_final_name() {
    use librqbit::storage::filesystem::marked_path;
    let name = format!("{}file", "界".repeat(251));
    let marker = marked_path(Path::new(&name));
    assert_eq!(marker.to_string_lossy().encode_utf16().count(), 255);
    assert!(marker.to_string_lossy().ends_with(".!fx"));
    let name = format!("{}abc", "😀".repeat(126));
    let marker = marked_path(Path::new(&name));
    assert!(marker.to_string_lossy().encode_utf16().count() <= 255);
    assert!(!marker.to_string_lossy().contains('\u{fffd}'));
}

#[tokio::test(flavor = "multi_thread")]
async fn r14c_deferred_exclusions_apply_once_and_marker_does_not_affect_matching() {
    let dir = TestDir::new(); let out = dir.0.join("files"); std::fs::create_dir_all(out.join("test")).unwrap();
    let (bytes, data) = torrent(&[("video.mkv", 16384), ("ReadMe.TXT", 16384)], &[]);
    std::fs::write(out.join("test/video.mkv"), &data[..16384]).unwrap();
    let cache = dir.0.join("info.torrent"); std::fs::write(&cache, &bytes).unwrap();
    let mgr = Manager::new();
    { let mut st = mgr.st.lock(); st.settings.torrent_skip_patterns = "*.txt".into(); st.settings.torrent_mark_unfinished = true; }
    let mut it = mgr.create_torrent_item(cache.to_str().unwrap(), None, Some(out.to_str().unwrap()), None);
    it.torrent_file = cache.to_string_lossy().into_owned();
    mgr.st.lock().items = vec![it.clone()];
    let preview = mgr.torrents.info(&mgr, cache.to_str().unwrap(), "").await.unwrap();
    assert_eq!(preview["files"][1]["wanted"], false);
    let session = Session::new_with_opts(out.clone(), options(None)).await.unwrap();
    *mgr.torrents.session.lock().await = Some(session.clone()); *mgr.torrents.live.lock() = Some(session.clone());
    let run = Run::test_run(&it.id); let mut handle = None;
    go(&mgr, &run, &mut handle).await.unwrap();
    let saved = mgr.item(&it.id).unwrap();
    assert_eq!(saved.skipped_files, vec![1]); assert!(saved.torrent_skip_patterns.is_empty());
    assert_eq!(saved.status, DlStatus::Completed);
    assert!(!out.join("test/ReadMe.TXT").exists() && !out.join("test/ReadMe.TXT.!fx").exists());
    assert!(out.join("test/video.mkv").exists() && !out.join("test/video.mkv.!fx").exists());
    session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn r14c_local_marked_transfer_publishes_or_reports_conflict_without_overwrite() {
    use librqbit::storage::filesystem::{FilesystemStorageFactory, marked_path};
    let dir = TestDir::new(); let seed_out = dir.0.join("seed"); std::fs::create_dir_all(&seed_out).unwrap();
    let (bytes, data) = torrent(&[("first", 16384), ("second", 16384)], &[]);
    std::fs::write(seed_out.join("first"), &data[..16384]).unwrap(); std::fs::write(seed_out.join("second"), &data[16384..]).unwrap();
    let seed = Session::new_with_opts(seed_out.clone(), listening_options(None)).await.unwrap();
    let sh = local_add(&seed, bytes.clone(), &seed_out, None, vec![], false).await;
    assert!(sh.stats().finished);
    for conflict in [false, true] {
        let out = dir.0.join(if conflict { "conflict" } else { "normal" });
        let client = Session::new_with_opts(out.clone(), options(None)).await.unwrap();
        let h = client.add_torrent(AddTorrent::from_bytes(bytes.clone()), Some(AddTorrentOptions { output_folder: Some(out.to_string_lossy().into_owned()), overwrite: true, paused: true, storage_factory: Some(FilesystemStorageFactory { mark_unfinished: true }.boxed()), initial_peers: Some(vec![seed.listen_addr().unwrap()]), ..Default::default() })).await.unwrap().into_handle().unwrap();
        settled(&h).await;
        assert!(marked_path(&out.join("first")).exists());
        if conflict { std::fs::write(out.join("first"), b"unrelated").unwrap(); }
        client.unpause(&h).await.unwrap();
        if conflict {
            tokio::time::timeout(Duration::from_secs(15), async {
                while !matches!(h.stats().state, TorrentStatsState::Error) { tokio::time::sleep(Duration::from_millis(10)).await; }
            }).await.expect("publication failure did not reach torrent error state");
            assert_eq!(std::fs::read(out.join("first")).unwrap(), b"unrelated");
            assert_eq!(std::fs::read(marked_path(&out.join("first"))).unwrap(), &data[..16384]);
        } else {
            completed(&h).await;
            assert_eq!(std::fs::read(out.join("first")).unwrap(), &data[..16384]); assert_eq!(std::fs::read(out.join("second")).unwrap(), &data[16384..]);
            assert!(!marked_path(&out.join("first")).exists() && !marked_path(&out.join("second")).exists());
        }
        client.stop().await;
    }
    seed.stop().await;
}

#[test]
fn r14d_settings_and_blocklist_sources() {
    let s: Settings = serde_json::from_str("{}").unwrap();
    assert!(s.torrent_enable_dht && s.torrent_enable_local_discovery);
    assert!(!s.torrent_ipv4_only);
    assert!(s.torrent_watch_folder.is_empty() && s.torrent_blocklist_url.is_empty());
    let mut value = serde_json::to_value(&s).unwrap();
    value["TorrentEnableDht"] = false.into();
    value["TorrentEnableLocalDiscovery"] = false.into();
    value["TorrentIpv4Only"] = true.into();
    value["TorrentBlocklistUrl"] = "https://example.invalid/list.gz".into();
    value["TorrentWatchFolder"] = "watch".into();
    let loaded: Settings = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(loaded).unwrap(), value);
    assert_eq!(blocklist_source("  ").unwrap(), None);
    assert!(blocklist_source("https://example.invalid/list.gz").unwrap().is_some());
    for bad in ["ftp://example.invalid/list", "relative/list", "https://user:secret@example.invalid/list"] {
        assert!(blocklist_source(bad).is_err());
    }
    let dir = TestDir::new();
    let file = dir.0.join("ranges.txt");
    assert!(blocklist_source(file.to_str().unwrap()).unwrap().unwrap().starts_with("file:"));
}

#[tokio::test]
async fn r14d_blocklist_local_refresh_retains_good_ranges_on_failure() {
    let dir = TestDir::new();
    let file = dir.0.join("ranges.txt");
    std::fs::write(&file, "blocked:192.0.2.1-192.0.2.10\n").unwrap();
    let source = blocklist_source(file.to_str().unwrap()).unwrap().unwrap();
    let mut opts = options(None);
    opts.blocklist_url = Some(source.clone());
    opts.ipv4_only = true;
    let s = Session::new_with_opts(dir.0.join("data"), opts).await.unwrap();
    assert!(s.get_dht().is_none() && s.ipv4_only);
    assert!(s.blocklist.read().has("192.0.2.1".parse().unwrap()));
    assert!(s.blocklist.read().has("192.0.2.10".parse().unwrap()));
    assert!(!s.blocklist.read().has("192.0.2.11".parse().unwrap()));
    std::fs::write(&file, "updated:198.51.100.1-198.51.100.5\n").unwrap();
    s.reload_blocklist(&source).await.unwrap();
    assert!(!s.blocklist.read().has("192.0.2.1".parse().unwrap()));
    assert!(s.blocklist.read().has("198.51.100.5".parse().unwrap()));
    std::fs::write(&file, "<html>service unavailable</html>").unwrap();
    assert!(s.reload_blocklist(&source).await.is_err());
    assert!(s.blocklist.read().has("198.51.100.5".parse().unwrap()));
    std::fs::remove_file(&file).unwrap();
    assert!(s.reload_blocklist(&source).await.is_err());
    assert!(s.blocklist.read().has("198.51.100.5".parse().unwrap()));
    s.stop().await;
}

#[tokio::test]
async fn r14d_watched_folder_waits_for_stability_archives_and_deduplicates() {
    let dir = TestDir::new();
    let mgr = Manager::new();
    {
        let mut st = mgr.st.lock();
        st.settings.torrent_watch_folder = dir.0.to_string_lossy().into_owned();
        st.settings.default_folder = dir.0.join("downloads").to_string_lossy().into_owned();
        st.settings.category_subfolders = true;
        st.settings.lookup_covers_online = false;
    }
    let (bytes, _) = torrent(&[("payload.bin", 32)], &[]);
    let file = dir.0.join("incoming.TORRENT");
    std::fs::write(&file, &bytes[..10]).unwrap();
    let mut watcher = crate::watched::Watcher::default();
    let now = std::time::Instant::now();
    watcher.scan(&mgr, now, false).await;
    assert!(mgr.st.lock().items.is_empty());
    std::fs::write(&file, &bytes).unwrap();
    watcher.scan(&mgr, now + Duration::from_secs(5), false).await;
    assert!(mgr.st.lock().items.is_empty());
    watcher.scan(&mgr, now + Duration::from_secs(10), false).await;
    assert!(!file.exists());
    assert!(dir.0.join("added/incoming.TORRENT").exists());
    let item = mgr.st.lock().items[0].clone();
    assert!(Path::new(&item.url).exists());
    assert!(item.save_folder.ends_with("Torrents"));
    assert_eq!(item.info_hash, parse(&bytes).unwrap().hash);
    std::fs::write(&file, &bytes).unwrap();
    watcher.scan(&mgr, now + Duration::from_secs(12), false).await;
    watcher.scan(&mgr, now + Duration::from_secs(17), false).await;
    assert_eq!(mgr.st.lock().items.len(), 1);
    assert_eq!(std::fs::read_dir(dir.0.join("added")).unwrap().count(), 2);
    let bad = dir.0.join("bad.torrent");
    std::fs::write(&bad, b"bad metadata").unwrap();
    watcher.scan(&mgr, now + Duration::from_secs(20), false).await;
    watcher.scan(&mgr, now + Duration::from_secs(25), false).await;
    assert!(bad.exists());
    assert_eq!(mgr.st.lock().items.len(), 1);
    mgr.st.lock().settings.torrent_watch_folder.clear();
    let ignored = dir.0.join("ignored.torrent");
    std::fs::write(&ignored, bytes).unwrap();
    watcher.scan(&mgr, now + Duration::from_secs(40), false).await;
    assert!(ignored.exists());
}

#[tokio::test]
async fn r14d_blocklist_http_gzip_and_initial_failure() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = TestDir::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source = format!("http://{}/list.gz", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        socket.read(&mut request).await.unwrap();
        let bytes: &[u8] = &[31, 139, 8, 0, 0, 0, 0, 0, 2, 10, 43, 73, 45, 46, 177, 50, 50, 48, 214, 51, 208, 51, 52, 52, 214, 51, 212, 69, 176, 45, 185, 0, 136, 188, 70, 54, 29, 0, 0, 0];
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).as_bytes()).await.unwrap();
        socket.write_all(bytes).await.unwrap();
    });
    let mut opts = options(None);
    opts.blocklist_url = Some(source);
    let session = Session::new_with_opts(dir.0.join("http"), opts).await.unwrap();
    assert!(session.blocklist.read().has("203.0.113.1".parse().unwrap()));
    assert!(session.blocklist.read().has("203.0.113.9".parse().unwrap()));
    assert!(!session.blocklist.read().has("203.0.113.10".parse().unwrap()));
    session.stop().await;
    server.await.unwrap();
    let mut opts = options(None);
    opts.blocklist_url = blocklist_source(dir.0.join("missing.txt").to_str().unwrap()).unwrap();
    assert!(Session::new_with_opts(dir.0.join("missing"), opts).await.is_err());
}

#[tokio::test]
async fn r14d_watched_folder_recovers_invalid_files_and_archive_failures() {
    let dir = TestDir::new();
    let mgr = Manager::new();
    {
        let mut st = mgr.st.lock();
        st.settings.torrent_watch_folder = dir.0.to_string_lossy().into_owned();
        st.settings.default_folder = dir.0.join("output").to_string_lossy().into_owned();
        st.settings.lookup_covers_online = false;
    }
    let warnings = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = warnings.clone();
    mgr.set_ui(Box::new(move |event, _| {
        if event == "toast" { count.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
    }));
    let file = dir.0.join("retry.torrent");
    std::fs::write(&file, b"invalid").unwrap();
    let mut w = crate::watched::Watcher::default();
    let now = std::time::Instant::now();
    w.scan(&mgr, now, false).await;
    w.scan(&mgr, now + Duration::from_secs(5), false).await;
    w.scan(&mgr, now + Duration::from_secs(10), false).await;
    assert_eq!(warnings.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(mgr.st.lock().items.is_empty());
    let (bytes, _) = torrent(&[("retry.bin", 17)], &[]);
    std::fs::write(&file, &bytes).unwrap();
    std::fs::write(dir.0.join("added"), b"folder blocked").unwrap();
    w.scan(&mgr, now + Duration::from_secs(15), false).await;
    w.scan(&mgr, now + Duration::from_secs(20), false).await;
    assert_eq!(mgr.st.lock().items.len(), 1);
    assert!(file.exists());
    assert!(Path::new(&mgr.st.lock().items[0].url).exists());
    std::fs::remove_file(dir.0.join("added")).unwrap();
    // Restarting the watcher retries the archive without adding the same hash twice.
    let mut w = crate::watched::Watcher::default();
    w.scan(&mgr, now + Duration::from_secs(25), false).await;
    w.scan(&mgr, now + Duration::from_secs(30), false).await;
    assert!(!file.exists());
    assert_eq!(mgr.st.lock().items.len(), 1);
    // Nested files are deliberately outside the watch scope.
    let nested = dir.0.join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join("untouched.torrent"), bytes).unwrap();
    w.scan(&mgr, now + Duration::from_secs(40), false).await;
    assert!(nested.join("untouched.torrent").exists());
}

#[tokio::test]
async fn r14d_service_applies_saved_network_settings() {
    let dir = TestDir::new();
    let ranges = dir.0.join("service-ranges.txt");
    std::fs::write(&ranges, "test:192.0.2.1-192.0.2.2\n").unwrap();
    let mgr = Manager::new();
    {
        let mut st = mgr.st.lock();
        st.settings.default_folder = dir.0.join("files").to_string_lossy().into_owned();
        st.settings.torrent_enable_dht = false;
        st.settings.torrent_enable_local_discovery = false;
        st.settings.torrent_ipv4_only = true;
        st.settings.torrent_use_upnp = false;
        st.settings.torrent_blocklist_url = ranges.to_string_lossy().into_owned();
        st.settings.extra.insert("TorrentEnableUtp".into(), false.into());
    }
    let session = mgr.torrents.session(&mgr).await.unwrap();
    assert!(session.get_dht().is_none());
    assert!(session.ipv4_only);
    assert!(session.listen_addr().unwrap().is_ipv4());
    assert!(session.blocklist.read().has("192.0.2.2".parse().unwrap()));
    // A live session keeps its startup configuration until the documented restart.
    mgr.st.lock().settings.torrent_ipv4_only = false;
    assert!(Arc::ptr_eq(&session, &mgr.torrents.session(&mgr).await.unwrap()));
    assert!(session.ipv4_only);
    mgr.torrents.shutdown().await;
}
