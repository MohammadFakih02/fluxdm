//! Small shared pieces: the token-bucket speed limiter and checksum verification.

use parking_lot::Mutex;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

/// Token bucket shared by every connection it throttles. The limit can change while running.
pub struct Limiter {
    bps: AtomicI64,
    state: Mutex<(f64, Instant)>,
}

impl Default for Limiter {
    fn default() -> Self {
        Self { bps: AtomicI64::new(0), state: Mutex::new((0.0, Instant::now())) }
    }
}

impl Limiter {
    pub fn get(&self) -> i64 {
        self.bps.load(Ordering::Relaxed)
    }
    pub fn set(&self, bps: i64) {
        self.bps.store(bps.max(0), Ordering::Relaxed);
    }
    /// How long to wait before `n` more bytes may pass (zero when unlimited).
    pub fn take(&self, n: usize) -> Duration {
        let rate = self.get();
        if rate <= 0 {
            return Duration::ZERO;
        }
        let rate = rate as f64;
        let mut st = self.state.lock();
        let now = Instant::now();
        // Only a small burst may build up while idle (a quarter second let the speed spike well over the limit
        // each time connections woke together); one network chunk's worth is enough to keep the pipe full.
        st.0 = (st.0 + now.duration_since(st.1).as_secs_f64() * rate).min((rate * 0.04).max(16384.0));
        st.1 = now;
        st.0 -= n as f64;
        if st.0 < 0.0 {
            let w = -st.0 / rate;
            if w > 0.002 {
                return Duration::from_secs_f64(w);
            }
        }
        Duration::ZERO
    }
}

// ------------------------------------------------------------------ checksums

#[derive(Clone, Copy, Debug)]
pub enum Algo {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

pub fn algo_for(hex: &str) -> Option<Algo> {
    if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        32 => Some(Algo::Md5),
        40 => Some(Algo::Sha1),
        64 => Some(Algo::Sha256),
        128 => Some(Algo::Sha512),
        _ => None,
    }
}

/// Hashes a file, reporting progress (0–100). Blocking — run it on a blocking thread.
pub fn hash_file(path: &str, algo: Algo, progress: impl Fn(f64), cancel: &AtomicBool) -> std::io::Result<String> {
    use sha2::Digest;
    enum H {
        Md5(md5::Md5),
        Sha1(sha1::Sha1),
        Sha256(sha2::Sha256),
        Sha512(sha2::Sha512),
    }
    let mut h = match algo {
        Algo::Md5 => H::Md5(md5::Md5::new()),
        Algo::Sha1 => H::Sha1(sha1::Sha1::new()),
        Algo::Sha256 => H::Sha256(sha2::Sha256::new()),
        Algo::Sha512 => H::Sha512(sha2::Sha512::new()),
    };
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut buf = vec![0u8; 4 << 20];
    let (mut done, mut last) = (0u64, -1.0);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        match &mut h {
            H::Md5(x) => x.update(&buf[..n]),
            H::Sha1(x) => x.update(&buf[..n]),
            H::Sha256(x) => x.update(&buf[..n]),
            H::Sha512(x) => x.update(&buf[..n]),
        }
        done += n as u64;
        let pct = if len > 0 { done as f64 * 100.0 / len as f64 } else { 100.0 };
        if pct - last >= 1.0 {
            last = pct;
            progress(pct);
        }
    }
    Ok(match h {
        H::Md5(x) => hex::encode(x.finalize()),
        H::Sha1(x) => hex::encode(x.finalize()),
        H::Sha256(x) => hex::encode(x.finalize()),
        H::Sha512(x) => hex::encode(x.finalize()),
    })
}

pub fn random_pin() -> String {
    let mut b = [0u8; 4];
    let _ = getrandom::fill(&mut b);
    format!("{:06}", u32::from_le_bytes(b) % 1_000_000)
}

pub fn log_error(msg: &str) {
    use std::io::Write;
    let path = crate::store::data_dir().join("error.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {msg}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_waits() {
        let l = Limiter::default();
        assert_eq!(l.take(1_000_000), Duration::ZERO);
        l.set(1000);
        let w = l.take(2000);
        assert!(w.as_secs_f64() > 1.5 && w.as_secs_f64() < 2.5, "{w:?}");
    }

    #[test]
    fn algos() {
        assert!(matches!(algo_for(&"a".repeat(64)), Some(Algo::Sha256)));
        assert!(algo_for("xyz").is_none());
    }
}
