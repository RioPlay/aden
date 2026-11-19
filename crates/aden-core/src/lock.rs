// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Persistent OS advisory locks. Version 2 deliberately fails closed in older Aden.
//! Never remove a version 2 lockfile: replacing its inode can admit two writers.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const POLL: Duration = Duration::from_millis(50);
const MARKER: &str = "aden-os-lock-v2\n";
const RECORD_LIMIT: u64 = 4096;
static OWNER_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockHolder {
    pub pid: u32,
    pub acquired_secs: u64,
}

pub fn store_lock_path(store_path: &Path) -> PathBuf {
    store_path.with_extension("lock")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockState {
    Missing,
    Active(LockHolder),
    Stale(LockHolder),
    Malformed,
    Unreadable(io::ErrorKind),
}

fn owner_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".owner");
    PathBuf::from(name)
}

fn read_record(path: &Path) -> io::Result<String> {
    let mut body = String::new();
    File::open(path)?
        .take(RECORD_LIMIT + 1)
        .read_to_string(&mut body)?;
    if body.len() as u64 > RECORD_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "lock metadata exceeds limit",
        ));
    }
    Ok(body)
}

fn publish_owner(path: &Path) -> io::Result<()> {
    let owner = owner_path(path);
    let sequence = OWNER_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = owner.with_extension(format!(
        "owner.{}.{timestamp}.{sequence}.pending",
        std::process::id()
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        write!(file, "{}\n{}\n", std::process::id(), now_secs())?;
        file.sync_all()?;
        drop(file);
        // Replace the directory entry, never truncate through an existing link.
        fs::rename(&temporary, &owner)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Legacy PID records remain supported, including metadata-only owner records.
pub fn inspect_lock(path: &Path) -> LockState {
    let body = match read_record(path) {
        Ok(body) => body,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return LockState::Missing,
        // Windows byte-range locking can prevent reading the held marker file.
        // Confirm the actual OS lease before using diagnostic owner metadata.
        Err(error) => {
            return match (
                lease_is_held(path),
                read_record(&owner_path(path))
                    .ok()
                    .and_then(|body| parse_holder(&body)),
            ) {
                (Ok(true), Some(holder)) => LockState::Active(holder),
                _ => LockState::Unreadable(error.kind()),
            };
        }
    };
    if MARKER.starts_with(&body) {
        let holder = read_record(&owner_path(path))
            .ok()
            .and_then(|s| parse_holder(&s));
        return match (lease_is_held(path), holder) {
            (Ok(true), Some(h)) => LockState::Active(h),
            (Ok(false), _) => LockState::Missing,
            (Ok(true), None) => LockState::Malformed,
            (Err(e), _) => LockState::Unreadable(e.kind()),
        };
    }
    match parse_holder(&body) {
        Some(h) if holder_is_stale(h) => LockState::Stale(h),
        Some(h) => LockState::Active(h),
        None => LockState::Malformed,
    }
}

pub fn read_holder(path: &Path) -> Option<LockHolder> {
    match inspect_lock(path) {
        LockState::Active(h) | LockState::Stale(h) => Some(h),
        _ => None,
    }
}
pub fn describe_holder(h: LockHolder) -> String {
    format!(
        "pid {} (held {}s)",
        h.pid,
        now_secs().saturating_sub(h.acquired_secs)
    )
}
pub fn describe_lock_state(state: LockState) -> String {
    match state {
        LockState::Missing => "no holder".into(),
        LockState::Active(h) => describe_holder(h),
        LockState::Stale(h) => format!("stale {}", describe_holder(h)),
        LockState::Malformed => "malformed lockfile".into(),
        LockState::Unreadable(k) => format!("unreadable lockfile ({k:?})"),
    }
}

/// A held OS lease. Closing the handle releases it, including after process death.
/// The marker inode and diagnostic owner record persist after release.
#[derive(Debug)]
pub struct FileLock {
    _file: File,
}

impl FileLock {
    pub fn acquire_timeout(path: impl AsRef<Path>, timeout: Duration) -> io::Result<Self> {
        Self::acquire(path.as_ref(), timeout, false)
    }
    pub fn acquire_timeout_verbose(path: impl AsRef<Path>, timeout: Duration) -> io::Result<Self> {
        Self::acquire(path.as_ref(), timeout, true)
    }
    fn acquire(path: &Path, timeout: Duration, verbose: bool) -> io::Result<Self> {
        let start = Instant::now();
        let mut note = start;
        loop {
            match try_acquire(path) {
                Ok(file) => return Ok(Self { _file: file }),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
            if start.elapsed() >= timeout {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!(
                        "store locked at {} ({}; waited {timeout:?})",
                        path.display(),
                        describe_lock_state(inspect_lock(path))
                    ),
                ));
            }
            if verbose && note.elapsed() >= Duration::from_secs(5) {
                eprintln!(
                    "NOTE: waiting for store writer lock at {} ({})",
                    path.display(),
                    describe_lock_state(inspect_lock(path))
                );
                note = Instant::now();
            }
            sleep(POLL.min(timeout.saturating_sub(start.elapsed())));
        }
    }
}

fn lease_is_held(path: &Path) -> io::Result<bool> {
    let file = open_existing(path)?;
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

fn open_existing(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    // Prevent deletion or replacement of an inode while any new holder has it open.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(3); // FILE_SHARE_READ | FILE_SHARE_WRITE, no DELETE
    }
    options.open(path)
}

fn try_acquire(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(3);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(path)?.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "lockfile must not be a symbolic link",
                ));
            }
            if path.is_dir() {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            open_existing(path)?
        }
        Err(e) => return Err(e),
    };
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Err(io::Error::from(io::ErrorKind::WouldBlock)),
        Err(TryLockError::Error(e)) => return Err(e),
    }
    file.rewind()?;
    let mut body = String::new();
    Read::by_ref(&mut file)
        .take(RECORD_LIMIT + 1)
        .read_to_string(&mut body)?;
    if body.len() as u64 > RECORD_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "lock metadata exceeds limit",
        ));
    }
    if body != MARKER && MARKER.starts_with(&body) {
        // A creator can exit before its initial marker is fully written. Old
        // PID-based writers never publish these bytes and fail closed on them.
        // Initialize or repair only this recognizable state under the OS lease.
        file.rewind()?;
        file.set_len(0)?;
        file.write_all(MARKER.as_bytes())?;
        file.sync_all()?;
        body = MARKER.to_owned();
    }
    if body != MARKER {
        // Old binaries do not honor OS leases and can race stale-file deletion.
        // Never convert such an inode while an old reclaimer could replace it.
        if parse_holder(&body).is_some_and(holder_is_stale) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "legacy stale lock at {}; stop all older Aden processes, remove this legacy lockfile, then retry",
                    path.display()
                ),
            ));
        }
        return Err(io::Error::from(io::ErrorKind::WouldBlock));
    }
    publish_owner(path)?;
    Ok(file)
}
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
fn parse_holder(body: &str) -> Option<LockHolder> {
    let mut lines = body.lines();
    Some(LockHolder {
        pid: lines.next()?.trim().parse().ok()?,
        acquired_secs: lines.next()?.trim().parse().ok()?,
    })
}

fn holder_is_stale(holder: LockHolder) -> bool {
    !process_is_alive(holder.pid)
}

#[cfg(target_os = "linux")]
fn process_is_alive(pid: u32) -> bool {
    pid != 0 && Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return false;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // Access can be denied for a live protected process. Only an invalid
            // PID is proof that no process exists.
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let mut exit_code = 0;
        let queried = GetExitCodeProcess(handle, &mut exit_code);
        let _ = CloseHandle(handle);
        queried == 0 || exit_code == STILL_ACTIVE as u32
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(any(unix, windows)))]
fn process_is_alive(pid: u32) -> bool {
    // On an unsupported platform, uncertainty must preserve mutual exclusion.
    pid != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_v2_initialization_recovers_under_exclusive_lease() {
        let temp = tempfile::tempdir().unwrap();
        for (index, prefix) in ["", "aden-", "aden-os-lock-v2"].iter().enumerate() {
            let path = temp.path().join(format!("partial-{index}.lock"));
            fs::write(&path, prefix).unwrap();
            assert_eq!(inspect_lock(&path), LockState::Missing);
            let held = FileLock::acquire_timeout(&path, Duration::ZERO).unwrap();
            assert!(FileLock::acquire_timeout(&path, Duration::ZERO).is_err());
            drop(held);
            assert_eq!(fs::read_to_string(&path).unwrap(), MARKER);
        }
    }
    #[test]
    fn publishing_owner_does_not_overwrite_an_existing_link_target() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("store.lock");
        let valuable = temp.path().join("valuable.txt");
        fs::write(&valuable, "original content").unwrap();
        fs::hard_link(&valuable, owner_path(&path)).unwrap();
        let held = FileLock::acquire_timeout(&path, Duration::ZERO).unwrap();
        assert_eq!(fs::read_to_string(&valuable).unwrap(), "original content");
        assert!(matches!(inspect_lock(&path), LockState::Active(_)));
        drop(held);
    }
    #[test]
    fn lease_blocks_then_recovers_without_removing_inode() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("store.lock");
        let guard = FileLock::acquire_timeout(&path, Duration::ZERO).unwrap();
        assert_eq!(
            FileLock::acquire_timeout(&path, Duration::ZERO)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(matches!(inspect_lock(&path), LockState::Active(_)));
        drop(guard);
        assert_eq!(fs::read_to_string(&path).unwrap(), MARKER);
        assert!(
            parse_holder(MARKER).is_none(),
            "old PID lock readers must fail closed"
        );
        assert_eq!(inspect_lock(&path), LockState::Missing);
        let _next = FileLock::acquire_timeout(&path, Duration::ZERO).unwrap();
    }
    #[test]
    fn legacy_live_and_malformed_fail_closed_stale_requires_explicit_recovery() {
        let temp = tempfile::tempdir().unwrap();
        for (name, body, kind) in [
            (
                "live",
                format!("{}\n0\nold-token", std::process::id()),
                io::ErrorKind::WouldBlock,
            ),
            ("malformed", "unknown".into(), io::ErrorKind::WouldBlock),
            (
                "stale",
                "0\n0\nold-token".into(),
                io::ErrorKind::InvalidData,
            ),
        ] {
            let path = temp.path().join(name);
            fs::write(&path, &body).unwrap();
            assert_eq!(
                FileLock::acquire_timeout(&path, Duration::ZERO)
                    .unwrap_err()
                    .kind(),
                kind
            );
            assert_eq!(fs::read_to_string(path).unwrap(), body);
        }
    }
    #[test]
    fn metadata_only_records_remain_inspectable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("launch.pid");
        fs::write(&path, format!("{}\n0\njob", std::process::id())).unwrap();
        assert!(matches!(inspect_lock(&path), LockState::Active(_)));
        fs::write(&path, "0\n0\njob").unwrap();
        assert!(matches!(inspect_lock(&path), LockState::Stale(_)));
    }
    #[test]
    fn threads_are_mutually_exclusive() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("store.lock");
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let threads: Vec<_> = (0..6)
            .map(|_| {
                let path = path.clone();
                let count = count.clone();
                std::thread::spawn(move || {
                    for _ in 0..10 {
                        let _guard =
                            FileLock::acquire_timeout(&path, Duration::from_secs(10)).unwrap();
                        assert_eq!(count.fetch_add(1, std::sync::atomic::Ordering::SeqCst), 0);
                        std::thread::yield_now();
                        assert_eq!(count.fetch_sub(1, std::sync::atomic::Ordering::SeqCst), 1);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    }
}
