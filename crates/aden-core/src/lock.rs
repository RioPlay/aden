// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cross-process advisory locking via atomic lockfiles (no external deps).
//!
//! aden coordinates concurrent writers — parallel-agent `gen` against one store,
//! and concurrent `.agent/session.adoc` appends — with an advisory lock built on
//! an atomically created lockfile. The lock is *advisory*: it only constrains
//! processes that go through [`FileLock`].
//!
//! Robustness and its limit: a crash can leave a stale lockfile behind. The
//! holder records its PID and an acquisition timestamp, and a contender reclaims
//! the lock when the holder is no longer alive (checked via `/proc/<pid>` on
//! Linux) or when the lockfile is older than [`STALE_TTL`] (the portable
//! fallback). A real `flock(2)` would auto-release on process death and close
//! the small stale-reclaim TOCTOU window noted on [`FileLock::acquire_timeout`];
//! the dependency-free lockfile trades that for zero new dependencies, which is
//! aden's standing preference.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Poll interval while waiting for a contended lock.
const POLL: Duration = Duration::from_millis(50);

/// A lockfile older than this is presumed stale (its holder crashed) on
/// platforms without a process-liveness check. Generous so a long but healthy
/// `gen` is never reclaimed out from under itself.
pub const STALE_TTL: Duration = Duration::from_secs(15 * 60);

/// A held advisory lock. Dropping it releases the lock by removing the lockfile.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
}

impl FileLock {
    /// Acquire the lock at `path`, waiting up to `timeout` for a current holder
    /// to release it. Reclaims a stale lock (dead holder, or older than
    /// [`STALE_TTL`]). Returns [`io::ErrorKind::WouldBlock`] if a live holder
    /// keeps the lock for the whole `timeout`.
    ///
    /// The parent directory must already exist. The stale-reclaim path has a
    /// small inherent TOCTOU window (a contender can, in the rare crash-recovery
    /// case, remove a lockfile a third party just recreated); the TTL is kept
    /// generous to make false-stale reclaim during a healthy write effectively
    /// impossible.
    pub fn acquire_timeout(path: impl AsRef<Path>, timeout: Duration) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let deadline = SystemTime::now() + timeout;
        loop {
            match create_exclusive(&path) {
                Ok(()) => return Ok(FileLock { path }),
                Err(e)
                    if e.kind() == io::ErrorKind::AlreadyExists
                        || e.kind() == io::ErrorKind::PermissionDenied =>
                {
                    // PermissionDenied is treated as contention on Windows:
                    // hard_link or create_new(tmp) can legitimately return
                    // Access Denied (code 5) when racing for an existing target
                    // instead of the Unix-y AlreadyExists. Reclaim or poll.
                    if reclaim_if_stale(&path)? {
                        continue;
                    }
                    if SystemTime::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!(
                                "store locked — another aden process holds the lock at {} \
                                 (waited {:?})",
                                path.display(),
                                timeout
                            ),
                        ));
                    }
                    sleep(POLL);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// Atomically create the lockfile at `path` already populated with this
/// process's identity. Errors with [`io::ErrorKind::AlreadyExists`] if a lock is
/// held.
///
/// The identity is written to a unique temp file and then `hard_link`ed into
/// place: `link(2)` fails if the target exists, so creation is atomic AND the
/// lockfile is never visible without its PID/timestamp. A plain `create_new`
/// would expose an empty file between create and write, which a contender would
/// misread as stale and reclaim — letting two writers in at once.
fn create_exclusive(path: &Path) -> io::Result<()> {
    let tmp = unique_temp(path);
    {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        writeln!(f, "{}", std::process::id())?;
        write!(f, "{}", now_secs())?;
        f.sync_all()?;
    }
    let link_res = fs::hard_link(&tmp, path);
    // The temp's only purpose was to carry content into the atomic link; drop it
    // either way (on success the link keeps the inode; on failure it is garbage).
    let _ = fs::remove_file(&tmp);
    match link_res {
        Ok(()) => Ok(()),
        Err(e) if path.exists() => {
            // Normalize to AlreadyExists on any link failure *if* the target now
            // exists. This is required for Windows, where hard_link to an existing
            // dest can report PermissionDenied (ERROR_ACCESS_DENIED, code 5) rather
            // than AlreadyExists. acquire_timeout relies on AlreadyExists to treat
            // as contention + poll/reclaim.
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("lock target exists (hard_link reported {:?})", e.kind()),
            ))
        }
        Err(e) => Err(e),
    }
}

/// A collision-resistant sibling temp path for the link dance: PID plus a
/// monotonic counter keeps it unique across threads and calls without needing a
/// randomness dependency.
fn unique_temp(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let suffix = format!(".tmp.{}.{}", std::process::id(), n);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Seconds since the Unix epoch, saturating at 0 before it.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// If the lockfile at `path` is stale (holder dead or past [`STALE_TTL`]), remove
/// it and return `Ok(true)`. A lockfile that vanished underneath us (the holder
/// released it) also returns `Ok(true)`.
fn reclaim_if_stale(path: &Path) -> io::Result<bool> {
    let mut buf = String::new();
    match File::open(path) {
        Ok(mut f) => {
            let _ = f.read_to_string(&mut buf);
        }
        Err(e)
            if e.kind() == io::ErrorKind::NotFound
                || e.kind() == io::ErrorKind::PermissionDenied =>
        {
            // On Windows a transient PermissionDenied on open can occur while a
            // prior holder is dropping the lockfile. Treat as "vanished" so the
            // caller can immediately retry create_exclusive.
            return Ok(true);
        }
        Err(e) => return Err(e),
    }

    let mut lines = buf.lines();
    let pid = lines.next().and_then(|l| l.trim().parse::<u32>().ok());
    let acquired = lines.next().and_then(|l| l.trim().parse::<u64>().ok());

    // Unparseable identity (partial write, foreign file) counts as stale.
    let dead = pid.is_none_or(|pid| !process_alive(pid));
    let expired = acquired.is_none_or(|ts| now_secs().saturating_sub(ts) > STALE_TTL.as_secs());

    if dead || expired {
        match fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    || e.kind() == io::ErrorKind::PermissionDenied =>
            {
                // Same Windows tolerance: a racing remove or delete-pending file
                // can yield PermissionDenied; the lock is effectively released.
                Ok(true)
            }
            Err(e) => Err(e),
        }
    } else {
        Ok(false)
    }
}

/// Whether a process with `pid` is currently alive.
#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Without a dependency there is no portable liveness probe, so off Linux a lock
/// is only ever reclaimed via the [`STALE_TTL`] timeout.
#[cfg(not(target_os = "linux"))]
fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        // Unique per test process to avoid cross-run collisions.
        p.push(format!("aden-lock-test-{}-{}", std::process::id(), name));
        let _ = fs::remove_file(&p);
        p
    }

    #[test]
    fn second_acquire_blocks_while_held() {
        let path = lock_path("held");
        let held = FileLock::acquire_timeout(&path, Duration::from_secs(5)).unwrap();
        let err =
            FileLock::acquire_timeout(&path, Duration::from_millis(100)).expect_err("must block");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        drop(held);
    }

    #[test]
    fn drop_releases_the_lock() {
        let path = lock_path("release");
        {
            let _g = FileLock::acquire_timeout(&path, Duration::from_secs(5)).unwrap();
        }
        // After the guard drops, a fresh acquire succeeds immediately.
        let _g2 = FileLock::acquire_timeout(&path, Duration::from_millis(100)).unwrap();
    }

    #[test]
    fn stale_lock_is_reclaimed_by_ttl() {
        let path = lock_path("stale");
        // A lockfile from a plausibly-live PID but an ancient timestamp.
        fs::write(&path, format!("{}\n0", std::process::id())).unwrap();
        // Should be reclaimed via the TTL (timestamp 0 is far past STALE_TTL).
        let _g = FileLock::acquire_timeout(&path, Duration::from_millis(200))
            .expect("ancient lock must be reclaimed");
    }

    #[test]
    fn dead_holder_lock_is_reclaimed() {
        let path = lock_path("dead");
        // PID 0 never names a live process; recent timestamp so only liveness
        // (on Linux) can reclaim it.
        fs::write(&path, format!("0\n{}", now_secs())).unwrap();
        let acquired = FileLock::acquire_timeout(&path, Duration::from_millis(200));
        if cfg!(target_os = "linux") {
            assert!(acquired.is_ok(), "dead holder must be reclaimed on Linux");
        } else {
            // Off Linux, liveness is unknown; the TTL governs instead.
            let _ = acquired;
        }
    }

    #[test]
    fn mutual_exclusion_under_threads() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let path = Arc::new(lock_path("mutex"));
        let inside = Arc::new(AtomicBool::new(false));
        let overlaps = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let path = Arc::clone(&path);
            let inside = Arc::clone(&inside);
            let overlaps = Arc::clone(&overlaps);
            handles.push(std::thread::spawn(move || {
                for _ in 0..20 {
                    let g = FileLock::acquire_timeout(&*path, Duration::from_secs(10)).unwrap();
                    if inside.swap(true, Ordering::SeqCst) {
                        overlaps.fetch_add(1, Ordering::SeqCst);
                    }
                    // Hold the critical section briefly.
                    std::thread::yield_now();
                    inside.store(false, Ordering::SeqCst);
                    drop(g);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            overlaps.load(Ordering::SeqCst),
            0,
            "two holders were inside the lock at once"
        );
    }
}
