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

const ESSENTIAL_REGISTRY_MAX_BYTES: usize = 5_000;

fn standard_ask_budget_schema_is_valid(schema: &Value) -> bool {
    schema["properties"]["budget"]["type"] == "integer"
        && schema["properties"]["budget"]["default"] == 4096
        && schema["required"] == serde_json::json!(["question"])
}

fn essential_registry_size_within_limit(tools: &[Value]) -> Option<usize> {
    let size = serde_json::to_vec(tools).ok()?.len();
    (size < ESSENTIAL_REGISTRY_MAX_BYTES).then_some(size)
}

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout_rx: Receiver<Option<String>>,
    stderr_rx: Receiver<String>,
}

impl McpClient {
    fn spawn(project: &Path, data: &Path, aden_bin: Option<&Path>, surface: &str) -> Self {
        let mut command = Command::new(mcp_bin());
        command
            .arg(project)
            .args(["--surface", surface])
            .env("ADEN_DATA_DIR", data)
            // Exercise startup argument precedence against a broader inherited
            // surface. Essential/Standard assertions fail if argv is ignored.
            .env("ADEN_MCP_SURFACE", "full");
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
        let result =
            self.tool_call_once(request_id, name, arguments.clone(), Duration::from_secs(60));
        if result["error"]["code"] != "index_generation_pending" {
            return result;
        }
        // Cold reads may return the documented recoverable generation state.
        // Observe completion before retrying once; never hide arbitrary errors.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "generation did not complete: {result}"
            );
            let status = self.tool_call_once(
                request_id + 10_000,
                "generation_status",
                serde_json::json!({}),
                remaining,
            );
            match status["state"].as_str() {
                Some("completed") => {
                    return self.tool_call_once(
                        request_id,
                        name,
                        arguments,
                        deadline.saturating_duration_since(Instant::now()),
                    );
                }
                Some("queued" | "running" | "cancelling") => {
                    thread::sleep(Duration::from_millis(50))
                }
                _ => panic!("generation did not succeed: {status}"),
            }
        }
    }

    fn tool_call_once(
        &mut self,
        request_id: u64,
        name: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Value {
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        }));
        let rpc = self.receive(request_id, timeout);
        assert!(rpc.get("error").is_none(), "{rpc}");
        assert!(
            rpc["result"].get("resultType").is_none(),
            "legacy tool responses must preserve their negotiated wire shape: {rpc}"
        );
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
fn live_mcp_essential_registry_remains_compact_and_omits_standard_tools() {
    let aden_bin = ensure_aden_cli();
    let work = tempfile::tempdir().expect("tempdir");
    let project = work.path().join("project");
    let data = work.path().join("data");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&data).unwrap();

    let mut client = McpClient::spawn(&project, &data, aden_bin.as_deref(), "essential");
    client.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "essential-registry-qa", "version": "1.0" },
        },
    }));
    assert!(
        client
            .receive(1, Duration::from_secs(30))
            .get("result")
            .is_some()
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
    let names: std::collections::HashSet<_> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    let expected: std::collections::HashSet<_> = [
        "read",
        "tree",
        "grep",
        "locate",
        "understand",
        "asm",
        "query",
        "generation_status",
    ]
    .into_iter()
    .collect();
    assert_eq!(names, expected, "Essential tool set changed: {names:?}");
    let asm = tools.iter().find(|tool| tool["name"] == "asm").unwrap();
    assert!(
        asm["inputSchema"]["properties"].get("format").is_none(),
        "Essential asm must omit the Standard format tuning field"
    );
    let registry_bytes = essential_registry_size_within_limit(tools)
        .expect("Essential registry must remain below its live compactness budget");
    assert!(
        registry_bytes < ESSENTIAL_REGISTRY_MAX_BYTES,
        "Essential registry size helper returned an invalid success"
    );
}

#[test]
fn live_mcp_grep_offers_one_exact_call_to_find_callers() {
    let aden_bin = ensure_aden_cli();
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("project with spaces");
    let data = work.path().join("data");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"grep-next-qa\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("app.rs"),
        "fn target() {}\nfn caller() { target(); }\n",
    )
    .unwrap();
    let mut client = McpClient::spawn(&project, &data, aden_bin.as_deref(), "essential");
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-03-26", "capabilities": {},
            "clientInfo": { "name": "grep-next-qa", "version": "1.0" } }
    }));
    assert!(
        client
            .receive(1, Duration::from_secs(30))
            .get("result")
            .is_some()
    );
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "notifications/initialized", "params": {}
    }));
    let hit = client.tool_call(2, "grep", serde_json::json!({"pattern": "target"}));
    let anchor = hit["matches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["symbol"] == "target")
        .expect("exact symbol source hit")["anchor"]
        .as_str()
        .unwrap();
    let actions = hit["next_actions"].as_array().expect("navigation actions");
    assert_eq!(actions.len(), 1, "{hit}");
    assert_eq!(actions[0]["tool"], "query", "{hit}");
    assert_eq!(actions[0]["arguments"]["backlinks"], anchor);
    let callers = client.tool_call(3, "query", actions[0]["arguments"].clone());
    assert_eq!(
        callers["relationship"]["direction"], "incoming",
        "{callers}"
    );
    assert!(
        callers["items"].as_array().unwrap().iter().any(|item| {
            item["anchor"]
                .as_str()
                .is_some_and(|anchor| anchor.ends_with("#caller"))
        }),
        "{callers}"
    );

    let missing = client.tool_call(4, "grep", serde_json::json!({"pattern": "absent_symbol"}));
    assert_eq!(missing["total"], 0, "{missing}");
    assert!(
        missing["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|action| {
                action["tool"] != "query" || action["arguments"].get("backlinks").is_none()
            }),
        "a missing source hit must not invent a backlinks target: {missing}"
    );
}

#[test]
fn live_mcp_tree_file_mode_includes_documents_and_recovers_to_symbols() {
    let aden_bin = ensure_aden_cli();
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("project");
    let data = work.path().join("data");
    std::fs::create_dir_all(project.join("docs with spaces")).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"tree-mode-qa\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    std::fs::write(project.join("app.rs"), "fn visible_symbol() {}\n").unwrap();
    std::fs::write(
        project.join("docs with spaces/guide.md"),
        "# Guide\nUsage\n",
    )
    .unwrap();
    std::fs::write(project.join("docs with spaces/settings.json"), "{}\n").unwrap();
    let mut client = McpClient::spawn(&project, &data, aden_bin.as_deref(), "essential");
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2025-03-26", "capabilities": {},
            "clientInfo": { "name": "tree-mode-qa", "version": "1.0" } }
    }));
    assert!(
        client
            .receive(1, Duration::from_secs(30))
            .get("result")
            .is_some()
    );
    client.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "notifications/initialized", "params": {}
    }));
    let files = client.tool_call(2, "tree", serde_json::json!({ "symbols": false }));
    assert_eq!(files["format"], "files-v1", "{files}");
    assert_eq!(files["result_state"], "complete", "{files}");
    let outline = files["outline"].as_str().unwrap();
    for expected in [
        "app.rs",
        "docs with spaces/guide.md",
        "docs with spaces/settings.json",
    ] {
        assert!(outline.contains(expected), "missing {expected}: {files}");
    }
    assert!(!outline.contains("visible_symbol"), "{files}");
    assert_eq!(files["file_count"], files["returned_file_count"]);
    let scoped = client.tool_call(
        3,
        "tree",
        serde_json::json!({
            "symbols": false, "path": "docs with spaces"
        }),
    );
    assert_eq!(scoped["format"], "files-v1", "{scoped}");
    assert_eq!(scoped["file_count"], 2, "{scoped}");
    assert!(
        scoped["outline"]
            .as_str()
            .unwrap()
            .contains("docs with spaces/guide.md")
    );
    assert!(!scoped["outline"].as_str().unwrap().contains("app.rs"));
    let symbols = client.tool_call(4, "tree", serde_json::json!({}));
    assert_eq!(symbols["format"], "symbol-outline-v1", "{symbols}");
    assert!(
        symbols["outline"]
            .as_str()
            .unwrap()
            .contains("visible_symbol")
    );
    assert!(!symbols["outline"].as_str().unwrap().contains("guide.md"));
    let files_again = client.tool_call(5, "tree", serde_json::json!({ "symbols": false }));
    assert_eq!(files_again["outline"], files["outline"]);

    for index in 0..400 {
        let directory = project.join(format!("group-{index:04}-abcdefghijklmnop"));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("guide.md"), "# Group guide\n").unwrap();
    }
    let clipped = client.tool_call(6, "tree", serde_json::json!({ "symbols": false }));
    assert_eq!(clipped["result_state"], "truncated", "{clipped}");
    assert!(
        clipped["returned_file_count"].as_u64().unwrap() < clipped["file_count"].as_u64().unwrap()
    );
    let actions = clipped["next_actions"].as_array().unwrap();
    assert!(!actions.is_empty(), "{clipped}");
    for (index, action) in actions.iter().enumerate() {
        assert_eq!(action["arguments"]["symbols"], false);
        let replayed = client.tool_call(7 + index as u64, "tree", action["arguments"].clone());
        assert_eq!(replayed["format"], "files-v1", "{replayed}");
        assert_eq!(replayed["result_state"], "complete", "{replayed}");
        assert!(replayed["file_count"].as_u64().unwrap() > 0, "{replayed}");
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
         fn helper() { leaf(); }\n\
         fn leaf() {}\n",
    )
    .unwrap();

    // This journey uses `ask`, which is opt-in on the Standard surface.
    let mut client = McpClient::spawn(&project, &data, aden_bin.as_deref(), "standard");

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
    assert_eq!(initialized["result"]["protocolVersion"], "2025-03-26");
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
        "read",
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
        standard_ask_budget_schema_is_valid(schemas["ask"]),
        "Standard ask budget must be an optional integer defaulting to 4096: {}",
        schemas["ask"]
    );
    assert!(schemas["ask"]["properties"].get("strict").is_some());
    assert!(schemas["ask"]["properties"].get("require_fresh").is_some());
    for key in ["depth", "edge_types", "budget"] {
        assert!(schemas["asm"]["properties"].get(key).is_some(), "asm.{key}");
    }
    for key in ["depth", "edge_type", "max_results"] {
        assert!(
            schemas["query"]["properties"].get(key).is_some(),
            "query.{key}"
        );
    }
    assert_eq!(schemas["asm"]["properties"]["format"]["default"], "json");
    let registry_bytes = serde_json::to_vec(&tools).unwrap().len();

    // Direct source evidence must work before graph discovery or store creation.
    let read = client.tool_call(
        19,
        "read",
        serde_json::json!({
            "file": "main.rs", "start_line": 2, "end_line": 2,
        }),
    );
    assert!(read.get("error").is_none(), "{read}");
    let source = read;
    assert_eq!(source["source"]["text"], "fn main() { helper(); }\n");
    assert_eq!(source["observation"], "file_bytes_read");
    assert!(source["source"]["source_hash"].is_string());
    assert!(source.get("context_receipt").is_none());
    assert_eq!(std::fs::read_dir(&data).unwrap().count(), 0);

    // Replay the actual startup examples before any discovery call: a known
    // unique symbol must not require tree/grep/locate boilerplate.
    let instructions = initialized["result"]["instructions"].as_str().unwrap();
    eprintln!("startup={instruction_bytes} bytes; Standard registry={registry_bytes} bytes");
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

    // Exercise each explicit traversal control through the real transport.
    let symbols = |result: &Value| -> std::collections::BTreeSet<String> {
        assert!(result.get("error").is_none(), "{result}");
        result["items"]
            .as_array()
            .expect("query items")
            .iter()
            .map(|item| {
                item["anchor"]
                    .as_str()
                    .unwrap()
                    .rsplit('#')
                    .next()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    };
    let shallow = client.tool_call(
        30,
        "query",
        serde_json::json!({
            "from":"main", "edge_type":"Calls", "depth":1, "max_results":10,
        }),
    );
    assert_eq!(
        symbols(&shallow),
        ["main", "helper"].into_iter().map(str::to_owned).collect()
    );
    let deep = client.tool_call(
        31,
        "query",
        serde_json::json!({
            "from":"main", "edge_type":"Calls", "depth":2, "max_results":10,
        }),
    );
    assert_eq!(
        symbols(&deep),
        ["main", "helper", "leaf"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    let incoming = client.tool_call(
        32,
        "query",
        serde_json::json!({
            "backlinks":"helper", "edge_type":"Calls", "depth":1, "max_results":10,
        }),
    );
    assert_eq!(
        symbols(&incoming),
        ["helper", "main"].into_iter().map(str::to_owned).collect()
    );
    assert_eq!(incoming["relationship"]["direction"], "incoming");
    let filtered = client.tool_call(
        33,
        "query",
        serde_json::json!({
            "from":"main", "edge_type":"Mutates", "depth":2, "max_results":10,
        }),
    );
    assert_eq!(
        symbols(&filtered),
        ["main"].into_iter().map(str::to_owned).collect()
    );
    let limited = client.tool_call(
        34,
        "query",
        serde_json::json!({
            "from":"main", "edge_type":"Calls", "depth":2, "max_results":1,
        }),
    );
    assert_eq!(
        symbols(&limited),
        ["main"].into_iter().map(str::to_owned).collect()
    );
    assert_eq!(limited["truncated"], true);
    let zero_hops = client.tool_call(
        35,
        "query",
        serde_json::json!({
            "from":"main", "edge_type":"Calls", "depth":0, "max_results":10,
        }),
    );
    assert_eq!(
        symbols(&zero_hops),
        ["main"].into_iter().map(str::to_owned).collect()
    );
    let assembly = client.tool_call(
        36,
        "asm",
        serde_json::json!({
            "from":"main", "edge_types":"Calls", "depth":0, "budget":2048,
        }),
    );
    assert!(assembly.get("error").is_none(), "{assembly}");
    assert!(
        assembly["documents"]
            .as_array()
            .is_some_and(|items| items.len() == 1),
        "{assembly}"
    );
    assert!(
        serde_json::to_vec(&assembly).unwrap().len() <= 2048 * 4,
        "{assembly}"
    );
    let assembly_filtered = client.tool_call(
        37,
        "asm",
        serde_json::json!({
            "from":"main", "edge_types":"Mutates", "depth":2, "budget":2048,
        }),
    );
    assert!(
        assembly_filtered["documents"]
            .as_array()
            .is_some_and(|items| items.len() == 1),
        "{assembly_filtered}"
    );
    let outgoing = client.tool_call(
        38,
        "query",
        serde_json::json!({
            "impact":"main", "edge_type":"Calls", "depth":1, "max_results":10,
        }),
    );
    assert_eq!(
        symbols(&outgoing),
        ["main", "helper"].into_iter().map(str::to_owned).collect()
    );
    assert_eq!(outgoing["relationship"]["direction"], "outgoing");
    let impact_filtered = client.tool_call(
        39,
        "query",
        serde_json::json!({
            "impact":"main", "edge_type":"Mutates", "depth":2, "max_results":10,
        }),
    );
    assert_eq!(
        symbols(&impact_filtered),
        ["main"].into_iter().map(str::to_owned).collect()
    );
    let invalid = client.tool_call(40, "query", serde_json::json!({"from":"main", "depth":-1}));
    assert!(
        invalid.get("error").is_some(),
        "negative depth accepted: {invalid}"
    );

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
fn registry_contract_checks_reject_invalid_standard_schema_and_oversized_essential() {
    let valid_schema = serde_json::json!({
        "properties": { "budget": { "type": "integer", "default": 4096 } },
        "required": ["question"]
    });
    assert!(standard_ask_budget_schema_is_valid(&valid_schema));

    let mut required_budget = valid_schema.clone();
    required_budget["required"] = serde_json::json!(["question", "budget"]);
    assert!(!standard_ask_budget_schema_is_valid(&required_budget));

    let mut wrong_default = valid_schema;
    wrong_default["properties"]["budget"]["default"] = serde_json::json!(2048);
    assert!(!standard_ask_budget_schema_is_valid(&wrong_default));

    let compact = vec![serde_json::json!({ "name": "read" })];
    assert!(essential_registry_size_within_limit(&compact).is_some());

    let oversized = vec![serde_json::json!({
        "name": "read",
        "description": "x".repeat(ESSENTIAL_REGISTRY_MAX_BYTES),
    })];
    assert!(essential_registry_size_within_limit(&oversized).is_none());
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

    let mut client = McpClient::spawn(&project, &data, aden_bin.as_deref(), "essential");
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
