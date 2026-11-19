// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! One bounded, replaceable CLI read worker per MCP server.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

const PROTOCOL: &str = "aden-read-worker-v1";
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 12 * 1024 * 1024;
const STDERR_TAIL_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct BinaryIdentity {
    command: OsString,
    canonical: Option<PathBuf>,
    modified: Option<std::time::SystemTime>,
    len: Option<u64>,
}

impl BinaryIdentity {
    fn current() -> Self {
        let command = super::resolve_aden_binary();
        let path = Path::new(&command);
        let resolved = if path.components().count() > 1 || path.is_absolute() {
            Some(path.to_path_buf())
        } else {
            std::env::var_os("PATH").and_then(|paths| {
                std::env::split_paths(&paths).find_map(|directory| {
                    let candidate = directory.join(path);
                    if candidate.is_file() {
                        Some(candidate)
                    } else if cfg!(windows) {
                        let candidate = candidate.with_extension("exe");
                        candidate.is_file().then_some(candidate)
                    } else {
                        None
                    }
                })
            })
        };
        let canonical = resolved.and_then(|path| path.canonicalize().ok());
        let metadata = canonical.as_ref().and_then(|path| path.metadata().ok());
        Self {
            command,
            canonical,
            modified: metadata.as_ref().and_then(|meta| meta.modified().ok()),
            len: metadata.map(|meta| meta.len()),
        }
    }
}

struct Worker {
    root: PathBuf,
    binary: BinaryIdentity,
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    pid: u64,
    stderr: std::sync::Arc<std::sync::Mutex<VecDeque<u8>>>,
    stderr_task: tokio::task::JoinHandle<()>,
}

#[derive(Debug)]
enum StartupError {
    Unsupported(String),
    Transient(String),
}

impl StartupError {
    fn fallback_diagnostic(&self) -> String {
        let (message, recovery) = match self {
            Self::Unsupported(message) => (
                message,
                "compatibility fallback is cached for this executable; update the CLI and reconnect to enable resident reads",
            ),
            Self::Transient(message) => {
                (message, "worker startup will be retried on the next read")
            }
        };
        let message = super::sanitize_error(message)
            .lines()
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "resident read worker startup failed: {message}; using serial execution for this call; {recovery}"
        )
    }
}

fn old_cli_rejects_worker(exit_code: Option<i32>, diagnostic: &str) -> bool {
    exit_code == Some(2)
        && [
            "unrecognized subcommand '__read-worker'",
            "unexpected argument '__read-worker'",
        ]
        .iter()
        .any(|message| diagnostic.contains(message))
}

impl Worker {
    async fn start(root: &Path, binary: BinaryIdentity) -> Result<Self, StartupError> {
        let executable = binary
            .canonical
            .as_ref()
            .map(|path| path.as_os_str())
            .unwrap_or(&binary.command);
        let mut command = super::configured_aden_command_for_binary(root, executable);
        command.arg("__read-worker");
        Self::start_with_command(root, binary, command).await
    }

    async fn start_with_command(
        root: &Path,
        binary: BinaryIdentity,
        mut command: tokio::process::Command,
    ) -> Result<Self, StartupError> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command
            .spawn()
            .map_err(|error| StartupError::Transient(error.to_string()))?;
        let mut worker = Self::from_child(root, binary, child).map_err(StartupError::Transient)?;
        let hello = match read_frame(&mut worker.output).await {
            Ok(hello) => hello,
            Err(error) => {
                let diagnostic = worker.failure_message(error).await;
                // EOF alone does not establish incompatibility: a transient
                // startup crash must not disable resident reads permanently.
                let status = tokio::time::timeout(Duration::from_millis(100), worker.child.wait())
                    .await
                    .ok()
                    .and_then(Result::ok);
                return Err(
                    if old_cli_rejects_worker(status.and_then(|status| status.code()), &diagnostic)
                    {
                        StartupError::Unsupported(diagnostic)
                    } else {
                        StartupError::Transient(diagnostic)
                    },
                );
            }
        };
        let protocol = hello
            .get("protocol")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| StartupError::Transient("worker handshake has no protocol".into()))?;
        let pid = hello
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                StartupError::Transient("worker handshake has no process identity".into())
            })?;
        if worker.child.id().map(u64::from) != Some(pid) {
            return Err(StartupError::Transient(
                "worker handshake process identity mismatch".into(),
            ));
        }
        if protocol != PROTOCOL {
            return Err(StartupError::Unsupported(format!(
                "CLI uses unsupported resident read protocol {protocol}"
            )));
        }
        Ok(worker)
    }

    fn from_child(root: &Path, binary: BinaryIdentity, mut child: Child) -> Result<Self, String> {
        let input = child.stdin.take().ok_or("worker stdin unavailable")?;
        let output = BufReader::new(child.stdout.take().ok_or("worker stdout unavailable")?);
        let mut error_output = child.stderr.take().ok_or("worker stderr unavailable")?;
        let stderr = std::sync::Arc::new(std::sync::Mutex::new(VecDeque::new()));
        let tail = stderr.clone();
        let stderr_task = tokio::spawn(async move {
            let mut buffer = [0_u8; 4096];
            while let Ok(count) = error_output.read(&mut buffer).await {
                if count == 0 {
                    break;
                }
                let mut tail = tail.lock().unwrap_or_else(|error| error.into_inner());
                tail.extend(&buffer[..count]);
                while tail.len() > STDERR_TAIL_BYTES {
                    tail.pop_front();
                }
            }
        });
        let pid = child
            .id()
            .map(u64::from)
            .ok_or("worker process identity unavailable")?;
        Ok(Self {
            root: root.to_path_buf(),
            binary,
            child,
            input,
            output,
            pid,
            stderr,
            stderr_task,
        })
    }

    async fn failure_message(&mut self, cause: String) -> String {
        // EOF often accompanies a process-level freshness error. Give its
        // independently drained stderr a bounded chance to finish before
        // preserving that diagnostic for the normal MCP error adapter.
        let _ = tokio::time::timeout(Duration::from_millis(100), &mut self.stderr_task).await;
        let bytes: Vec<_> = self
            .stderr
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .copied()
            .collect();
        let diagnostic = String::from_utf8_lossy(&bytes);
        if diagnostic.trim().is_empty() {
            cause
        } else {
            format!("{cause}\n{diagnostic}")
        }
    }

    async fn request(&mut self, args: &[&str]) -> Result<(bool, String, String), String> {
        self.stderr
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
        let mut request = serde_json::to_vec(&serde_json::json!({"args": args}))
            .map_err(|error| error.to_string())?;
        request.push(b'\n');
        if request.len() > MAX_REQUEST_BYTES {
            return Err("resident read arguments exceed the request limit".into());
        }
        if let Err(error) = self.input.write_all(&request).await {
            return Err(self
                .failure_message(format!("resident read write failed: {error}"))
                .await);
        }
        if let Err(error) = self.input.flush().await {
            return Err(self
                .failure_message(format!("resident read flush failed: {error}"))
                .await);
        }
        let response = match read_frame(&mut self.output).await {
            Ok(response) => response,
            Err(error) => return Err(self.failure_message(error).await),
        };
        if response.get("protocol").and_then(serde_json::Value::as_str) != Some(PROTOCOL)
            || response.get("pid").and_then(serde_json::Value::as_u64) != Some(self.pid)
        {
            return Err("resident read protocol identity mismatch".into());
        }
        let ok = response
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .ok_or("resident read response has no outcome")?;
        let stdout = response
            .get("stdout")
            .and_then(serde_json::Value::as_str)
            .ok_or("resident read response has no stdout")?;
        let stderr = response
            .get("stderr")
            .and_then(serde_json::Value::as_str)
            .ok_or("resident read response has no stderr")?;
        if ok && serde_json::from_str::<serde_json::Value>(stdout).is_err() {
            return Err("resident read returned an invalid machine body".into());
        }
        Ok((ok, stdout.to_string(), stderr.to_string()))
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stderr_task.abort();
    }
}

async fn read_frame<R: AsyncBufRead + Unpin>(output: &mut R) -> Result<serde_json::Value, String> {
    let mut line = Vec::new();
    loop {
        let buffer = output.fill_buf().await.map_err(|error| error.to_string())?;
        if buffer.is_empty() {
            return Err("resident read worker closed its response pipe".into());
        }
        let count = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        if line.len().saturating_add(count) > MAX_RESPONSE_BYTES {
            return Err("resident read response exceeds its transport limit".into());
        }
        let complete = buffer[count - 1] == b'\n';
        line.extend_from_slice(&buffer[..count]);
        output.consume(count);
        if complete {
            return serde_json::from_slice(&line)
                .map_err(|_| "resident read worker returned invalid JSON".into());
        }
    }
}

#[derive(Default)]
struct State {
    worker: Option<Worker>,
    unsupported_binary: Option<BinaryIdentity>,
}

#[derive(Default)]
pub(super) struct ResidentReads {
    state: Mutex<State>,
}

impl ResidentReads {
    pub(super) async fn execute(
        &self,
        project_dir: &Path,
        tool: &str,
        args: &[&str],
        deadline: Duration,
    ) -> Result<String, String> {
        if !matches!(tool, "locate" | "understand" | "query")
            || std::env::var("ADEN_READ_SESSION").as_deref() == Ok("0")
        {
            return super::run_aden_command_with_timeout(project_dir, tool, args, deadline).await;
        }
        tokio::time::timeout(
            deadline,
            self.execute_inner(project_dir, tool, args, deadline),
        )
        .await
        .map_err(|_| {
            format!(
                "resident read timed out after {}s; any worker owned by this request was discarded",
                deadline.as_secs_f64()
            )
        })?
    }

    async fn execute_inner(
        &self,
        project_dir: &Path,
        tool: &str,
        args: &[&str],
        deadline: Duration,
    ) -> Result<String, String> {
        self.execute_inner_with_start(
            project_dir,
            tool,
            args,
            deadline,
            |root, binary| async move { Worker::start(&root, binary).await },
        )
        .await
    }

    async fn execute_inner_with_start<S, F>(
        &self,
        project_dir: &Path,
        tool: &str,
        args: &[&str],
        deadline: Duration,
        start: S,
    ) -> Result<String, String>
    where
        S: FnOnce(PathBuf, BinaryIdentity) -> F,
        F: std::future::Future<Output = Result<Worker, StartupError>>,
    {
        let root = project_dir
            .canonicalize()
            .map_err(|error| format!("read root unavailable: {error}"))?;
        let binary = BinaryIdentity::current();
        let mut state = self.state.lock().await;
        if state.unsupported_binary.as_ref() == Some(&binary) {
            drop(state);
            return super::run_aden_command_with_timeout(project_dir, tool, args, deadline).await;
        }
        // Take ownership before any await. Cancellation or timeout drops this
        // child (kill_on_drop), leaving an empty slot rather than stale frames.
        let mut worker = state.worker.take();
        if worker
            .as_ref()
            .is_some_and(|worker| worker.root != root || worker.binary != binary)
        {
            worker = None;
        }
        if let Some(cached) = worker.as_mut()
            && cached
                .child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_some()
        {
            // This call has not sent anything. Replacing a worker known to
            // have exited is safe; failures after request() starts are not.
            worker = None;
        }
        let mut worker = match worker {
            Some(worker) => worker,
            None => match start(root, binary.clone()).await {
                Ok(worker) => worker,
                Err(error) => {
                    // No request was sent. Compatibility fallback is safe here;
                    // never replay a request after an uncertain write or read.
                    if matches!(error, StartupError::Unsupported(_)) {
                        state.unsupported_binary = Some(binary);
                    }
                    eprintln!("{}", error.fallback_diagnostic());
                    drop(state);
                    return super::run_aden_command_with_timeout(project_dir, tool, args, deadline)
                        .await;
                }
            },
        };
        if worker
            .child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            let error = worker
                .failure_message(
                    "resident read worker exited; retry this call to start a new session".into(),
                )
                .await;
            return Err(super::preserve_cli_error_for_mcp(&error));
        }
        let (ok, stdout, stderr) = worker
            .request(args)
            .await
            .map_err(|error| super::preserve_cli_error_for_mcp(&error))?;
        state.worker = Some(worker);
        if ok {
            Ok(super::preserve_cli_output_for_mcp(tool, &stdout))
        } else {
            Err(super::preserve_cli_error_for_mcp(&stderr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RestoreBinary(Option<OsString>);

    impl RestoreBinary {
        fn pin_current_cli() -> Self {
            Self(
                crate::TEST_ADEN_BINARY
                    .lock()
                    .unwrap()
                    .replace(crate::tests::aden_cli_test_binary().into_os_string()),
            )
        }
    }

    impl Drop for RestoreBinary {
        fn drop(&mut self) {
            *crate::TEST_ADEN_BINARY.lock().unwrap() = self.0.take();
        }
    }

    fn recovery_fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname='resident-recovery'\nversion='0.1.0'\n",
        )
        .unwrap();
        std::fs::write(root.path().join("lib.rs"), "pub fn recovery_symbol() {}\n").unwrap();
        // These tests exercise resident startup/replacement, not whether cold
        // generation happens to finish inside the read's bounded wait.
        let generated = std::process::Command::new(crate::tests::aden_cli_test_binary())
            .current_dir(root.path())
            .args(["gen", "."])
            .output()
            .unwrap();
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        root
    }

    fn assert_recovery_definition(body: &str) {
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["match_kind"], "definition", "{body}");
        assert_eq!(body["items"].as_array().unwrap().len(), 1, "{body}");
        assert!(
            body["items"][0]["anchor"]
                .as_str()
                .unwrap()
                .ends_with("#recovery_symbol")
        );
    }

    #[test]
    fn compatibility_detection_requires_explicit_old_cli_rejection_and_diagnostics_are_bounded() {
        let unsupported = "error: unrecognized subcommand '__read-worker'";
        assert!(old_cli_rejects_worker(Some(2), unsupported));
        assert!(old_cli_rejects_worker(
            Some(2),
            "error: unexpected argument '__read-worker' found"
        ));
        assert!(!old_cli_rejects_worker(Some(1), unsupported));
        assert!(!old_cli_rejects_worker(None, unsupported));
        assert!(!old_cli_rejects_worker(
            Some(2),
            "worker initialization failed"
        ));
        let diagnostic = StartupError::Transient(format!(
            "failed to start /private/sensitive/aden\n{}",
            "x".repeat(20_000)
        ))
        .fallback_diagnostic();
        assert!(diagnostic.len() < 4500);
        assert!(
            !diagnostic.contains("/private/sensitive/aden"),
            "{diagnostic}"
        );
        assert!(!diagnostic.contains('\n'));
        assert!(diagnostic.contains("using serial execution for this call"));
        assert!(diagnostic.contains("retried on the next read"));
    }

    fn startup_failure_command(root: &Path, unsupported: bool) -> tokio::process::Command {
        let mut command = if cfg!(windows) {
            let mut command = tokio::process::Command::new("powershell.exe");
            let script = if unsupported {
                "[Console]::Error.WriteLine(\"error: unrecognized subcommand '__read-worker'\"); exit 2"
            } else {
                "[Console]::Error.WriteLine('worker initialization failed'); exit 2"
            };
            command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
            command
        } else {
            let mut command = tokio::process::Command::new("sh");
            let script = if unsupported {
                "printf \"error: unrecognized subcommand '__read-worker'\\n\" >&2; exit 2"
            } else {
                "printf 'worker initialization failed\\n' >&2; exit 2"
            };
            command.args(["-c", script]);
            command
        };
        command.current_dir(root);
        command
    }

    #[tokio::test]
    async fn startup_process_failure_distinguishes_incompatibility_from_a_transient_crash() {
        let root = tempfile::tempdir().unwrap();
        for unsupported in [false, true] {
            let result = Worker::start_with_command(
                root.path(),
                BinaryIdentity::current(),
                startup_failure_command(root.path(), unsupported),
            )
            .await;
            assert_eq!(
                matches!(result, Err(StartupError::Unsupported(_))),
                unsupported
            );
            if !unsupported {
                assert!(matches!(result, Err(StartupError::Transient(_))));
            }
        }
    }

    #[tokio::test]
    async fn transient_startup_falls_back_once_and_retries_the_same_binary_on_the_next_call() {
        let _guard = crate::tests::MCP_ROOT_TEST_ENV.lock().await;
        let _binary = RestoreBinary::pin_current_cli();
        let root = recovery_fixture();
        let reads = ResidentReads::default();
        let original_identity = BinaryIdentity::current();
        let body = reads
            .execute_inner_with_start(
                root.path(),
                "locate",
                &["locate", "--symbol", "recovery_symbol", "."],
                Duration::from_secs(10),
                |_, _| async {
                    Err(StartupError::Transient(
                        "injected temporary startup failure".into(),
                    ))
                },
            )
            .await
            .unwrap();
        assert_recovery_definition(&body);
        {
            let state = reads.state.lock().await;
            assert!(state.worker.is_none());
            assert!(
                state.unsupported_binary.is_none(),
                "a transient failure became sticky"
            );
        }
        let starts = std::sync::atomic::AtomicUsize::new(0);
        let body = reads
            .execute_inner_with_start(
                root.path(),
                "locate",
                &["locate", "--symbol", "recovery_symbol", "."],
                Duration::from_secs(10),
                |root, binary| {
                    starts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    assert_eq!(binary, original_identity);
                    async move { Worker::start(&root, binary).await }
                },
            )
            .await
            .unwrap();
        assert_recovery_definition(&body);
        assert_eq!(starts.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(reads.state.lock().await.worker.is_some());
    }

    #[tokio::test]
    async fn confirmed_unsupported_protocol_uses_cached_compatibility_fallback() {
        let _guard = crate::tests::MCP_ROOT_TEST_ENV.lock().await;
        let _binary = RestoreBinary::pin_current_cli();
        let root = recovery_fixture();
        let reads = ResidentReads::default();
        let body = reads
            .execute_inner_with_start(
                root.path(),
                "locate",
                &["locate", "--symbol", "recovery_symbol", "."],
                Duration::from_secs(10),
                |_, _| async {
                    Err(StartupError::Unsupported(
                        "injected incompatible protocol".into(),
                    ))
                },
            )
            .await
            .unwrap();
        assert_recovery_definition(&body);
        assert_eq!(
            reads.state.lock().await.unsupported_binary.as_ref(),
            Some(&BinaryIdentity::current())
        );
        let body = reads
            .execute_inner_with_start(
                root.path(),
                "locate",
                &["locate", "--symbol", "recovery_symbol", "."],
                Duration::from_secs(10),
                |_, _| async { panic!("known unsupported binary was started again") },
            )
            .await
            .unwrap();
        assert_recovery_definition(&body);
        assert!(reads.state.lock().await.worker.is_none());
    }

    #[tokio::test]
    async fn a_known_dead_cached_worker_is_replaced_before_sending_the_new_request() {
        let _guard = crate::tests::MCP_ROOT_TEST_ENV.lock().await;
        let _binary = RestoreBinary::pin_current_cli();
        let root = recovery_fixture();
        let reads = ResidentReads::default();
        let body = reads
            .execute_inner(
                root.path(),
                "locate",
                &["locate", "--symbol", "recovery_symbol", "."],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_recovery_definition(&body); // known-success process control
        let original_pid = {
            let mut state = reads.state.lock().await;
            let worker = state.worker.as_mut().unwrap();
            let pid = worker.pid;
            worker.child.kill().await.unwrap();
            pid
        };
        let starts = std::sync::atomic::AtomicUsize::new(0);
        let body = reads
            .execute_inner_with_start(
                root.path(),
                "locate",
                &["locate", "--symbol", "recovery_symbol", "."],
                Duration::from_secs(10),
                |root, binary| {
                    starts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    async move { Worker::start(&root, binary).await }
                },
            )
            .await
            .unwrap();
        assert_recovery_definition(&body);
        assert_eq!(starts.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_ne!(
            reads.state.lock().await.worker.as_ref().unwrap().pid,
            original_pid
        );
    }

    async fn wait_for_marker(path: &Path) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !path.is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("fixture did not reach barrier {}", path.display()));
    }

    #[tokio::test]
    async fn frame_reader_accepts_valid_json_and_rejects_malformed_or_truncated_frames() {
        let mut valid = BufReader::new(&b"{\"protocol\":\"aden-read-worker-v1\"}\n"[..]);
        assert_eq!(read_frame(&mut valid).await.unwrap()["protocol"], PROTOCOL);
        let mut malformed = BufReader::new(&b"not-json\n"[..]);
        assert!(
            read_frame(&mut malformed)
                .await
                .unwrap_err()
                .contains("invalid JSON")
        );
        let mut truncated = BufReader::new(&b"{\"protocol\":\"aden-read-worker-v1\"}"[..]);
        assert!(
            read_frame(&mut truncated)
                .await
                .unwrap_err()
                .contains("closed")
        );
        let mut oversized =
            BufReader::new(std::io::Cursor::new(vec![b'x'; MAX_RESPONSE_BYTES + 1]));
        assert!(
            read_frame(&mut oversized)
                .await
                .unwrap_err()
                .contains("transport limit")
        );
    }

    async fn delayed_worker(root: &Path, marker: &Path, malformed: bool) -> Worker {
        // Use the platform's ordinary shell as a controlled child, with no
        // dependencies on Python or a separately built fixture executable.
        let mut command = if cfg!(windows) {
            let mut command = tokio::process::Command::new("powershell.exe");
            let ready = marker
                .with_extension("ready")
                .to_string_lossy()
                .replace('\'', "''");
            let accepted = marker
                .with_extension("accepted")
                .to_string_lossy()
                .replace('\'', "''");
            let marker = marker.to_string_lossy().replace('\'', "''");
            let prefix = if malformed {
                "Write-Output 'invalid-json'; "
            } else {
                ""
            };
            command.args(["-NoProfile", "-NonInteractive", "-Command", &format!("[IO.File]::WriteAllText('{ready}', 'ready'); $null = [Console]::ReadLine(); [IO.File]::WriteAllText('{accepted}', 'accepted'); {prefix}Start-Sleep -Milliseconds 600; [IO.File]::WriteAllText('{marker}', 'survived')")]);
            command
        } else {
            let mut command = tokio::process::Command::new("sh");
            let script = if malformed {
                "printf ready > \"$2\"; read request; printf accepted > \"$3\"; printf 'invalid-json\\n'; sleep 0.6; printf survived > \"$1\""
            } else {
                "printf ready > \"$2\"; read request; printf accepted > \"$3\"; sleep 0.6; printf survived > \"$1\""
            };
            command
                .arg("-c")
                .arg(script)
                .arg("resident-fixture")
                .arg(marker)
                .arg(marker.with_extension("ready"))
                .arg(marker.with_extension("accepted"));
            command
        };
        command
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let worker = Worker::from_child(
            &root.canonicalize().unwrap(),
            BinaryIdentity::current(),
            command.spawn().unwrap(),
        )
        .unwrap();
        wait_for_marker(&marker.with_extension("ready")).await;
        worker
    }

    #[tokio::test]
    async fn timed_out_resident_request_discards_child_before_late_mutation() {
        let _guard = crate::tests::MCP_ROOT_TEST_ENV.lock().await;
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("late-mutation");
        let reads = ResidentReads::default();
        reads.state.lock().await.worker = Some(delayed_worker(root.path(), &marker, false).await);
        let error = reads
            .execute(
                root.path(),
                "query",
                &["query", "--from", "example", "."],
                Duration::from_millis(300),
            )
            .await
            .unwrap_err();
        assert!(error.contains("timed out"), "{error}");
        assert!(
            marker.with_extension("accepted").exists(),
            "fixture never read the request"
        );
        assert!(reads.state.lock().await.worker.is_none());
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert!(!marker.exists(), "timed-out resident child survived");
    }

    #[tokio::test]
    async fn malformed_resident_response_is_not_replayed_and_discards_child() {
        let _guard = crate::tests::MCP_ROOT_TEST_ENV.lock().await;
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("late-mutation");
        let reads = ResidentReads::default();
        reads.state.lock().await.worker = Some(delayed_worker(root.path(), &marker, true).await);
        let error = reads
            .execute(
                root.path(),
                "query",
                &["query", "--from", "example", "."],
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
        assert!(error.contains("invalid JSON"), "{error}");
        assert!(reads.state.lock().await.worker.is_none());
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert!(
            !marker.exists(),
            "invalid worker survived protocol rejection"
        );
    }

    #[tokio::test]
    async fn cancellation_leaves_no_worker_or_stale_response_for_next_call() {
        let _guard = crate::tests::MCP_ROOT_TEST_ENV.lock().await;
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("late-mutation");
        let reads = std::sync::Arc::new(ResidentReads::default());
        reads.state.lock().await.worker = Some(delayed_worker(root.path(), &marker, false).await);
        let task_reads = reads.clone();
        let path = root.path().to_path_buf();
        let request = tokio::spawn(async move {
            task_reads
                .execute(
                    &path,
                    "query",
                    &["query", "--from", "example", "."],
                    Duration::from_secs(5),
                )
                .await
        });
        wait_for_marker(&marker.with_extension("accepted")).await;
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert!(reads.state.lock().await.worker.is_none());
        tokio::time::sleep(Duration::from_millis(900)).await;
        assert!(!marker.exists(), "cancelled resident child survived");
    }

    #[tokio::test]
    async fn delayed_child_control_reaches_the_late_mutation_without_cancellation() {
        let _guard = crate::tests::MCP_ROOT_TEST_ENV.lock().await;
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("late-mutation");
        let mut worker = delayed_worker(root.path(), &marker, false).await;
        let result = tokio::time::timeout(Duration::from_secs(5), worker.request(&["query", "."]))
            .await
            .unwrap();
        assert!(result.unwrap_err().contains("closed"));
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "survived");
        assert!(marker.with_extension("accepted").exists());
    }

    #[tokio::test]
    async fn process_exit_stderr_preserves_authoritative_freshness_error() {
        let _guard = crate::tests::MCP_ROOT_TEST_ENV.lock().await;
        let root = tempfile::tempdir().unwrap();
        let mut command = if cfg!(windows) {
            let mut command = tokio::process::Command::new("powershell.exe");
            command.args(["-NoProfile", "-NonInteractive", "-Command", "$null = [Console]::ReadLine(); [Console]::Error.WriteLine('authoritative freshness required: source changed'); exit 2"]);
            command
        } else {
            let mut command = tokio::process::Command::new("sh");
            command.args(["-c", "read request; printf 'authoritative freshness required: source changed\\n' >&2; exit 2"]);
            command
        };
        command
            .current_dir(root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let worker = Worker::from_child(
            &root.path().canonicalize().unwrap(),
            BinaryIdentity::current(),
            command.spawn().unwrap(),
        )
        .unwrap();
        let reads = ResidentReads::default();
        reads.state.lock().await.worker = Some(worker);
        let error = reads
            .execute(
                root.path(),
                "query",
                &["query", "."],
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
        assert!(
            error.contains("authoritative freshness required"),
            "{error}"
        );
        let envelope = crate::agent_error_for_mcp_in_scope("query", &error, root.path());
        let envelope: serde_json::Value = serde_json::from_str(&envelope).unwrap();
        assert_eq!(
            envelope["error"]["code"], "freshness_required",
            "{envelope}"
        );
        assert!(reads.state.lock().await.worker.is_none());
    }

    #[tokio::test]
    async fn switching_roots_replaces_worker_and_returns_only_the_selected_repository() {
        let _guard = crate::tests::MCP_ROOT_TEST_ENV.lock().await;
        struct RestoreBinary(Option<OsString>);
        impl Drop for RestoreBinary {
            fn drop(&mut self) {
                *crate::TEST_ADEN_BINARY.lock().unwrap() = self.0.take();
            }
        }
        let _binary = RestoreBinary(
            crate::TEST_ADEN_BINARY
                .lock()
                .unwrap()
                .replace(crate::tests::aden_cli_test_binary().into_os_string()),
        );
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        for (root, name) in [(first.path(), "first_only"), (second.path(), "second_only")] {
            std::fs::write(
                root.join("Cargo.toml"),
                "[package]\nname='resident-root'\nversion='0.1.0'\n",
            )
            .unwrap();
            std::fs::write(root.join("lib.rs"), format!("pub fn {name}() {{}}\n")).unwrap();
        }
        let reads = ResidentReads::default();
        let first_body = reads
            .execute_inner(
                first.path(),
                "locate",
                &["locate", "--symbol", "first_only", "."],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        let first_pid = reads
            .state
            .lock()
            .await
            .worker
            .as_ref()
            .expect("current CLI must support resident protocol")
            .pid;
        let first_body: serde_json::Value = serde_json::from_str(&first_body).unwrap();
        assert_eq!(first_body["match_kind"], "definition");
        let second_body = reads
            .execute_inner(
                second.path(),
                "locate",
                &["locate", "--symbol", "second_only", "."],
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        let state = reads.state.lock().await;
        let worker = state.worker.as_ref().unwrap();
        assert_ne!(
            worker.pid, first_pid,
            "root change retained the old process"
        );
        assert_eq!(worker.root, second.path().canonicalize().unwrap());
        drop(state);
        let second_body: serde_json::Value = serde_json::from_str(&second_body).unwrap();
        assert_eq!(second_body["match_kind"], "definition");
        assert_eq!(second_body["items"].as_array().unwrap().len(), 1);
        assert!(
            second_body["items"][0]["anchor"]
                .as_str()
                .unwrap()
                .ends_with("#second_only")
        );
        assert!(!second_body.to_string().contains("first_only"));
    }
}
