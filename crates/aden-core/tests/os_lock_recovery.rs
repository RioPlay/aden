// SPDX-License-Identifier: AGPL-3.0-or-later

use aden_core::lock::{FileLock, LockState, inspect_lock};
use std::{
    fs,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

struct OwnedChild(std::process::Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn lease_child() {
    let Some(path) = std::env::var_os("ADEN_TEST_LEASE_PATH") else {
        return;
    };
    let _lease = FileLock::acquire_timeout(std::path::Path::new(&path), Duration::ZERO).unwrap();
    fs::write(std::env::var_os("ADEN_TEST_LEASE_READY").unwrap(), "ready").unwrap();
    std::thread::sleep(Duration::from_secs(60));
}

#[test]
fn process_death_releases_lease_without_unlinking_marker() {
    let fixture = tempfile::tempdir().unwrap();
    let path = fixture.path().join("store.lock");
    let ready = fixture.path().join("ready");
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lease_child", "--nocapture"])
            .env("ADEN_TEST_LEASE_PATH", &path)
            .env("ADEN_TEST_LEASE_READY", &ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.exists() && Instant::now() < deadline {
        if child.0.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !ready.exists() {
        let _ = child.0.kill();
        let _ = child.0.wait();
        panic!("lease child failed to become ready");
    }
    assert!(matches!(inspect_lock(&path), LockState::Active(_)));
    let blocked = FileLock::acquire_timeout(&path, Duration::ZERO).unwrap_err();
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert_eq!(blocked.kind(), std::io::ErrorKind::WouldBlock);
    assert!(path.exists());
    assert_eq!(inspect_lock(&path), LockState::Missing);
    let _lease = FileLock::acquire_timeout(&path, Duration::from_secs(2)).unwrap();
}
