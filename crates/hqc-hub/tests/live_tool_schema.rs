//! T42 [verify] (SPEC §8.1): does a Claude Code session whose transcript holds a `ToolSearch`
//! result from before an update of the hqc MCP server see the new tool schema when resumed?
//!
//! Two `claude -p` processes on one session id, in `tmp/spike-t42-schema-<uuid>`. The MCP
//! server `hqc` is a stand-in (`probe.py`, written by the test) that lists the tools of
//! `hqc_proto::mcp::TOOLS`; the first process gets them as before T41 (`hqc_send` and
//! `hqc_ask` without `files`, their descriptions without the files sentences), the second, a
//! `--resume` of the first, gets them as they are now. The first turn loads `hqc_send` with
//! `ToolSearch` and names its parameters; the resumed turn names them again without calling
//! a tool. The folder keeps both stream-json outputs, the server's log and a copy of the
//! transcript (the original under `~/.claude/projects` is removed).
//!
//! Spends the subscription (`claude-haiku-5-5`, `--max-turns 2`), so it runs only with
//! `HQC_LIVE_CLAUDE=1`:
//!
//! ```
//! HQC_LIVE_CLAUDE=1 cargo test -p hqc-hub --test live_tool_schema -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use hqc_proto::mcp::{ASK, SEND, TOOLS};
use serde_json::{json, Value};
use uuid::Uuid;

const MODEL: &str = "claude-haiku-5-5";

const PROBE: &str = r#"
import json, os, sys
tools = json.load(open(os.environ["PROBE_TOOLS"]))
log = open(os.environ["PROBE_LOG"], "a")
for line in sys.stdin:
    m = json.loads(line)
    log.write(os.environ["PROBE_TOOLS"] + " " + str(m.get("method")) + "\n")
    log.flush()
    if "id" not in m:
        continue
    method = m.get("method")
    if method == "initialize":
        res = {"protocolVersion": m["params"].get("protocolVersion", "2025-06-18"),
               "capabilities": {"tools": {}}, "serverInfo": {"name": "hqc", "version": "0"}}
    elif method == "tools/list":
        res = {"tools": tools}
    elif method == "tools/call":
        res = {"content": [{"type": "text", "text": "ok"}]}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": m["id"],
                          "error": {"code": -32601, "message": "no such method"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": res}), flush=True)
"#;

/// The tools as `tools/list` gives them now, or (`old`) as before T41: no `files` in
/// `hqc_send` / `hqc_ask`, and their descriptions end before "To hand over files".
fn tools(old: bool) -> Value {
    Value::Array(
        TOOLS
            .iter()
            .map(|t| {
                let mut l = t.listing();
                if old && (t.name == SEND || t.name == ASK) {
                    l["inputSchema"]["properties"]
                        .as_object_mut()
                        .unwrap()
                        .remove("files");
                    let d = l["description"].as_str().unwrap();
                    let cut = d.find(" To hand over files").unwrap();
                    l["description"] = json!(&d[..cut]);
                }
                l
            })
            .collect(),
    )
}

/// `claude` on `PATH`.
fn claude_on_path() -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join("claude"))
        .find(|p| p.is_file())
        .expect("claude on PATH")
}

/// One `claude -p` in `ws` with the probe listing `tools_file`; its stream-json lines.
fn claude(run: &Path, tools_file: &Path, session: &str, resume: bool, prompt: &str) -> Vec<Value> {
    let mcp = json!({"mcpServers": {"hqc": {
        "command": "/usr/bin/python3",
        "args": ["-I", run.join("probe.py")],
        "env": {"PROBE_TOOLS": tools_file, "PROBE_LOG": run.join("probe.log")},
    }}});
    let mut cmd = Command::new(claude_on_path());
    cmd.current_dir(run.join("ws"))
        .env("ENABLE_TOOL_SEARCH", "true")
        .args(["-p", prompt, "--model", MODEL, "--max-turns", "2"])
        .args(["--output-format", "stream-json", "--verbose"])
        .args(["--mcp-config", &mcp.to_string(), "--strict-mcp-config"])
        .args(["--allowedTools", "mcp__hqc"]);
    if resume {
        cmd.args(["--resume", session]);
    } else {
        cmd.args(["--session-id", session]);
    }
    let out = cmd.output().expect("claude runs");
    let name = if resume {
        "second.jsonl"
    } else {
        "first.jsonl"
    };
    std::fs::write(run.join(name), &out.stdout).unwrap();
    eprintln!("{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn result_text(lines: &[Value]) -> String {
    lines
        .iter()
        .rev()
        .find(|l| l["type"] == "result")
        .map(|l| l["result"].as_str().unwrap_or_default().to_string())
        .unwrap_or_default()
}

/// The stand-in server answers `initialize` and `tools/list` (no claude involved).
#[test]
fn the_probe_lists_the_old_tools() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tmp")
        .join(format!("spike-t42-probe-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("probe.py"), PROBE).unwrap();
    std::fs::write(dir.join("tools.json"), tools(true).to_string()).unwrap();
    let mut child = Command::new("/usr/bin/python3")
        .arg("-I")
        .arg(dir.join("probe.py"))
        .env("PROBE_TOOLS", dir.join("tools.json"))
        .env("PROBE_LOG", dir.join("probe.log"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        for m in [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        ] {
            writeln!(stdin, "{m}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0]["result"]["serverInfo"]["name"], "hqc");
    let send = lines[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == SEND)
        .unwrap()
        .clone();
    assert!(send["inputSchema"]["properties"].get("files").is_none());
    assert!(!send["description"].as_str().unwrap().contains("files"));
}

#[test]
fn live_a_resumed_session_and_a_changed_tool_schema() {
    if std::env::var("HQC_LIVE_CLAUDE").as_deref() != Ok("1") {
        eprintln!("skipped: set HQC_LIVE_CLAUDE=1 to run against the real claude");
        return;
    }
    let run = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tmp")
        .join(format!("spike-t42-schema-{}", Uuid::now_v7()));
    std::fs::create_dir_all(run.join("ws")).unwrap();
    let run = run.canonicalize().unwrap();
    std::fs::write(run.join("probe.py"), PROBE).unwrap();
    let (old, new) = (run.join("tools-old.json"), run.join("tools-new.json"));
    std::fs::write(&old, tools(true).to_string()).unwrap();
    std::fs::write(&new, tools(false).to_string()).unwrap();
    let session = Uuid::new_v4().to_string();

    let first = claude(
        &run,
        &old,
        &session,
        false,
        "Use the ToolSearch tool with the query \"select:mcp__hqc__hqc_send\" to load that \
         tool. Do not call hqc_send itself. Then reply with the names of its parameters, \
         comma-separated, and nothing else.",
    );
    println!("first: {}", result_text(&first));
    let second = claude(
        &run,
        &new,
        &session,
        true,
        "Do not call any tool. Reply with the parameter names of mcp__hqc__hqc_send as its \
         schema shows them to you now, comma-separated; then, on a new line, the last \
         sentence of its description.",
    );
    println!("second: {}", result_text(&second));
    for l in first.iter().chain(&second) {
        if l["type"] == "system" && l["subtype"] == "init" {
            println!("init tools: {}", l["tools"]);
        }
    }

    // The transcript: copied into the run folder, the original removed.
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let projects = home.join(".claude/projects");
    for d in std::fs::read_dir(&projects).unwrap().flatten() {
        let f = d.path().join(format!("{session}.jsonl"));
        if f.is_file() {
            std::fs::copy(&f, run.join("transcript.jsonl")).unwrap();
            std::fs::remove_file(&f).unwrap();
            let _ = std::fs::remove_dir(d.path());
        }
    }
    println!("run folder: {}", run.display());
    assert!(
        run.join("transcript.jsonl").is_file(),
        "the transcript was found"
    );
}
