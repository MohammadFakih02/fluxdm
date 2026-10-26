use arc_swap::ArcSwapOption;
use governor::DefaultDirectRateLimiter as RateLimiter;
use governor::Quota;
use serde::Deserialize;
use serde::Serialize;
use std::num::NonZeroU32;
use std::sync::Arc;

#[derive(Default, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitsConfig {
    pub upload_bps: Option<NonZeroU32>,
    pub download_bps: Option<NonZeroU32>,
}

/// FluxDM patch: smallest bucket, in bytes (two 16 KB blocks).
const MIN_BURST: u32 = 32 * 1024;

struct Limit {
    limiter: ArcSwapOption<RateLimiter>,
    current_bps: std::sync::atomic::AtomicU32,
}

impl Limit {
    fn new_inner(bps: Option<NonZeroU32>) -> Option<Arc<RateLimiter>> {
        let bps = bps?;
        // FluxDM patch: the bucket holds at least two 16 KB blocks. With a bucket of one second's worth, any limit
        // under 16 KB/s could never pass a block (governor refuses it outright), so the app had to raise every
        // limit to 32 KB/s. The average rate is still `bps`.
        let burst = bps.max(NonZeroU32::new(MIN_BURST).unwrap());
        Some(Arc::new(RateLimiter::direct(Quota::per_second(bps).allow_burst(burst))))
    }

    fn new(bps: Option<NonZeroU32>) -> Self {
        use std::sync::atomic::AtomicU32;
        Self {
            limiter: ArcSwapOption::new(Self::new_inner(bps)),
            current_bps: AtomicU32::new(bps.map(|v| v.get()).unwrap_or(0)),
        }
    }

    async fn acquire(&self, size: NonZeroU32) -> crate::Result<()> {
        let lim = self.limiter.load().clone();
        if let Some(rl) = lim.as_ref() {
            rl.until_n_ready(size).await?;
        }
        Ok(())
    }

    fn set(&self, limit: Option<NonZeroU32>) {
        use std::sync::atomic::Ordering;
        // FluxDM patch: a new limiter starts with a full bucket, so re-setting the same value (the app
        // does it every second) must not rebuild it, or the limit leaks a burst each time.
        if self.get() == limit {
            return;
        }
        let new = Self::new_inner(limit);
        self.limiter.swap(new);
        self.current_bps
            .store(limit.map(|v| v.get()).unwrap_or(0), Ordering::Relaxed);
    }

    fn get(&self) -> Option<NonZeroU32> {
        use std::sync::atomic::Ordering;
        NonZeroU32::new(self.current_bps.load(Ordering::Relaxed))
    }
}

pub struct Limits {
    down: Limit,
    up: Limit,
    /// FluxDM patch: an upload limit of 1 B/s means "no upload at all".
    up_blocked: std::sync::atomic::AtomicBool,
}

/// FluxDM patch: the upload limit that means "don't upload". A real 1 B/s limiter can never pass a 16 KB block
/// (governor refuses it, which ended the upload task), so callers hold every request back instead.
fn is_block(bps: Option<NonZeroU32>) -> bool {
    bps.is_some_and(|b| b.get() == 1)
}

impl Limits {
    pub fn new(config: LimitsConfig) -> Self {
        let blocked = is_block(config.upload_bps);
        Self {
            down: Limit::new(config.download_bps),
            up: Limit::new(if blocked { None } else { config.upload_bps }),
            up_blocked: std::sync::atomic::AtomicBool::new(blocked),
        }
    }

    /// FluxDM patch: true while uploads are switched off (upload limit 1 B/s).
    pub fn upload_blocked(&self) -> bool {
        self.up_blocked.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub async fn prepare_for_upload(&self, len: NonZeroU32) -> crate::Result<()> {
        self.up.acquire(len).await
    }

    pub async fn prepare_for_download(&self, len: NonZeroU32) -> crate::Result<()> {
        self.down.acquire(len).await
    }

    pub fn set_upload_bps(&self, bps: Option<NonZeroU32>) {
        // FluxDM patch: "no upload" is a flag, not a limiter (see `is_block`).
        let blocked = is_block(bps);
        self.up_blocked.store(blocked, std::sync::atomic::Ordering::Relaxed);
        self.up.set(if blocked { None } else { bps });
    }

    pub fn set_download_bps(&self, bps: Option<NonZeroU32>) {
        self.down.set(bps);
    }

    pub fn get_upload_bps(&self) -> Option<NonZeroU32> {
        if self.upload_blocked() {
            return NonZeroU32::new(1);
        }
        self.up.get()
    }

    pub fn get_download_bps(&self) -> Option<NonZeroU32> {
        self.down.get()
    }

    pub fn get_config(&self) -> LimitsConfig {
        LimitsConfig {
            upload_bps: self.get_upload_bps(),
            download_bps: self.get_download_bps(),
        }
    }
}

/// FluxDM patch tests.
#[cfg(test)]
mod fluxdm_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn slow_limits_still_pass_whole_blocks() {
        // 8 KB/s used to fail outright (a 16 KB block is more than one second's bucket).
        let l = Limits::new(LimitsConfig { upload_bps: NonZeroU32::new(8192), download_bps: NonZeroU32::new(8192) });
        let block = NonZeroU32::new(16384).unwrap();
        let start = Instant::now();
        for _ in 0..3 {
            l.prepare_for_upload(block).await.unwrap();
        }
        // 48 KB through a 32 KB bucket refilling at 8 KB/s: about 2 s, so the average rate holds.
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(1800) && took < Duration::from_secs(4), "{took:?}");
        l.prepare_for_download(block).await.unwrap();
    }

    #[test]
    fn one_byte_per_second_means_no_upload() {
        let l = Limits::new(LimitsConfig { upload_bps: NonZeroU32::new(1), download_bps: None });
        assert!(l.upload_blocked());
        assert_eq!(l.get_upload_bps(), NonZeroU32::new(1));
        l.set_upload_bps(NonZeroU32::new(50 * 1024));
        assert!(!l.upload_blocked());
        assert_eq!(l.get_upload_bps(), NonZeroU32::new(50 * 1024));
        l.set_upload_bps(NonZeroU32::new(1));
        assert!(l.upload_blocked());
        l.set_upload_bps(None);
        assert!(!l.upload_blocked());
        assert_eq!(l.get_upload_bps(), None);
    }
}
