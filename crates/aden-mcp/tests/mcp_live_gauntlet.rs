// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Standalone MCP stdio product gauntlet (Python-free).
//!
//! Ports `scripts/test-mcp-live.py`: initialize, tool list bounds, ask → asm
//! without budget boilerplate, broad-question guard, and path confinement.
//! Uses threaded stdout/stderr pumps so Windows does not need `select()` on pipes.
//!
//! Requires a sibling or `ADEN_BIN` CLI — `aden-mcp` shells out to `aden`.

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

fn mcp_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aden-mcp"))
}

fn ensure_aden_cli() -> Option<PathBuf> {
    if let Some(aden_bin) = std::env::var_os("ADEN_BIN") {
        return Some(PathBuf::from(aden_bin));
    }
    let sibling = mcp_bin().with_file_name(format!("aden{}", std::env::consts::EXE_SUFFIX));
    if sibling.is_file() {
        return Some(sibling);
    }
    // Fall back to PATH; fail clearly if neither works.
    let probe = Command::new("aden").arg("--version").output();
    assert!(
        probe.map(|o| o.status.success()).unwrap_or(false),
        "aden CLI not found: set ADEN_BIN or build aden next to aden-mcp ({})",
        sibling.display()
    );
    None
}

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout_rx: Receiver<Option<String>>,
    stderr_rx: Receiver<String>,
}

impl McpClient {
    fn spawn(project: &Path, data: &Path, aden_bin: Option<&Path>) -> Self {
        let mut command = Command::new(mcp_bin());
        command.arg(project).env("ADEN_DATA_DIR", data);
        if let Some(aden_bin) = aden_bin {
            command.env("ADEN_BIN", aden_bin);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn aden-mcp");
        let stdout = child.stdout.take().expect("stdout");
        let stderr = child.stderr.take().expect("stderr");
        let stdin = child.stdin.take().expect("stdin");

        let (stdout_tx, stdout_rx) = mpsc::channel();
        let (stderr_tx, stderr_rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if stdout_tx.send(Some(l)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = stdout_tx.send(None);
        });
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if stderr_tx.send(line).is_err() {
                    break;
                }
            }
        });

        Self {
            child,
            stdin,
            stdout_rx,
            stderr_rx,
        }
    }

    fn drain_stderr(&self) {
        while let Ok(line) = self.stderr_rx.try_recv() {
            if !line.trim().is_empty() {
                eprintln!("aden-mcp: {line}");
            }
        }
    }

    fn send(&mut self, message: &Value) {
        let line = serde_json::to_string(message).unwrap();
        writeln!(self.stdin, "{line}").expect("write mcp stdin");
        self.stdin.flush().expect("flush mcp stdin");
    }

    fn receive(&mut self, request_id: u64, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain_stderr();
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.drain_stderr();
                panic!("timed out waiting for MCP response {request_id}");
            }
            match self
                .stdout_rx
                .recv_timeout(remaining.min(Duration::from_millis(500)))
            {
                Ok(Some(line)) => {
                    let message: Value = serde_json::from_str(&line)
                        .unwrap_or_else(|e| panic!("bad json from mcp: {e}\n{line}"));
                    if message.get("id").and_then(|v| v.as_u64()) == Some(request_id) {
                        return message;
                    }
                }
                Ok(None) => panic!("aden-mcp closed stdout"),
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(status) = self.child.try_wait().ok().flatten() {
                        self.drain_stderr();
                        panic!("aden-mcp exited with {status}");
                    }
                }
                Err(RecvTimeoutError::Disconnected) => panic!("aden-mcp stdout pump died"),
            }
        }
    }

    fn tool_call(&mut self, request_id: u64, name: &str, arguments: Value) -> Value {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        }));
        let rpc = self.receive(request_id, Duration::from_secs(60));
        assert!(rpc.get("error").is_none(), "{rpc}");
        let text = rpc["result"]["content"][0]["text"]
            .as_str()
            .expect("tool text content");
        serde_json::from_str(text).unwrap_or_else(|e| panic!("tool json: {e}\n{text}"))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn live_mcp_bounded_ask_asm_and_path_confinement() {
    let aden_bin = ensure_aden_cli();

    let work = tempfile::tempdir().expect("tempdir");
    let project = work.path().join("project");
    let data = work.path().join("data");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"mcp-live-qa\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("main.rs"),
        "/// Entry point exercised over the real MCP transport.\n\
         fn main() { helper(); }\n\
         fn helper() {}\n",
    )
    .unwrap();

    let mut client = McpClient::spawn(&project, &data, aden_bin.as_deref());

    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "aden-live-qa", "version": "1.0" },
        },
    }));
    let initialized = client.receive(1, Duration::from_secs(30));
    assert!(initialized.get("result").is_some(), "{initialized}");
    let instruction_bytes = initialized["result"]
        .get("instructions")
        .and_then(|v| v.as_str())
        .map(|s| s.len())
        .unwrap_or(0);
    assert!(
        instruction_bytes < 1_600,
        "instructions too large: {instruction_bytes}"
    );

    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {},
    }));

    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {},
    }));
    let listed = client.receive(2, Duration::from_secs(30));
    let tools = listed["result"]["tools"].as_array().expect("tools array");
    let names: std::collections::HashSet<_> =
        tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for required in [
        "tree",
        "ask",
        "asm",
        "grep",
        "locate",
        "query",
        "understand",
    ] {
        assert!(
            names.contains(required),
            "missing tool {required}: {names:?}"
        );
    }
    let schemas: std::collections::HashMap<_, _> = tools
        .iter()
        .filter_map(|t| {
            let name = t["name"].as_str()?;
            Some((name, &t["inputSchema"]))
        })
        .collect();
    assert!(
        schemas["ask"]["properties"].get("budget").is_none(),
        "ask must not require budget"
    );
    assert!(schemas["ask"]["properties"].get("strict").is_none());
    assert!(schemas["asm"]["properties"].get("budget").is_none());
    assert!(schemas["asm"]["properties"].get("format").is_none());
    let registry_bytes = serde_json::to_string(&tools).unwrap().len();
    assert!(
        registry_bytes < 5_000,
        "registry too large: {registry_bytes}"
    );

    // Replay the actual startup examples before any discovery call: a known
    // unique symbol must not require tree/grep/locate boilerplate.
    let instructions = initialized["result"]["instructions"].as_str().unwrap();
    eprintln!("startup={instruction_bytes} bytes; registry={registry_bytes} bytes");
    for (id, name, result_key) in [(20, "asm", "documents"), (21, "query", "items")] {
        let prefix = format!("{name}(");
        let example = instructions
            .split_once(&prefix)
            .unwrap_or_else(|| panic!("missing executable {name} startup example"))
            .1
            .split_once(')')
            .unwrap()
            .0;
        let arguments: Value = serde_json::from_str(example).expect("JSON example arguments");
        let guide = include_str!("../../../docs/ai-integration.adoc");
        assert!(guide.contains(&format!("{name}({example})")));
        let mut cli = Command::new(aden_bin.as_deref().unwrap_or_else(|| Path::new("aden")));
        cli.arg(name)
            .current_dir(&project)
            .env("ADEN_DATA_DIR", &data);
        for key in arguments.as_object().unwrap().keys() {
            assert!(schemas[name]["properties"].get(key).is_some());
            let value = arguments[key].as_str().expect("symbol argument");
            cli.arg(format!("--{key}")).arg(value);
            assert!(guide.contains(&format!("aden {name} --{key} {value}")));
        }
        let result = client.tool_call(id, name, arguments);
        assert!(result.get("error").is_none(), "{result}");
        assert!(
            result[result_key].as_array().is_some_and(|a| !a.is_empty()),
            "{result}"
        );
        assert_eq!(
            result["context_receipt"]["freshness"], "current",
            "{result}"
        );
        assert!(result.to_string().contains("helper"), "{result}");
        let output = cli.output().expect("run documented CLI example");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let cli_result: Value = serde_json::from_slice(&output.stdout).expect("CLI JSON");
        assert_eq!(cli_result[result_key], result[result_key]);
    }

    let ask = client.tool_call(
        3,
        "ask",
        serde_json::json!({ "question": "Where is the main entry point?" }),
    );
    assert!(
        ask.get("anchor").and_then(|v| v.as_str()).is_some(),
        "{ask}"
    );
    assert!(
        ask.get("context")
            .and_then(|v| v.as_str())
            .is_some_and(|c| !c.is_empty()),
        "{ask}"
    );
    let ask_receipt = ask["context_receipt"].as_object().expect("receipt");
    for field in [
        "freshness",
        "graph_revision",
        "observed_source_fingerprint",
        "refresh_cause",
    ] {
        assert!(
            ask_receipt
                .get(field)
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty()),
            "missing {field}: {ask}"
        );
    }

    let assembly = client.tool_call(4, "asm", serde_json::json!({ "from": ask["anchor"] }));
    assert!(
        assembly
            .get("documents")
            .and_then(|d| d.as_array())
            .is_some_and(|a| !a.is_empty()),
        "{assembly}"
    );
    assert_eq!(
        assembly["context_receipt"]["freshness"], "current",
        "{assembly}"
    );
    assert_eq!(
        assembly["context_receipt"]["graph_revision"],
        ask["context_receipt"]["graph_revision"]
    );
    assert_eq!(
        assembly["context_receipt"]["observed_source_fingerprint"],
        ask["context_receipt"]["observed_source_fingerprint"]
    );

    let outline = client.tool_call(5, "tree", serde_json::json!({ "symbols": true }));
    assert_eq!(outline["format"], "symbol-outline-v1", "{outline}");
    assert_eq!(outline["result_state"], "complete", "{outline}");
    assert_eq!(outline["truncated"], false, "{outline}");
    assert_eq!(
        outline["symbol_count"], outline["returned_symbol_count"],
        "{outline}"
    );
    let outline_text = outline["outline"].as_str().unwrap_or("");
    assert!(outline_text.contains("main.rs:"), "{outline}");
    assert!(
        outline_text.contains(" main") || outline_text.split_whitespace().any(|w| w == "main"),
        "{outline}"
    );
    assert!(
        outline["symbol_count"].as_u64().unwrap_or(0) >= 1,
        "{outline}"
    );

    let broad = client.tool_call(
        6,
        "ask",
        serde_json::json!({ "question": "Find all security issues across the repository" }),
    );
    assert_eq!(broad["result_state"], "needs_narrowing", "{broad}");
    assert_eq!(broad["question_fit"], "repository_wide", "{broad}");
    assert_eq!(broad["context"], "", "{broad}");
    assert!(
        serde_json::to_string(&broad).unwrap().len() < 2_000,
        "{broad}"
    );

    let escaped = client.tool_call(
        7,
        "grep",
        serde_json::json!({ "pattern": "root", "path": "/etc" }),
    );
    assert_eq!(
        escaped["error"]["code"], "path_outside_workspace",
        "{escaped}"
    );
    assert!(
        escaped["error"]
            .get("recovery")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty()),
        "{escaped}"
    );
}

#[test]
fn live_mcp_ambiguous_asm_requires_exact_candidate() {
    let aden_bin = ensure_aden_cli();

    let work = tempfile::tempdir().expect("tempdir");
    let project = work.path().join("project");
    let data = work.path().join("data");
    let src = project.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"mcp-live-ambiguous\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::write(src.join("first.rs"), "pub fn reset() {}\n").unwrap();
    std::fs::write(src.join("second.rs"), "pub fn reset() {}\n").unwrap();

    let mut client = McpClient::spawn(&project, &data, aden_bin.as_deref());
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "aden-live-ambiguity", "version": "1.0" },
        },
    }));
    let initialized = client.receive(1, Duration::from_secs(30));
    assert!(initialized.get("result").is_some(), "{initialized}");
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {},
    }));

    let ambiguous = client.tool_call(2, "asm", serde_json::json!({ "from": "reset" }));
    assert_eq!(
        ambiguous["error"]["code"], "ambiguous_symbol",
        "{ambiguous}"
    );
    assert_eq!(ambiguous["error"]["safe_to_retry"], true, "{ambiguous}");
    let candidates = ambiguous["error"]["candidates"]
        .as_array()
        .expect("ambiguous candidates");
    assert_eq!(candidates.len(), 2, "{ambiguous}");
    let candidates: Vec<&str> = candidates
        .iter()
        .map(|candidate| candidate.as_str().expect("candidate anchor"))
        .collect();
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.ends_with("#reset"))
    );
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.contains("first.rs"))
    );
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.contains("second.rs"))
    );
    assert_ne!(candidates[0], candidates[1], "{ambiguous}");

    let resolved = client.tool_call(3, "asm", serde_json::json!({ "from": candidates[0] }));
    assert!(
        resolved["documents"]
            .as_array()
            .is_some_and(|documents| !documents.is_empty()),
        "{resolved}"
    );
    assert_eq!(
        resolved["context_receipt"]["freshness"], "current",
        "{resolved}"
    );
    assert!(resolved.to_string().contains(candidates[0]), "{resolved}");
}
