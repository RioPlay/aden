// Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Private, sequential pipe transport for the MCP director's reusable reads.

use std::cell::RefCell;
use std::io::{BufRead, Read, Write};

const PROTOCOL: &str = "aden-read-worker-v1";
const RESIDENT_COMMANDS: &[&str] = &[
    "locate",
    "understand",
    "query",
    "read",
    "grep",
    "tree",
    "search",
    "asm",
];
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
struct CapturedBody {
    body: Option<String>,
    invalid: bool,
}

thread_local! {
    static CAPTURE: RefCell<Option<CapturedBody>> = const { RefCell::new(None) };
}

/// Emit an already serialized machine response. Terminal CLI behavior is kept
/// unchanged; a resident request captures exactly one body instead of stdout.
pub(crate) fn emit_machine_body(body: String) {
    if let Some(body) = capture_body(body) {
        println!("{body}");
    }
}

/// Like [`emit_machine_body`] for bodies that already carry their own line
/// ending: terminal output is printed verbatim, with no added newline.
pub(crate) fn emit_machine_body_raw(body: String) {
    if let Some(body) = capture_body(body) {
        print!("{body}");
    }
}

/// Capture `body` for the active resident request, or hand it back for the
/// terminal when no request is being served.
fn capture_body(body: String) -> Option<String> {
    CAPTURE.with(|capture| {
        let mut capture = capture.borrow_mut();
        let Some(state) = capture.as_mut() else {
            return Some(body);
        };
        if body.len() > MAX_BODY_BYTES || state.body.is_some() {
            state.invalid = true;
        } else {
            state.body = Some(body);
        }
        None
    })
}

pub(super) fn validate_command(cli: &super::Cli) -> Result<(), Box<dyn std::error::Error>> {
    use super::Commands;
    let supported = match &cli.command {
        Commands::Locate { .. }
        | Commands::Understand { .. }
        | Commands::Read { .. }
        | Commands::Grep { .. }
        | Commands::Tree { .. }
        | Commands::Search { .. } => true,
        Commands::Query { format, .. } => format == "json",
        Commands::Asm {
            format,
            out,
            inspect,
            ..
        } => format == "json" && out.is_none() && !inspect,
        _ => false,
    };
    if !supported || cli.project.is_some() || cli.human || cli.verbose {
        return Err("resident worker accepts only quiet JSON locate, understand, query, read, grep, tree, search, and asm reads without --project".into());
    }
    Ok(())
}

fn response_for_request(value: serde_json::Value) -> serde_json::Value {
    let args = value.get("args").and_then(serde_json::Value::as_array);
    let Some(args) = args else {
        return response(false, "", "worker request requires an args array");
    };
    let Some(args) = args
        .iter()
        .map(|arg| arg.as_str().map(std::ffi::OsString::from))
        .collect::<Option<Vec<_>>>()
    else {
        return response(false, "", "worker arguments must be strings");
    };
    CAPTURE.with(|capture| *capture.borrow_mut() = Some(CapturedBody::default()));
    let result = super::real_main_from(
        std::iter::once(std::ffi::OsString::from("aden")).chain(args),
        true,
    );
    let capture = CAPTURE.with(|capture| capture.borrow_mut().take().unwrap_or_default());
    match result {
        Err(error) => response(false, "", &super::command_error_text(error.as_ref())),
        Ok(()) if capture.invalid => response(
            false,
            "",
            "resident response exceeded its body limit or emitted multiple bodies",
        ),
        Ok(()) => match capture.body {
            Some(body) => response(true, &body, ""),
            None => response(
                false,
                "",
                "resident command did not emit a machine response",
            ),
        },
    }
}

fn response(ok: bool, stdout: &str, stderr: &str) -> serde_json::Value {
    serde_json::json!({
        "protocol": PROTOCOL,
        "pid": std::process::id(),
        "ok": ok,
        "stdout": stdout,
        "stderr": stderr,
    })
}

pub(super) fn serve() -> Result<(), Box<dyn std::error::Error>> {
    // No live store handle survives a request. Only immutable graph data can be
    // reused, and the normal command freshness observations still run each time.
    aden_graph::snapshot::set_session_cache_enabled(true);
    aden_index::set_session_cache_enabled(true);
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    writeln!(
        output,
        "{}",
        serde_json::json!({
            "protocol": PROTOCOL,
            "pid": std::process::id(),
            "commands": RESIDENT_COMMANDS,
        })
    )?;
    output.flush()?;
    loop {
        let mut line = Vec::new();
        let count = (&mut input)
            .take((MAX_REQUEST_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        if line.len() > MAX_REQUEST_BYTES || !line.ends_with(b"\n") {
            return Err("resident request exceeded its line limit or was incomplete".into());
        }
        let result = match serde_json::from_slice(&line) {
            Ok(value) => response_for_request(value),
            Err(_) => response(false, "", "worker request is not valid JSON"),
        };
        writeln!(output, "{result}")?;
        output.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_accepts_one_body_and_rejects_multiple_or_oversized_bodies() {
        CAPTURE.with(|capture| *capture.borrow_mut() = Some(CapturedBody::default()));
        emit_machine_body("{}".into());
        let state = CAPTURE.with(|capture| capture.borrow_mut().take().unwrap());
        assert_eq!(state.body.as_deref(), Some("{}"));
        assert!(!state.invalid);
        CAPTURE.with(|capture| *capture.borrow_mut() = Some(CapturedBody::default()));
        emit_machine_body("{}".into());
        emit_machine_body("{}".into());
        assert!(CAPTURE.with(|capture| capture.borrow_mut().take().unwrap().invalid));
        CAPTURE.with(|capture| *capture.borrow_mut() = Some(CapturedBody::default()));
        emit_machine_body("x".repeat(MAX_BODY_BYTES + 1));
        assert!(CAPTURE.with(|capture| capture.borrow_mut().take().unwrap().invalid));
    }

    #[test]
    fn worker_rejects_mutations_and_process_global_options() {
        // Match the production CLI thread: clap's full command tree needs a
        // larger stack than Windows' default test worker provides.
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(|| {
                for args in [
                    vec!["gen", "."],
                    vec!["locate", "--symbol", "foo", "--project", "."],
                    vec!["locate", "--symbol", "foo", "--human"],
                    vec!["locate", "--symbol", "foo", "--verbose"],
                ] {
                    let response = response_for_request(serde_json::json!({"args": args}));
                    assert_eq!(response["ok"], false, "{args:?}");
                    assert!(
                        response["stderr"]
                            .as_str()
                            .unwrap()
                            .contains("resident worker accepts")
                    );
                }
                assert_eq!(
                    response_for_request(serde_json::json!({"args": [3]}))["ok"],
                    false
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
