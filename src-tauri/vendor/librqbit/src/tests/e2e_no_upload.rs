//! FluxDM patch: an upload limit of 1 B/s ("No upload") sends nothing, leaves the torrent healthy, and
//! uploading works again as soon as the limit is lifted.

use std::{net::Ipv4Addr, num::NonZeroU32, time::Duration};

use crate::{
    AddTorrent, AddTorrentOptions, CreateTorrentOptions, ListenerMode, ManagedTorrentState, Session,
    SessionOptions, create_torrent, limits::LimitsConfig, listen::ListenerOptions,
    spawn_utils::BlockingSpawner, tests::test_util::create_default_random_dir_with_torrents,
};

#[tokio::test(flavor = "multi_thread")]
async fn no_upload_sends_nothing_until_lifted() {
    let dir = create_default_random_dir_with_torrents(2, 2_000_000, Some("fluxdm_noupload"));
    let torrent = create_torrent(
        dir.path(),
        CreateTorrentOptions { piece_length: Some(32768), ..Default::default() },
        &BlockingSpawner::new(1),
    )
    .await
    .unwrap();
    let bytes = torrent.as_bytes().unwrap();

    // The seeder: has every piece, uploads switched off for the whole session.
    let server = Session::new_with_opts(
        std::env::temp_dir().join("does_not_exist"),
        SessionOptions {
            dht: None,
            listen: Some(ListenerOptions {
                mode: ListenerMode::TcpOnly,
                listen_addr: (Ipv4Addr::LOCALHOST, 15190).into(),
                ..Default::default()
            }),
            disable_local_service_discovery: true,
            ratelimits: LimitsConfig { upload_bps: NonZeroU32::new(1), download_bps: None },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(server.ratelimits.upload_blocked());
    let seed = server
        .add_torrent(
            AddTorrent::TorrentFileBytes(bytes.clone()),
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(dir.path().to_str().unwrap().to_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !seed.with_state(|s| matches!(s, ManagedTorrentState::Live(l) if l.is_finished())) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("seeder never went live");
    let addr = server.listen_addr().unwrap();

    // The downloader, pointed straight at the seeder.
    let out = tempfile::TempDir::with_prefix("fluxdm_noupload_client").unwrap();
    let client = Session::new_with_opts(
        out.path().to_owned(),
        SessionOptions { dht: None, disable_local_service_discovery: true, ..Default::default() },
    )
    .await
    .unwrap();
    let leech = client
        .add_torrent(
            AddTorrent::TorrentFileBytes(bytes),
            Some(AddTorrentOptions { initial_peers: Some(vec![addr]), overwrite: true, ..Default::default() }),
        )
        .await
        .unwrap()
        .into_handle()
        .unwrap();

    // Connected, asking for pieces, getting nothing — and the seeder's torrent must not break over it.
    tokio::time::sleep(Duration::from_secs(8)).await;
    let st = leech.stats();
    assert_eq!(st.progress_bytes, 0, "the seeder uploaded while uploads were off");
    assert!(seed.with_state(|s| matches!(s, ManagedTorrentState::Live(_))), "the seeder's torrent left Live");
    assert_eq!(seed.stats().uploaded_bytes, 0);

    // Lift it: the download completes.
    server.ratelimits.set_upload_bps(None);
    assert!(!server.ratelimits.upload_blocked());
    tokio::time::timeout(Duration::from_secs(120), leech.wait_until_completed())
        .await
        .expect("no upload after the limit was lifted")
        .unwrap();
    assert!(seed.stats().uploaded_bytes > 0);
}
