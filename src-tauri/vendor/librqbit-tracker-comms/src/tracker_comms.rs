use std::collections::HashSet;
use std::net::SocketAddr;
use std::net::SocketAddrV4;
use std::net::SocketAddrV6;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::bail;
use backon::ExponentialBuilder;
use backon::Retryable;
use futures::FutureExt;
use futures::StreamExt;
use futures::future::Either;
use futures::stream::BoxStream;
use futures::stream::FuturesUnordered;
use tracing::Instrument;
use tracing::debug;
use tracing::debug_span;
use tracing::trace;
use tracing::trace_span;
use url::Url;

use crate::tracker_comms_http;
use crate::tracker_comms_udp;
use crate::tracker_comms_udp::UdpTrackerClient;
use librqbit_core::hash_id::Id20;

// FluxDM patch: what each tracker last said, for the Trackers tab (seeds, leeches, message).
pub mod status {
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::LazyLock;
    use std::time::{Duration, Instant};

    #[derive(Clone, Debug, Default, PartialEq)]
    pub enum State {
        #[default]
        NotContacted,
        Updating,
        Working,
        Error,
    }

    #[derive(Clone, Debug, Default)]
    pub struct TrackerStatus {
        pub state: State,
        /// Seeders / leechers as the tracker counts them (None until it answered).
        pub seeds: Option<u64>,
        pub leeches: Option<u64>,
        pub peers_received: u64,
        /// The tracker's failure or warning text, or our error talking to it.
        pub message: String,
        pub last_ok: Option<Instant>,
        pub next_announce: Option<Instant>,
    }

    // info hash (hex) -> tracker url -> status
    static TABLE: LazyLock<Mutex<HashMap<String, HashMap<String, TrackerStatus>>>> = LazyLock::new(Default::default);

    pub fn get(info_hash_hex: &str) -> HashMap<String, TrackerStatus> {
        TABLE.lock().get(info_hash_hex).cloned().unwrap_or_default()
    }

    pub fn forget(info_hash_hex: &str) {
        TABLE.lock().remove(info_hash_hex);
    }

    pub(crate) fn update(info_hash_hex: &str, url: &str, f: impl FnOnce(&mut TrackerStatus)) {
        let mut t = TABLE.lock();
        f(t.entry(info_hash_hex.to_string()).or_default().entry(url.to_string()).or_default());
    }

    pub(crate) fn ok(hash: &str, url: &str, seeds: Option<u64>, leeches: Option<u64>, peers: usize, warning: Option<String>, next: Duration) {
        update(hash, url, |s| {
            s.state = State::Working;
            s.seeds = seeds;
            s.leeches = leeches;
            s.peers_received = peers as u64;
            s.message = warning.unwrap_or_default();
            s.last_ok = Some(Instant::now());
            s.next_announce = Some(Instant::now() + next);
        });
    }

    pub(crate) fn ok_udp(hash: &str, url: &str, last: &Mutex<(u64, u64, usize, Duration)>) {
        let (seeds, leeches, peers, next) = *last.lock();
        ok(hash, url, Some(seeds), Some(leeches), peers, None, next);
    }

    pub(crate) fn err(hash: &str, url: &str, message: String, retry_in: Option<Duration>) {
        update(hash, url, |s| {
            s.state = State::Error;
            s.message = message;
            s.next_announce = retry_in.map(|d| Instant::now() + d);
        });
    }
}

pub struct TrackerComms {
    info_hash: Id20,
    peer_id: Id20,
    stats: Box<dyn TorrentStatsProvider>,
    force_tracker_interval: Option<Duration>,
    tx: Sender,
    // This MUST be set as trackers don't work with 0 port.
    announce_port: u16,
    reqwest_client: reqwest::Client,
    key: u32,
    // FluxDM patch: seeds, leeches, peers, interval of the last UDP answer.
    last_udp: parking_lot::Mutex<(u64, u64, usize, Duration)>,
    // FluxDM patch: each tracker subscribes independently.
    force_announce: tokio::sync::watch::Receiver<u64>,
}

#[derive(Default)]
pub enum TrackerCommsStatsState {
    #[default]
    None,
    Initializing,
    Paused,
    Live,
}

#[derive(Default)]
pub struct TrackerCommsStats {
    pub uploaded_bytes: u64,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub torrent_state: TrackerCommsStatsState,
}

impl TrackerCommsStats {
    pub fn get_left_to_download_bytes(&self) -> u64 {
        let total = self.total_bytes;
        let down = self.downloaded_bytes;
        if total >= down {
            return total - down;
        }
        0
    }

    pub fn is_completed(&self) -> bool {
        self.downloaded_bytes >= self.total_bytes
    }
}

pub trait TorrentStatsProvider: Send + Sync {
    fn get(&self) -> TrackerCommsStats;
}

impl TorrentStatsProvider for () {
    fn get(&self) -> TrackerCommsStats {
        Default::default()
    }
}

type Sender = tokio::sync::mpsc::Sender<SocketAddr>;

enum SupportedTracker {
    Udp(Url),
    Http(Url),
}

impl std::fmt::Debug for SupportedTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SupportedTracker::Udp(u) => std::fmt::Display::fmt(u, f),
            SupportedTracker::Http(u) => std::fmt::Display::fmt(u, f),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum UdpTrackerResolveResult {
    One(SocketAddr),
    Two(SocketAddrV4, SocketAddrV6),
}

async fn udp_tracker_to_socket_addrs(
    host: url::Host<&str>,
    port: u16,
) -> anyhow::Result<UdpTrackerResolveResult> {
    let res = match host {
        url::Host::Domain(name) => {
            // Use the first IPv4 and the first IPv6 addresses only.

            let mut v4: Option<SocketAddrV4> = None;
            let mut v6: Option<SocketAddrV6> = None;
            for addr in tokio::net::lookup_host((name, port))
                .await
                .with_context(|| format!("error looking up hostname {name}"))?
            {
                match (v4, v6, addr) {
                    (None, _, SocketAddr::V4(addr)) => v4 = Some(addr),
                    (_, None, SocketAddr::V6(addr)) => v6 = Some(addr),
                    _ => continue,
                }
            }
            let res = match (v4, v6) {
                (Some(v4), Some(v6)) => UdpTrackerResolveResult::Two(v4, v6),
                (Some(v4), None) => UdpTrackerResolveResult::One(v4.into()),
                (None, Some(v6)) => UdpTrackerResolveResult::One(v6.into()),
                _ => anyhow::bail!("zero addresses returned looking up {name}"),
            };
            trace!(?res, "resolved");
            res
        }
        url::Host::Ipv4(addr) => UdpTrackerResolveResult::One((addr, port).into()),
        url::Host::Ipv6(addr) => UdpTrackerResolveResult::One((addr, port).into()),
    };
    Ok(res)
}

impl TrackerComms {
    // TODO: fix too many args
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        info_hash: Id20,
        peer_id: Id20,
        trackers: HashSet<Url>,
        stats: Box<dyn TorrentStatsProvider>,
        force_interval: Option<Duration>,
        announce_port: u16,
        reqwest_client: reqwest::Client,
        udp_client: UdpTrackerClient,
        force_announce: tokio::sync::watch::Receiver<u64>,
    ) -> Option<BoxStream<'static, SocketAddr>> {
        let trackers = trackers
            .into_iter()
            .filter_map(|t| match t.scheme() {
                "http" | "https" => Some(SupportedTracker::Http(t)),
                "udp" => Some(SupportedTracker::Udp(t)),
                _ => {
                    debug!("unsupported tracker URL: {}", t);
                    None
                }
            })
            .collect::<Vec<_>>();
        if trackers.is_empty() {
            debug!(?info_hash, "trackers list is empty");
            return None;
        }

        tracing::trace!(?trackers);
        status::forget(&info_hash.as_string());

        let (tx, mut rx) = tokio::sync::mpsc::channel::<SocketAddr>(16);

        let s = async_stream::stream! {
            use futures::StreamExt;
            let comms = Arc::new(Self {
                info_hash,
                peer_id,
                stats,
                force_tracker_interval: force_interval,
                tx,
                announce_port,
                reqwest_client,
                key: rand::random(),
                last_udp: Default::default(),
                force_announce,
            });
            let mut futures = FuturesUnordered::new();
            for tracker in trackers {
                futures.push(comms.add_tracker(tracker, &udp_client))
            }
            while !(futures.is_empty()) {
                tokio::select! {
                    addr = rx.recv() => {
                        if let Some(addr) = addr {
                            yield addr;
                        }
                    }
                    e = futures.next(), if !futures.is_empty() => {
                        if let Some(Err(e)) = e {
                            debug!("error: {e}");
                        }
                    }
                }
            }
        };

        Some(s.boxed())
    }

    fn add_tracker(
        &self,
        url: SupportedTracker,
        client: &UdpTrackerClient,
    ) -> Either<
        impl std::future::Future<Output = anyhow::Result<()>> + '_ + Send,
        impl std::future::Future<Output = anyhow::Result<()>> + '_ + Send,
    > {
        let info_hash = self.info_hash;
        match url {
            SupportedTracker::Udp(url) => {
                let span = debug_span!(parent: None, "udp_tracker", tracker = %url, info_hash = ?info_hash);
                self.task_single_tracker_monitor_udp(url, client.clone())
                    .instrument(span)
                    .right_future()
            }
            SupportedTracker::Http(url) => {
                let span = debug_span!(
                    parent: None,
                    "http_tracker",
                    tracker = %url,
                    info_hash = ?info_hash
                );
                self.task_single_tracker_monitor_http(url)
                    .instrument(span)
                    .left_future()
            }
        }
    }

    async fn task_single_tracker_monitor_http(&self, tracker_url: Url) -> anyhow::Result<()> {
        trace!(url=%tracker_url, "starting monitor");
        let mut wake = self.force_announce.clone();
        let mut event = Some(tracker_comms_http::TrackerRequestEvent::Started);
        let hash = self.info_hash.as_string();
        let url_s = tracker_url.to_string();

        loop {
            wake.borrow_and_update();
            status::update(&hash, &url_s, |s| s.state = status::State::Updating);
            let request = (|| self.tracker_one_request_http(&tracker_url, event))
                .retry(
                    ExponentialBuilder::new()
                        .without_max_times()
                        .with_jitter()
                        .with_factor(2.)
                        .with_min_delay(Duration::from_secs(10))
                        .with_max_delay(Duration::from_secs(600)),
                )
                .notify(|err, retry_in| {
                    debug!(?retry_in, "error calling tracker: {err:#}");
                    status::err(&hash, &url_s, tracker_message(err), Some(retry_in));
                });
            // FluxDM patch: a manual request also interrupts retry backoff.
            tokio::pin!(request);
            let interval = tokio::select! {
                result = &mut request => result.context("this shouldn't fail")?,
                _ = wake.changed() => continue,
            };

            event = None;
            let interval = self.force_tracker_interval.unwrap_or(interval);
            debug!("sleeping for {:?} after calling tracker", interval);
            tokio::select! {
                _ = tokio::time::sleep(interval) => {},
                _ = wake.changed() => {},
            }
        }
    }

    async fn tracker_one_request_http(
        &self,
        tracker_url: &Url,
        event: Option<tracker_comms_http::TrackerRequestEvent>,
    ) -> anyhow::Result<Duration> {
        let stats = self.stats.get();
        let request = tracker_comms_http::TrackerRequest {
            info_hash: &self.info_hash,
            peer_id: &self.peer_id,
            port: self.announce_port,
            uploaded: stats.uploaded_bytes,
            downloaded: stats.downloaded_bytes,
            left: stats.get_left_to_download_bytes(),
            compact: true,
            no_peer_id: false,
            event,
            ip: None,
            numwant: None,
            key: Some(self.key),
            trackerid: None,
        };

        let mut url = tracker_url.clone();

        let mut queries = request.as_querystring();
        if let Some(url_query) = url.query() {
            queries.push_str(&format!("&{}", url_query));
        }
        url.set_query(Some(&queries));

        let response: reqwest::Response = self.reqwest_client.get(url).send().await?;
        if !response.status().is_success() {
            anyhow::bail!("tracker responded with {:?}", response.status());
        }
        let bytes = response.bytes().await?;
        if let Ok((error, _)) =
            bencode::from_bytes_with_rest::<tracker_comms_http::TrackerError>(&bytes)
        {
            anyhow::bail!(
                "tracker returned failure. Failure reason: {}",
                error.failure_reason
            )
        };
        let response = bencode::from_bytes_with_rest::<tracker_comms_http::TrackerResponse>(&bytes)
            .map_err(|e| {
                tracing::trace!("error deserializing TrackerResponse: {e:#}");
                e.into_kind()
            })?
            .0;

        let next = Duration::from_secs(response.min_interval.unwrap_or(response.interval));
        status::ok(
            &self.info_hash.as_string(),
            tracker_url.as_str(),
            response.complete,
            response.incomplete,
            response.iter_peers().count(),
            response.warning_message.as_ref().map(|w| String::from_utf8_lossy(w.as_ref()).into_owned()),
            self.force_tracker_interval.unwrap_or(next),
        );
        for peer in response.iter_peers() {
            self.tx.send(peer).await?;
        }
        Ok(next)
    }

    async fn task_single_tracker_monitor_udp(
        &self,
        url: Url,
        client: UdpTrackerClient,
    ) -> anyhow::Result<()> {
        if url.scheme() != "udp" {
            bail!("expected UDP scheme in {}", url);
        }
        let (host, port) = (
            url.host().context("missing host")?,
            url.port().context("missing port")?,
        );

        let mut wake = self.force_announce.clone();
        let mut sleep_interval: Option<Duration> = None;
        let mut prev_addrs: Option<UdpTrackerResolveResult> = None;
        let hash = self.info_hash.as_string();
        let url_s = url.to_string();
        loop {
            if let Some(i) = sleep_interval {
                trace!(interval=?sleep_interval, "sleeping");
                tokio::select! {
                    _ = tokio::time::sleep(i) => {},
                    _ = wake.changed() => {},
                }
            }
            wake.borrow_and_update();
            status::update(&hash, &url_s, |s| s.state = status::State::Updating);

            // This should retry forever until the addrs are resolved.
            let addrs = {
                let resolve = (async || {
                    udp_tracker_to_socket_addrs(host.clone(), port)
                        .instrument(trace_span!("resolve", ?host))
                        .await
                        .or_else(|err| prev_addrs.ok_or(err))
                })
                .retry(
                    ExponentialBuilder::new()
                        .without_max_times()
                        .with_max_delay(Duration::from_secs(60))
                        .with_jitter(),
                )
                .notify(|err, retry| {
                    debug!(retry_in=?retry, "error resolving tracker: {err:#}");
                    status::err(&hash, &url_s, "Can't resolve the tracker's address".into(), Some(retry));
                });
                // FluxDM patch: manual requests bypass address-resolution retry backoff too.
                tokio::pin!(resolve);
                tokio::select! {
                    result = &mut resolve => result.context("failed resolving tracker addrs")?,
                    _ = wake.changed() => { sleep_interval = None; continue; },
                }
            };

            prev_addrs = Some(addrs);

            match addrs {
                UdpTrackerResolveResult::One(addr) => {
                    match self
                        .tracker_one_request_udp(addr, &client)
                        .instrument(trace_span!("udp request", ?addr))
                        .await
                    {
                        Ok(sleep) => {
                            sleep_interval = Some(sleep);
                            status::ok_udp(&hash, &url_s, &self.last_udp);
                        }
                        Err(e) => {
                            sleep_interval = Some(sleep_interval.unwrap_or(Duration::from_secs(60)));
                            status::err(&hash, &url_s, tracker_message(&e), sleep_interval);
                        }
                    }
                }
                UdpTrackerResolveResult::Two(v4, v6) => {
                    let (r4, r6) = tokio::join!(
                        self.tracker_one_request_udp(v4.into(), &client)
                            .instrument(trace_span!("udp request", addr=?v4)),
                        self.tracker_one_request_udp(v6.into(), &client)
                            .instrument(trace_span!("udp request", addr=?v6))
                    );
                    match (&r4, &r6) {
                        (Err(e), Err(_)) => status::err(
                            &hash,
                            &url_s,
                            tracker_message(e),
                            Some(sleep_interval.unwrap_or(Duration::from_secs(60))),
                        ),
                        _ => status::ok_udp(&hash, &url_s, &self.last_udp),
                    }
                    sleep_interval = Some(
                        r4.or(r6)
                            .ok()
                            .or(sleep_interval)
                            .unwrap_or(Duration::from_secs(60)),
                    )
                }
            }
        }
    }

    async fn tracker_one_request_udp(
        &self,
        addr: SocketAddr,
        client: &UdpTrackerClient,
    ) -> anyhow::Result<Duration> {
        use tracker_comms_udp::*;

        let stats = self.stats.get();
        let request = AnnounceFields {
            info_hash: self.info_hash,
            peer_id: self.peer_id,
            downloaded: stats.downloaded_bytes,
            left: stats.get_left_to_download_bytes(),
            uploaded: stats.uploaded_bytes,
            event: match stats.torrent_state {
                TrackerCommsStatsState::None => EVENT_NONE,
                TrackerCommsStatsState::Initializing => EVENT_STARTED,
                TrackerCommsStatsState::Paused => EVENT_STOPPED,
                TrackerCommsStatsState::Live => {
                    if stats.is_completed() {
                        EVENT_COMPLETED
                    } else {
                        EVENT_STARTED
                    }
                }
            },
            key: self.key,
            port: self.announce_port,
        };

        match client.announce(addr, request).await {
            Ok(response) => {
                trace!(len = response.addrs.len(), "received announce response");
                *self.last_udp.lock() = (
                    response.seeders as u64,
                    response.leechers as u64,
                    response.addrs.len(),
                    Duration::from_secs(response.interval.max(5) as u64),
                );
                for addr in response.addrs {
                    self.tx.send(addr).await.context("rx closed")?;
                }
                let sleep = response.interval.max(5);
                let sleep = Duration::from_secs(sleep as u64);
                Ok(sleep)
            }
            Err(e) => {
                debug!(?addr, "error reading announce response: {e:#}");
                Err(e)
            }
        }
    }
}

/// A tracker error as a short sentence for the Trackers tab.
fn tracker_message(e: &anyhow::Error) -> String {
    let full = format!("{e:#}");
    if let Some(reason) = full.split("Failure reason: ").nth(1) {
        return reason.trim().to_string();
    }
    let l = full.to_lowercase();
    if l.contains("timed out") || l.contains("timeout") || l.contains("elapsed") {
        "Timed out".into()
    } else if l.contains("dns") || l.contains("resolve") || l.contains("lookup") {
        "Can't resolve the tracker's address".into()
    } else if l.contains("refused") {
        "Connection refused".into()
    } else if let Some(code) = full.split("tracker responded with ").nth(1) {
        format!("Tracker answered {}", code.trim())
    } else {
        full.chars().take(160).collect()
    }
}
