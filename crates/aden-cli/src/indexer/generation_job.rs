// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Durable read-triggered generation. Metadata never supplies executable paths
//! or a PID to terminate. Cancellation is cooperative and bound to a job token.
use aden_core::lock::{FileLock, LockState, inspect_lock};
use std::io;
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct JobStatus {
    pub schema_version: u8,
    pub job_id: String,
    pub root: PathBuf,
    pub state: String,
    pub phase: String,
    pub completed_files: usize,
    pub total_files: usize,
    pub started_ms: u64,
    pub updated_ms: u64,
    pub worker_pid: Option<u32>,
    pub error: Option<String>,
}
impl JobStatus {
    pub fn active(&self) -> bool {
        matches!(self.state.as_str(), "queued" | "running" | "cancelling")
    }
}
#[derive(Clone)]
pub(super) struct Paths {
    root: PathBuf,
    dir: PathBuf,
}
impl Paths {
    fn new(path: &Path) -> Self {
        let root = crate::util::find_project_root(if path.is_file() {
            path.parent().unwrap_or(path)
        } else {
            path
        });
        // Derive all mutation targets from the selected store, never job metadata.
        let store = aden_paths::store_dir(&root);
        Self::for_store(root, &store)
    }
    /// Derive job metadata paths from the selected store, including when a
    /// generator already holds its writer lease.
    pub(super) fn for_store(root: PathBuf, store: &Path) -> Self {
        Self {
            root,
            dir: store.with_extension("generation"),
        }
    }
    pub(super) fn clear_recovery(&self) -> Result<()> {
        match std::fs::remove_file(self.file("recovery.json")) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
    fn validate(&self) -> io::Result<()> {
        for name in [
            "",
            "status.json",
            "status.pending",
            "events.log",
            "cancel",
            "start.lock",
            "worker.lock",
            "launch.pid",
            "recovery.json",
            "recovery.pending",
        ] {
            let path = if name.is_empty() {
                self.dir.clone()
            } else {
                self.file(name)
            };
            match std::fs::symlink_metadata(path) {
                Ok(metadata) => {
                    #[cfg(windows)]
                    let redirected = {
                        use std::os::windows::fs::MetadataExt;
                        metadata.file_attributes() & 0x400 != 0
                    };
                    #[cfg(not(windows))]
                    let redirected = metadata.file_type().is_symlink();
                    if redirected
                        || (name.is_empty() && !metadata.is_dir())
                        || (!name.is_empty()
                            && (!metadata.is_file()
                                || metadata.len()
                                    > if name.starts_with("recovery.") {
                                        64 * 1024 * 1024
                                    } else {
                                        65536
                                    }))
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Unsafe generation metadata path or size",
                        ));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    fn lease_gate(&self) -> io::Result<FileLock> {
        self.validate()?;
        FileLock::acquire_timeout(self.file("start.lock"), Duration::from_secs(2))
    }
    fn gate(&self) -> io::Result<FileLock> {
        let guard = self.lease_gate()?;
        if aden_paths::store_dir(&self.root).with_extension("generation") != self.dir {
            return Err(io::Error::other(
                "Storage configuration changed while waiting; retry the command",
            ));
        }
        Ok(guard)
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn portable_relocation(root: &Path) -> Result<bool> {
    Ok(std::env::var_os("ADEN_DATA_DIR").is_none()
        && std::env::var_os("ADEN_STORE").is_none()
        && aden_paths::read_storage_config(root)?.is_some_and(|config| config.portable))
}
fn load(paths: &Paths) -> Result<Option<JobStatus>> {
    load_with_shared_policy(paths, std::env::var_os("ADEN_STORE").is_some())
}
fn load_with_shared_policy(paths: &Paths, shared: bool) -> Result<Option<JobStatus>> {
    let file = match std::fs::File::open(paths.file("status.json")) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(16385).read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err("generation status exceeds limit".into());
    }
    let mut status: JobStatus = serde_json::from_slice(&bytes)?;
    if status.schema_version != 1 || !valid_id(&status.job_id) {
        return Err("generation status identity mismatch".into());
    }
    if status.root != paths.root {
        if shared {
            return Ok(Some(status));
        }
        if !portable_relocation(&paths.root)? || live(paths, &status) {
            return Err("generation status belongs to another root or an active worker".into());
        }
        // Portable metadata is history after a move. Never join or cancel an
        // active worker at its old root, and never follow the recorded path.
        status.root = paths.root.clone();
        if status.active() {
            status.state = "interrupted".into();
            status.phase = "relocated".into();
        }
    }
    Ok(Some(status))
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}
static NEXT_PUBLICATION: AtomicU64 = AtomicU64::new(0);
/// Publish a new inode without ever truncating an existing named sidecar.
/// Rename replaces a hardlink directory entry while preserving its other names.
fn atomic_write(paths: &Paths, name: &str, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    for _ in 0..32 {
        let sequence = NEXT_PUBLICATION.fetch_add(1, Ordering::Relaxed);
        let tmp = paths.file(&format!(
            ".job-write-{}-{}-{sequence:x}.tmp",
            std::process::id(),
            now()
        ));
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let result = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&tmp, paths.file(name))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        return result;
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Could not allocate generation metadata temporary file",
    ))
}
fn save(paths: &Paths, status: &JobStatus) -> Result<()> {
    atomic_write(paths, "status.json", &serde_json::to_vec(status)?)?;
    Ok(())
}
fn worker_lease_busy(paths: &Paths) -> bool {
    matches!(
        inspect_lock(&paths.file("worker.lock")),
        LockState::Active(_) | LockState::Malformed | LockState::Unreadable(_)
    )
}
fn ensure_owned(paths: &Paths, status: &JobStatus) -> Result<()> {
    if status.root != paths.root {
        return Err("Generation job belongs to another project; cancellation refused".into());
    }
    Ok(())
}
/// Return true only when this caller can join its own active job. Foreign
/// terminal history may be replaced, but never an active lease or recovery work.
fn admit_start(paths: &Paths, status: &JobStatus, shared: bool) -> Result<bool> {
    if status.root == paths.root {
        if !status.active() && worker_lease_busy(paths) {
            return Err("Generation worker lease is still active; retry after it finishes".into());
        }
        return Ok(live(paths, status));
    }
    let terminal = matches!(
        status.state.as_str(),
        "completed" | "failed" | "cancelled" | "interrupted"
    );
    if !shared
        || !terminal
        || worker_lease_busy(paths)
        || paths.file("recovery.json").try_exists()?
    {
        return Err("Shared store has another project's active job or unreconciled recovery; finish that project before starting this one".into());
    }
    Ok(false)
}
fn ensure_recovery_owner(paths: &Paths) -> Result<()> {
    use std::io::Read;
    #[derive(serde::Deserialize)]
    struct Identity {
        root: PathBuf,
    }
    let file = match std::fs::File::open(paths.file("recovery.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let identity: Identity = serde_json::from_reader(file.take(64 * 1024 * 1024 + 1))?;
    if identity.root != paths.root && !portable_relocation(&paths.root)? {
        return Err("Shared store has another project's unreconciled recovery; finish that project before starting this one".into());
    }
    Ok(())
}
fn live(paths: &Paths, status: &JobStatus) -> bool {
    if !status.active() {
        return false;
    }
    if matches!(
        inspect_lock(&paths.file("worker.lock")),
        LockState::Active(_) | LockState::Malformed | LockState::Unreadable(_)
    ) {
        return true;
    }
    // PID metadata only bridges launch until the worker owns its real lease.
    // Once running, a missing OS lease proves interruption even if that PID has
    // since been reused by an unrelated process.
    status.phase == "launching"
        && (matches!(
            inspect_lock(&paths.file("launch.pid")),
            LockState::Active(_) | LockState::Malformed | LockState::Unreadable(_)
        ) || now().saturating_sub(status.started_ms) < 5000)
}
fn reconcile(paths: &Paths) -> Result<Option<JobStatus>> {
    let Some(mut s) = load(paths)? else {
        return Ok(None);
    };
    if s.root != paths.root {
        return Ok(Some(s));
    }
    if s.active() && !live(paths, &s) {
        // The last recorded phase is the only evidence of where a vanished
        // worker stopped; keep it instead of overwriting it with the state.
        let phase = s.phase.clone();
        s.state = "interrupted".into();
        s.error = Some(interrupted_error(paths, &s, &phase));
        s.updated_ms = now();
        save(paths, &s)?;
    }
    Ok(Some(s))
}

const WORKER_LOG_BYTES: usize = 32 * 1024;

/// Bounded worker log. A detached worker's stderr is discarded, so the log is
/// the only place a panic or shutdown reason can survive the process.
fn append_worker_log(paths: &Paths, line: &str) -> io::Result<()> {
    let mut bytes = std::fs::read(paths.file("worker.log")).unwrap_or_default();
    bytes.extend_from_slice(format!("{} {}\n", now(), line.trim_end()).as_bytes());
    if bytes.len() > WORKER_LOG_BYTES {
        let cut = bytes.len() - WORKER_LOG_BYTES / 2;
        bytes.drain(..cut);
    }
    atomic_write(paths, "worker.log", &bytes)
}

fn worker_log_tail(paths: &Paths, limit: usize) -> Option<String> {
    let bytes = std::fs::read(paths.file("worker.log")).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let start = text.len().saturating_sub(limit);
    let start = (start..=text.len())
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(text.len());
    Some(text[start..].to_string())
}

fn interrupted_error(paths: &Paths, status: &JobStatus, phase: &str) -> String {
    let pid = status
        .worker_pid
        .map_or_else(|| "worker".to_string(), |pid| format!("worker pid {pid}"));
    let mut message = format!(
        "Generation {pid} stopped during phase '{phase}' at {}/{} files without recording an exit, so it was killed or aborted before it could report a reason. Restart reuses the last completed cache and reconciles interrupted work.",
        status.completed_files, status.total_files
    );
    if let Some(tail) = worker_log_tail(paths, 1200) {
        message.push_str("\nworker.log tail:\n");
        message.push_str(&tail);
    }
    message
}

/// Record worker panics where a caller can read them. A detached worker has no
/// stderr, so without this a panic is indistinguishable from an external kill.
fn install_worker_panic_log(p: &Paths) {
    let paths = p.clone();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!(" at {}:{}", l.file(), l.line()))
            .unwrap_or_default();
        let _ = append_worker_log(
            &paths,
            &format!("panic{location}: {}", panic_detail(info.payload())),
        );
    }));
}

fn panic_detail(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".into())
}
pub fn status(path: &Path) -> Result<Option<JobStatus>> {
    let p = Paths::new(path);
    if !p.dir.exists() {
        return Ok(None);
    }
    let _gate = p.gate()?;
    reconcile(&p)
}
pub fn start(path: &Path) -> Result<JobStatus> {
    let p = Paths::new(path);
    aden_paths::guard_creatable_root(&p.root, crate::util::creation_explicit())?;
    std::fs::create_dir_all(&p.dir)?;
    let _gate = p.gate()?;
    ensure_recovery_owner(&p)?;
    if let Some(s) = reconcile(&p)?
        && admit_start(&p, &s, std::env::var_os("ADEN_STORE").is_some())?
    {
        return Ok(s);
    }
    let job_id = format!(
        "{:x}-{:x}-{:x}",
        now(),
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.subsec_nanos()
    );
    let mut s = JobStatus {
        schema_version: 1,
        job_id: job_id.clone(),
        root: p.root.clone(),
        state: "queued".into(),
        phase: "launching".into(),
        completed_files: 0,
        total_files: 0,
        started_ms: now(),
        updated_ms: now(),
        worker_pid: None,
        error: None,
    };
    let _ = std::fs::remove_file(p.file("launch.pid"));
    let _ = std::fs::remove_file(p.file("cancel"));
    save(&p, &s)?;
    match spawn_worker(&p.root, &job_id) {
        Ok(pid) => {
            s.worker_pid = Some(pid);
            atomic_write(
                &p,
                "launch.pid",
                format!("{}\n{}\n{}\n", pid, now() / 1000, job_id).as_bytes(),
            )?;
            save(&p, &s)?;
        }
        Err(e) => {
            s.state = "failed".into();
            s.error = Some(e.to_string());
            save(&p, &s)?;
            return Err(e.into());
        }
    }
    Ok(s)
}
/// Start or join generation and wait only for the caller's budget. A pending
/// result leaves the independently owned worker running and names its job.
pub fn wait_for_completion(path: &Path, budget: Duration) -> Result<JobStatus> {
    let began = std::time::Instant::now();
    let job = start(path)?;
    let status = wait(path, &job.job_id, budget.saturating_sub(began.elapsed()))?;
    if status.state == "completed" {
        return Ok(status);
    }
    if status.active() {
        return Err(format!("Generation job {} is still pending after {}s; the worker continues. Check `aden jobs status` and retry after completion.", status.job_id, budget.as_secs()).into());
    }
    Err(status
        .error
        .unwrap_or_else(|| format!("Generation job {} {}", status.job_id, status.state))
        .into())
}
/// Poll one resolved job without repeating root discovery or mutating metadata.
/// Publication is atomic, and active jobs block storage configuration changes.
pub(super) fn wait(path: &Path, job_id: &str, budget: Duration) -> Result<JobStatus> {
    let p = Paths::new(path);
    let deadline = std::time::Instant::now() + budget;
    loop {
        let status = load(&p)?.ok_or("Generation job disappeared; retry the command")?;
        if status.job_id != job_id {
            return Err("Generation job changed; retry the command".into());
        }
        if status.active() && !live(&p, &status) {
            let _gate = p.lease_gate()?;
            let current = reconcile(&p)?.ok_or("Generation job disappeared; retry the command")?;
            if current.job_id != job_id {
                return Err("Generation job changed; retry the command".into());
            }
            return Ok(current);
        }
        if !status.active() || std::time::Instant::now() >= deadline {
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
pub fn cancel(path: &Path, expected_job_id: &str) -> Result<JobStatus> {
    let p = Paths::new(path);
    let _gate = p.gate()?;
    let mut s = reconcile(&p)?.ok_or("No generation job")?;
    ensure_owned(&p, &s)?;
    if s.job_id != expected_job_id {
        return Err("Generation job changed; cancellation refused".into());
    }
    if s.active() {
        atomic_write(&p, "cancel", s.job_id.as_bytes())?;
        s.state = "cancelling".into();
        s.updated_ms = now();
        save(&p, &s)?;
    }
    Ok(s)
}
struct Context {
    paths: Paths,
    status: JobStatus,
}
static CONTEXT: OnceLock<Mutex<Option<Context>>> = OnceLock::new();
pub(super) fn is_worker() -> bool {
    CONTEXT
        .get()
        .is_some_and(|c| c.lock().unwrap_or_else(|e| e.into_inner()).is_some())
}
/// Generation calls this at safe stage boundaries. Never interrupts an atomic batch.
pub(super) fn checkpoint(
    phase: &str,
    completed: Option<usize>,
    total: Option<usize>,
) -> Result<()> {
    let mut context = CONTEXT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Some(c) = context.as_mut() else {
        return Ok(());
    };
    // Active jobs prevent configuration changes. Keep per-file checkpoints
    // local rather than repeatedly spawning root discovery processes.
    let _gate = c.paths.lease_gate()?;
    if std::fs::read(c.paths.file("cancel")).ok().as_deref() == Some(c.status.job_id.as_bytes()) {
        return Err("generation cancelled at safe checkpoint".into());
    }
    let changed = c.status.phase != phase;
    if let Some(v) = completed {
        c.status.completed_files = v;
    }
    if let Some(v) = total {
        c.status.total_files = v;
    }
    if !changed && now().saturating_sub(c.status.updated_ms) < 250 {
        return Ok(());
    }
    c.status.phase = phase.to_owned();
    c.status.updated_ms = now();
    if let Some(v) = completed {
        c.status.completed_files = v
    }
    if let Some(v) = total {
        c.status.total_files = v
    }
    save(&c.paths, &c.status)?;
    // The event log is a bounded ring, independent of uncontrolled stderr output.
    let log = c.paths.file("events.log");
    let mut bytes = std::fs::read(&log).unwrap_or_default();
    bytes.extend_from_slice(
        format!(
            "{} {} {}/{}\n",
            c.status.updated_ms, phase, c.status.completed_files, c.status.total_files
        )
        .as_bytes(),
    );
    if bytes.len() > 65536 {
        let cut = bytes.len() - 32768;
        bytes.drain(..cut);
    }
    atomic_write(&c.paths, "events.log", &bytes)?;
    Ok(())
}
pub fn run_worker(path: &Path, job_id: &str) -> Result<()> {
    if !valid_id(job_id) {
        return Err("Invalid generation job identity".into());
    }
    let p = Paths::new(path);
    p.validate()?;
    install_worker_panic_log(&p);
    let _ = append_worker_log(
        &p,
        &format!("started pid={} job={job_id}", std::process::id()),
    );
    let lease = FileLock::acquire_timeout(p.file("worker.lock"), Duration::ZERO)?;
    let mut s;
    {
        let _gate = p.gate()?;
        s = load(&p)?.ok_or("Missing generation job")?;
        if s.job_id != job_id || s.root != p.root {
            return Err("Superseded generation job or project identity mismatch".into());
        }
        s.worker_pid = Some(std::process::id());
        s.state = "running".into();
        s.phase = "starting".into();
        s.updated_ms = now();
        save(&p, &s)?;
    }
    *CONTEXT
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(Context {
        paths: p.clone(),
        status: s.clone(),
    });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        checkpoint("starting", None, None)?;
        #[cfg(debug_assertions)]
        if std::env::var_os("ADEN_TEST_GENERATION_HOLD").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            while p.root.join(".aden-test-generation-hold").exists() {
                checkpoint("test_held", None, None)?;
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        #[cfg(debug_assertions)]
        if let Ok(delay) = std::env::var("ADEN_TEST_GENERATION_DELAY_MS")
            && let Ok(ms) = delay.parse::<u64>()
        {
            for _ in 0..ms.min(30000).div_ceil(50) {
                std::thread::sleep(Duration::from_millis(50));
                checkpoint("test_delay", None, None)?;
            }
        }
        super::r#gen::cmd_gen_silent(path)?;
        checkpoint("verifying", None, None)?;
        if !aden_paths::graph_snapshot_file(&p.root).is_file()
            || super::fresh::index_is_stale_for_root(&p.root)
        {
            return Err("Generation finished without an authoritative current snapshot; sources may have changed or publication failed".into());
        }
        Ok(())
    }))
    .unwrap_or_else(|panic| {
        Err(format!("Generation worker panicked: {}", panic_detail(&*panic)).into())
    });
    let mut context = CONTEXT
        .get()
        .unwrap()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(c) = context.take() {
        s = c.status;
    }
    let _gate = p.gate()?;
    let cancelled = std::fs::read(p.file("cancel")).ok().as_deref() == Some(job_id.as_bytes());
    if result.is_ok() && !cancelled {
        s.completed_files = s.total_files;
    }
    s.state = if cancelled {
        "cancelled"
    } else if result.is_ok() {
        "completed"
    } else {
        "failed"
    }
    .into();
    s.phase = s.state.clone();
    s.updated_ms = now();
    s.error = result
        .as_ref()
        .err()
        .map(|e| e.to_string().chars().take(2048).collect());
    save(&p, &s)?;
    let _ = append_worker_log(
        &p,
        &format!(
            "exit state={} completed={}/{}{}",
            s.state,
            s.completed_files,
            s.total_files,
            s.error
                .as_deref()
                .map(|e| format!(" error={}", e.lines().next().unwrap_or_default()))
                .unwrap_or_default()
        ),
    );
    drop(lease);
    result
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Recovery {
    root: PathBuf,
    files: std::collections::BTreeMap<String, Vec<String>>,
}
/// Reparse interrupted files even if sources reverted to the last cached bytes.
/// Retaining every possibly committed anchor lets the normal prune pass remove
/// symbols introduced by an interrupted batch. This is reconciliation, not rollback.
/// Returns whether a validated journal contains owners requiring reconciliation.
pub(super) fn recover_cache(
    p: &Paths,
    cache: &mut crate::types::GenCache,
    storage: &aden_store::Storage,
) -> Result<bool> {
    use std::io::Read;
    p.validate()?;
    let file = match std::fs::File::open(p.file("recovery.json")) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(64 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("Generation recovery journal exceeds safety limit".into());
    }
    let recovery: Recovery = serde_json::from_slice(&bytes)?;
    if recovery.root != p.root && !portable_relocation(&p.root)? {
        return Err("Generation recovery identity mismatch".into());
    }
    let pending = !recovery.files.is_empty();
    for (key, anchors) in recovery.files {
        if key.is_empty()
            || key.contains([':', '\\'])
            || Path::new(&key).is_absolute()
            || key.split('/').any(|p| p == ".." || p.is_empty())
        {
            return Err("Invalid recovery source key".into());
        }
        let entry =
            cache
                .entries
                .entry(key.clone())
                .or_insert_with(|| crate::types::GenCacheEntry {
                    source_mtime: 0,
                    source_path: p.root.join(&key).to_string_lossy().into_owned(),
                    source_digest: String::new(),
                    anchors: Vec::new(),
                });
        entry.source_digest.clear();
        use aden_store::GraphStorage;
        for anchor in anchors {
            // An external journal cannot authorize deleting another source's
            // documents. Verify each possible batch write against the store.
            if let Some(doc) = storage.get_document(&anchor)?
                && doc
                    .attributes
                    .get("source_file")
                    .is_some_and(|source| source.replace('\\', "/") == key)
            {
                entry.anchors.push(anchor);
            }
        }
        entry.anchors.sort();
        entry.anchors.dedup();
    }
    Ok(pending)
}
pub(super) fn prepare_recovery(
    p: &Paths,
    files: &std::collections::BTreeMap<String, Vec<String>>,
) -> Result<()> {
    std::fs::create_dir_all(&p.dir)?;
    // The store writer lease serializes this journal with every generator.
    p.validate()?;
    #[derive(serde::Serialize)]
    struct RecoveryRef<'a> {
        root: &'a Path,
        files: &'a std::collections::BTreeMap<String, Vec<String>>,
    }
    let bytes = serde_json::to_vec(&RecoveryRef {
        root: &p.root,
        files,
    })?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("Generation recovery journal exceeds safety limit".into());
    }
    atomic_write(p, "recovery.json", &bytes)?;
    Ok(())
}
/// Caller must hold the store writer lease (or ConfigurationGuard).
pub fn clear_recovery(root: &Path) -> Result<()> {
    Paths::new(root).clear_recovery()
}

/// Holds both launch serialization and the actual store writer lease while the
/// caller changes storage configuration. Contention fails without stopping work.
pub struct ConfigurationGuard {
    _gate: FileLock,
    _writer: FileLock,
}
pub fn configuration_guard(path: &Path) -> Result<ConfigurationGuard> {
    let p = Paths::new(path);
    std::fs::create_dir_all(&p.dir)?;
    let gate = p.gate()?;
    ensure_recovery_owner(&p)?;
    if reconcile(&p)?.is_some_and(|s| s.active()) {
        return Err("Storage configuration cannot change while a generation job is active; wait or cancel it first".into());
    }
    let writer = FileLock::acquire_timeout(
        aden_core::lock::store_lock_path(&aden_paths::store_dir(&p.root)),
        Duration::ZERO,
    )?;
    Ok(ConfigurationGuard {
        _gate: gate,
        _writer: writer,
    })
}

#[cfg(not(windows))]
fn spawn_worker(root: &Path, job_id: &str) -> io::Result<u32> {
    let child = Command::new(std::env::current_exe()?)
        .arg("__gen-worker")
        .arg("--path")
        .arg(root)
        .arg("--job-id")
        .arg(job_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(child.id())
}

#[cfg(windows)]
fn spawn_worker(root: &Path, job_id: &str) -> io::Result<u32> {
    // Stable std::process::Command inherits every inheritable Windows handle,
    // including unrelated capture pipes even when its stdio is redirected.
    // Explicitly disable inheritance so caller EOF is independent of this job.
    use std::ffi::{OsStr, c_void};
    use std::os::windows::ffi::OsStrExt;
    #[repr(C)]
    struct StartupInfo {
        cb: u32,
        reserved: *mut u16,
        desktop: *mut u16,
        title: *mut u16,
        x: u32,
        y: u32,
        x_size: u32,
        y_size: u32,
        x_chars: u32,
        y_chars: u32,
        fill: u32,
        flags: u32,
        show: u16,
        reserved_size: u16,
        reserved_bytes: *mut u8,
        stdin: *mut c_void,
        stdout: *mut c_void,
        stderr: *mut c_void,
    }
    #[repr(C)]
    struct ProcessInfo {
        process: *mut c_void,
        thread: *mut c_void,
        pid: u32,
        thread_id: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateProcessW(
            application: *const u16,
            command: *mut u16,
            process_attributes: *mut c_void,
            thread_attributes: *mut c_void,
            inherit: i32,
            flags: u32,
            environment: *mut c_void,
            cwd: *const u16,
            startup: *mut StartupInfo,
            process: *mut ProcessInfo,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    let exe = std::env::current_exe()?;
    let mut application: Vec<u16> = exe.as_os_str().encode_wide().collect();
    application.push(0);
    let args = [
        exe.as_os_str(),
        OsStr::new("__gen-worker"),
        OsStr::new("--path"),
        root.as_os_str(),
        OsStr::new("--job-id"),
        OsStr::new(job_id),
    ];
    let mut command = Vec::new();
    for arg in args {
        if !command.is_empty() {
            command.push(b' ' as u16)
        }
        command.extend(quote(arg));
    }
    if command.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in generation worker argument",
        ));
    }
    command.push(0);
    // Own a hidden console so nested Git processes inherit it instead of
    // repeatedly allocating new consoles. Null std handles and disabled handle
    // inheritance keep all caller-owned streams out of the worker.
    let mut startup: StartupInfo = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<StartupInfo>() as u32;
    startup.flags = 0x00000001 | 0x00000100; // USESHOWWINDOW | USESTDHANDLES
    startup.show = 0; // SW_HIDE
    let mut process: ProcessInfo = unsafe { std::mem::zeroed() };
    let success = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            0x00000010 | 0x00000400, // NEW_CONSOLE | UNICODE_ENVIRONMENT
            std::ptr::null_mut(),
            std::ptr::null(),
            &mut startup,
            &mut process,
        )
    };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe {
        CloseHandle(process.thread);
        CloseHandle(process.process);
    }
    Ok(process.pid)
}

#[cfg(windows)]
fn quote(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut out = vec![b'"' as u16];
    let mut slashes = 0;
    for c in value.encode_wide() {
        if c == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(
            b'\\' as u16,
            if c == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    out.push(b'"' as u16);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_layout_cannot_mutate_after_configuration_changes() {
        if std::env::var_os("ADEN_DATA_DIR").is_some() || std::env::var_os("ADEN_STORE").is_some() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        let old_config = aden_paths::StorageConfig {
            portable: false,
            data_dir: Some(temp.path().join("old")),
        };
        aden_paths::write_storage_config(&root, Some(&old_config)).unwrap();
        let old = Paths::new(&root);
        std::fs::create_dir_all(&old.dir).unwrap();
        let held = old.lease_gate().unwrap();
        let new_config = aden_paths::StorageConfig {
            portable: false,
            data_dir: Some(temp.path().join("new")),
        };
        aden_paths::write_storage_config(&root, Some(&new_config)).unwrap();
        drop(held);
        let error = old.gate().unwrap_err();
        assert!(error.to_string().contains("configuration changed"));
        assert!(!old.file("status.json").exists());
        let current = Paths::new(&root);
        assert_ne!(old.dir, current.dir);
        std::fs::create_dir_all(&current.dir).unwrap();
        assert!(current.gate().is_ok());
    }
    #[test]
    fn running_job_requires_lease_instead_of_reused_launch_pid() {
        let temp = tempfile::tempdir().unwrap();
        let p = Paths {
            root: temp.path().to_path_buf(),
            dir: temp.path().join("job"),
        };
        std::fs::create_dir_all(&p.dir).unwrap();
        std::fs::write(
            p.file("launch.pid"),
            format!("{}\n{}\nabcdef\n", std::process::id(), now() / 1000),
        )
        .unwrap();
        let mut s = JobStatus {
            schema_version: 1,
            job_id: "abcdef".into(),
            root: p.root.clone(),
            state: "running".into(),
            phase: "persist_prune".into(),
            completed_files: 0,
            total_files: 1,
            started_ms: 0,
            updated_ms: 0,
            worker_pid: Some(std::process::id()),
            error: None,
        };
        assert!(
            !live(&p, &s),
            "PID alone cannot keep a previously running job active"
        );
        s.state = "queued".into();
        s.phase = "launching".into();
        assert!(
            live(&p, &s),
            "launch PID bridges startup before worker lease acquisition"
        );
    }
    fn fixture_status(paths: &Paths, state: &str) -> JobStatus {
        JobStatus {
            schema_version: 1,
            job_id: "abcdef".into(),
            root: paths.root.clone(),
            state: state.into(),
            phase: state.into(),
            completed_files: 0,
            total_files: 1,
            started_ms: 0,
            updated_ms: 0,
            worker_pid: None,
            error: None,
        }
    }
    #[test]
    fn interruption_preserves_phase_and_reports_worker_log() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            root: temp.path().to_path_buf(),
            dir: temp.path().join("job"),
        };
        std::fs::create_dir_all(&paths.dir).unwrap();
        let mut status = fixture_status(&paths, "running");
        status.phase = "parse_prepare".into();
        status.completed_files = 6559;
        status.total_files = 30951;
        status.worker_pid = Some(380);
        save(&paths, &status).unwrap();
        append_worker_log(&paths, "started pid=380 job=abcdef").unwrap();
        append_worker_log(&paths, "panic at src/gen.rs:10: index out of bounds").unwrap();

        let reconciled = reconcile(&paths).unwrap().unwrap();
        assert_eq!(reconciled.state, "interrupted");
        assert_eq!(
            reconciled.phase, "parse_prepare",
            "an interruption must not erase where the worker stopped"
        );
        let error = reconciled.error.unwrap();
        assert!(error.contains("worker pid 380"), "{error}");
        assert!(error.contains("parse_prepare"), "{error}");
        assert!(error.contains("6559/30951"), "{error}");
        assert!(
            error.contains("panic at src/gen.rs:10: index out of bounds"),
            "worker log evidence missing: {error}"
        );
    }

    #[test]
    fn metadata_publication_preserves_hardlink_targets_and_named_pending_files() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths {
            root: temp.path().join("repo"),
            dir: temp.path().join("job"),
        };
        std::fs::create_dir_all(&paths.dir).unwrap();
        let victim = temp.path().join("caller-owned.txt");
        let original = b"caller-owned bytes must remain unchanged";
        std::fs::write(&victim, original).unwrap();
        for name in [
            "status.pending",
            "recovery.pending",
            "status.json",
            "cancel",
            "events.log",
            "launch.pid",
            "recovery.json",
        ] {
            std::fs::hard_link(&victim, paths.file(name)).unwrap();
        }
        paths.validate().unwrap();
        save(&paths, &fixture_status(&paths, "completed")).unwrap();
        for name in ["cancel", "events.log", "launch.pid", "recovery.json"] {
            atomic_write(&paths, name, b"new job metadata").unwrap();
        }
        assert_eq!(std::fs::read(&victim).unwrap(), original);
        for name in ["status.pending", "recovery.pending"] {
            assert_eq!(std::fs::read(paths.file(name)).unwrap(), original);
        }
        assert_eq!(
            load_with_shared_policy(&paths, false)
                .unwrap()
                .unwrap()
                .state,
            "completed"
        );
        for name in ["cancel", "events.log", "launch.pid", "recovery.json"] {
            assert_eq!(
                std::fs::read(paths.file(name)).unwrap(),
                b"new job metadata"
            );
        }
    }
    #[test]
    fn configuration_guard_refuses_foreign_recovery_before_reset() {
        if std::env::var_os("ADEN_DATA_DIR").is_some() || std::env::var_os("ADEN_STORE").is_some() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        aden_paths::write_storage_config(
            &root,
            Some(&aden_paths::StorageConfig {
                portable: false,
                data_dir: Some(temp.path().join("data")),
            }),
        )
        .unwrap();
        let paths = Paths::new(&root);
        std::fs::create_dir_all(&paths.dir).unwrap();
        save(&paths, &fixture_status(&paths, "completed")).unwrap();
        let recovery = serde_json::to_vec(&Recovery {
            root: temp.path().join("another-project"),
            files: std::collections::BTreeMap::new(),
        })
        .unwrap();
        atomic_write(&paths, "recovery.json", &recovery).unwrap();
        let cache = aden_paths::gen_cache_file(&root);
        std::fs::write(&cache, b"preserve cache").unwrap();
        let config = aden_paths::storage_config_file(&root);
        let original_config = std::fs::read(&config).unwrap();
        match configuration_guard(&root) {
            Err(error) => assert!(
                error
                    .to_string()
                    .contains("another project's unreconciled recovery")
            ),
            Ok(_) => panic!("configuration/reset guard must refuse a foreign recovery journal"),
        }
        assert_eq!(
            std::fs::read(paths.file("recovery.json")).unwrap(),
            recovery
        );
        assert_eq!(std::fs::read(cache).unwrap(), b"preserve cache");
        assert_eq!(std::fs::read(&config).unwrap(), original_config);
        std::fs::remove_file(paths.file("recovery.json")).unwrap();
        assert!(
            configuration_guard(&root).is_ok(),
            "the same fixture must accept a valid idle store after foreign recovery is removed"
        );
    }
    #[test]
    fn shared_store_terminal_handoff_preserves_foreign_active_and_recovery_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let first = Paths {
            root: temp.path().join("a"),
            dir: temp.path().join("job"),
        };
        let second = Paths {
            root: temp.path().join("b"),
            dir: first.dir.clone(),
        };
        std::fs::create_dir_all(&first.dir).unwrap();
        let mut prior = fixture_status(&first, "completed");
        save(&first, &prior).unwrap();
        let observed = load_with_shared_policy(&second, true).unwrap().unwrap();
        assert_eq!(
            observed.root, first.root,
            "status must retain the recorded project"
        );
        assert!(
            !admit_start(&second, &observed, true).unwrap(),
            "completed foreign history admits a new job instead of joining it"
        );
        assert!(admit_start(&second, &observed, false).is_err());
        assert!(
            ensure_owned(&second, &observed).is_err(),
            "another project cannot cancel this job"
        );
        let lease = FileLock::acquire_timeout(first.file("worker.lock"), Duration::ZERO).unwrap();
        assert!(
            admit_start(&second, &observed, true).is_err(),
            "actual foreign worker lease blocks handoff even with terminal metadata"
        );
        drop(lease);
        prior.state = "running".into();
        assert!(
            admit_start(&second, &prior, true).is_err(),
            "foreign active status cannot be reassigned"
        );
        let recovery = serde_json::to_vec(&Recovery {
            root: first.root.clone(),
            files: std::collections::BTreeMap::new(),
        })
        .unwrap();
        atomic_write(&first, "recovery.json", &recovery).unwrap();
        assert!(admit_start(&second, &observed, true).is_err());
        assert!(
            ensure_recovery_owner(&second).is_err(),
            "foreign recovery blocks startup even without a status file"
        );
        assert_eq!(
            std::fs::read(first.file("recovery.json")).unwrap(),
            recovery
        );
        std::fs::remove_file(first.file("recovery.json")).unwrap();
        assert!(!admit_start(&second, &observed, true).unwrap());
        let next = fixture_status(&second, "queued");
        save(&second, &next).unwrap();
        assert_eq!(
            load_with_shared_policy(&second, true)
                .unwrap()
                .unwrap()
                .root,
            second.root
        );
    }
    #[cfg(windows)]
    #[test]
    fn windows_worker_quotes_spaces_quotes_and_terminal_backslashes() {
        use std::ffi::OsStr;
        let cases = [
            ("", "\"\""),
            ("C:\\repo with spaces\\", "\"C:\\repo with spaces\\\\\""),
            ("a\"b", "\"a\\\"b\""),
            ("plain", "\"plain\""),
        ];
        for (input, expected) in cases {
            assert_eq!(
                String::from_utf16(&quote(OsStr::new(input))).unwrap(),
                expected
            );
        }
    }
}
