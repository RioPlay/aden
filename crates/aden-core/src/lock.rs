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

/// A lockfile older than this is presumed stale on platforms without a
/// process-liveness check. On Linux, a live holder is never reclaimed solely
/// because it is old.
pub const STALE_TTL: Duration = Duration::from_secs(15 * 60);

/// Holder identity recorded in a lockfile (`pid` on line 1, unix secs on line 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockHolder {
    pub pid: u32,
    pub acquired_secs: u64,
}

/// Sibling lockfile path for a store directory (`store` → `store.lock`).
pub fn store_lock_path(store_path: &Path) -> PathBuf {
    store_path.with_extension("lock")
}

/// Read the live holder from an existing lockfile, if parseable.
pub fn read_holder(lock_path: &Path) -> Option<LockHolder> {
    parse_holder(&read_lock_body(lock_path)?)
}

/// Human-readable holder summary for status / wait messages.
pub fn describe_holder(holder: LockHolder) -> String {
    let age = now_secs().saturating_sub(holder.acquired_secs);
    format!("pid {} (held {}s)", holder.pid, age)
}

/// A held advisory lock. Dropping it releases the lock by removing the lockfile.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
    token: String,
}

impl FileLock {
    /// Acquire the lock at `path`, waiting up to `timeout` for a current holder
    /// to release it. Reclaims a dead holder on Linux and a lock older than
    /// [`STALE_TTL`] only where process liveness is unavailable. Returns
    /// [`io::ErrorKind::WouldBlock`] if a live holder keeps the lock for the
    /// whole `timeout`.
    ///
    /// The parent directory must already exist. Each guard retains an ownership
    /// token and removes the lockfile only when that token still matches, so a
    /// delayed drop cannot release a replacement holder's lock.
    pub fn acquire_timeout(path: impl AsRef<Path>, timeout: Duration) -> io::Result<Self> {
        Self::acquire_timeout_inner(path, timeout, false)
    }

    /// Like [`acquire_timeout`](Self::acquire_timeout), but emits a `NOTE:` line
    /// to stderr every 5s while waiting so long-running `gen`/`ready` queues do
    /// not look hung (ADR-011 Phase 2).
    pub fn acquire_timeout_verbose(path: impl AsRef<Path>, timeout: Duration) -> io::Result<Self> {
        Self::acquire_timeout_inner(path, timeout, true)
    }

    fn acquire_timeout_inner(
        path: impl AsRef<Path>,
        timeout: Duration,
        verbose: bool,
    ) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let deadline = SystemTime::now() + timeout;
        let started = SystemTime::now();
        let mut last_note = started;
        const NOTE_EVERY: Duration = Duration::from_secs(5);

        loop {
            match create_exclusive(&path) {
                Ok(token) => return Ok(FileLock { path, token }),
                Err(e)
                    if e.kind() == io::ErrorKind::AlreadyExists
                        || e.kind() == io::ErrorKind::PermissionDenied =>
                {
                    if reclaim_if_stale(&path)? {
                        continue;
                    }
                    let now = SystemTime::now();
                    if verbose && now.duration_since(last_note).unwrap_or_default() >= NOTE_EVERY {
                        let waited = now.duration_since(started).unwrap_or_default();
                        let detail = read_holder(&path)
                            .map(describe_holder)
                            .unwrap_or_else(|| "unknown holder".into());
                        eprintln!(
                            "NOTE: waiting for store writer lock at {} ({detail}; waited {}s)…",
                            path.display(),
                            waited.as_secs()
                        );
                        last_note = now;
                    }
                    if now >= deadline {
                        let detail = read_holder(&path)
                            .map(describe_holder)
                            .unwrap_or_else(|| "unknown holder".into());
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            format!(
                                "store locked — another aden process holds the lock at {} \
                                 ({detail}; waited {:?})",
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
/// lockfile is never visible without its holder identity and ownership token.
/// A plain `create_new` would expose an empty file between create and write,
/// which a contender would misread as stale and reclaim — letting two writers
/// in at once.
fn create_exclusive(path: &Path) -> io::Result<String> {
    let tmp = unique_temp(path);
    let token = unique_token();
    {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        writeln!(f, "{}", std::process::id())?;
        writeln!(f, "{}", now_secs())?;
        write!(f, "{token}")?;
        f.sync_all()?;
    }
    let link_res = fs::hard_link(&tmp, path);
    // The temp's only purpose was to carry content into the atomic link; drop it
    // either way (on success the link keeps the inode; on failure it is garbage).
    let _ = fs::remove_file(&tmp);
    match link_res {
        Ok(()) => Ok(token),
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
    let n = next_unique_counter();
    let suffix = format!(".tmp.{}.{}", std::process::id(), n);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

fn unique_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}-{}", std::process::id(), next_unique_counter())
}

fn next_unique_counter() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if lock_token(&self.path).as_deref() == Some(self.token.as_str()) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Seconds since the Unix epoch, saturating at 0 before it.
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// If the lockfile at `path` is stale (dead holder on Linux, or past
/// [`STALE_TTL`] where liveness is unavailable), remove it and return `Ok(true)`.
/// A lockfile that vanished underneath us (the holder released it) also returns
/// `Ok(true)`.
fn read_lock_body(path: &Path) -> Option<String> {
    let mut buf = String::new();
    File::open(path).ok()?.read_to_string(&mut buf).ok()?;
    Some(buf)
}

fn parse_holder(buf: &str) -> Option<LockHolder> {
    let mut lines = buf.lines();
    let pid = lines.next()?.trim().parse().ok()?;
    let acquired_secs = lines.next()?.trim().parse().ok()?;
    Some(LockHolder { pid, acquired_secs })
}

fn lock_token(path: &Path) -> Option<String> {
    read_lock_body(path)?.lines().nth(2).map(str::to_owned)
}

fn reclaim_if_stale(path: &Path) -> io::Result<bool> {
    let buf = match read_lock_body(path) {
        Some(b) => b,
        None => {
            // On Windows a transient PermissionDenied on open can occur while a
            // prior holder is dropping the lockfile. Treat as "vanished".
            return Ok(true);
        }
    };

    let stale = match parse_holder(&buf) {
        None => true,
        Some(holder) => holder_is_stale(holder),
    };

    if stale {
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

fn holder_is_stale(holder: LockHolder) -> bool {
    #[cfg(target_os = "linux")]
    {
        !Path::new(&format!("/proc/{}", holder.pid)).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        now_secs().saturating_sub(holder.acquired_secs) > STALE_TTL.as_secs()
    }
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
    fn read_holder_parses_lockfile() {
        let path = lock_path("parse");
        fs::write(&path, "4242\n1700000000\n").unwrap();
        let h = read_holder(&path).expect("must parse");
        assert_eq!(h.pid, 4242);
        assert_eq!(h.acquired_secs, 1_700_000_000);
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
            let g = FileLock::acquire_timeout(&path, Duration::from_secs(5)).unwrap();
            // Verify the lock file exists while held
            assert!(path.exists(), "lock file must exist while held");
            drop(g);
        }
        // After the guard drops, the lock file may remain (stale) but the
        // exclusive hold is released: a fresh acquire succeeds immediately.
        let g2 = FileLock::acquire_timeout(&path, Duration::from_millis(100))
            .expect("lock must be acquirable after guard drops");
        // Clean up
        drop(g2);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn drop_does_not_release_a_replacement_lock() {
        let path = lock_path("replacement");
        let original = FileLock::acquire_timeout(&path, Duration::from_secs(5)).unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(
            &path,
            format!("{}\n{}\nreplacement", std::process::id(), now_secs()),
        )
        .unwrap();
        drop(original);
        assert!(
            path.exists(),
            "original guard must not remove replacement lock"
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn stale_lock_is_reclaimed() {
        let path = lock_path("stale");
        // PID 0 is dead on Linux; off Linux the ancient timestamp exceeds TTL.
        fs::write(&path, "0\n0\nexpired").unwrap();
        assert!(path.exists(), "stale lock file must exist before reclaim");
        let g = FileLock::acquire_timeout(&path, Duration::from_millis(200))
            .expect("stale lock must be reclaimed");
        // After reclaim, the lock file is re-written with our PID and
        // the acquired guard must be functional (can't be re-acquired).
        let err = FileLock::acquire_timeout(&path, Duration::from_millis(100))
            .expect_err("reclaimed lock must block second acquire");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        drop(g);
        let _ = fs::remove_file(&path);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn aged_live_holder_is_not_reclaimed() {
        let path = lock_path("live-aged");
        fs::write(&path, format!("{}\n0\nlive", std::process::id())).unwrap();
        let err = FileLock::acquire_timeout(&path, Duration::from_millis(100))
            .expect_err("a live holder must not be reclaimed solely by age");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        fs::remove_file(path).unwrap();
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
            // After reclaim the lock must block a second acquire
            let err = FileLock::acquire_timeout(&path, Duration::from_millis(100))
                .expect_err("reclaimed lock must block");
            assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        } else {
            // Off Linux, liveness is unknown; the TTL governs instead.
            // At minimum verify the acquire doesn't panic.
            if let Ok(g) = acquired {
                drop(g);
            }
        }
        let _ = fs::remove_file(&path);
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
