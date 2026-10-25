//! T30 (SPEC §5.2, §7.2): the hqc MCP server `hqc-worker mcp` driven over stdio by a scripted
//! runtime, the daemon's socket and its token check, `mcp.call` / `mcp.result` / `mcp.cancel`
//! with a fake hub. T31 (SPEC §7, §12): `hqc_attach` reads and uploads the file on the worker
//! before its `mcp.call`. The worker runs in process with the fake adapter; the MCP server is the
//! real binary.

mod common;

use std::process::Stdio;
use std::time::Duration;

use common::*;
use hqc_worker::adapter::fake::FakeAdapters;
use hqc_worker::outbox::{Outbox, FRAME_MCP_CALL};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use uuid::Uuid;

const TOKEN_A: &str = "hqcs_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TOKEN_B: &str = "hqcs_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// A runtime's side of the MCP stdio pipe.
struct Runtime {
    child: Child,
    stdin: Option<ChildStdin>,
    out: Lines<BufReader<ChildStdout>>,
}

impl Runtime {
    fn spawn(env: &Env, token: Option<&str>) -> Self {
        let mut cmd = tokio::process::Command::new(binary());
        cmd.arg("mcp")
            .arg("--socket")
            .arg(env.mcp_socket())
            .env_remove(hqc_worker::mcp::TOKEN_ENV)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(t) = token {
            cmd.env(hqc_worker::mcp::TOKEN_ENV, t);
        }
        let mut child = cmd.spawn().expect("hqc-worker mcp starts");
        let stdin = child.stdin.take();
        let out = BufReader::new(child.stdout.take().unwrap()).lines();
        Self { child, stdin, out }
    }

    async fn send(&mut self, v: Value) {
        let w = self.stdin.as_mut().unwrap();
        w.write_all(format!("{v}\n").as_bytes()).await.unwrap();
        w.flush().await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let line = tokio::time::timeout(WAIT, self.out.next_line())
            .await
            .expect("no message from the MCP server")
            .unwrap()
            .expect("MCP server closed stdout");
        serde_json::from_str(&line).unwrap()
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        let r = self.recv().await;
        assert_eq!(r["id"], id, "{r}");
        r
    }

    async fn initialize(&mut self) {
        let r = self
            .request(
                1,
                "initialize",
                json!({"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "claude-code", "version": "2.1.310"}}),
            )
            .await;
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18", "{r}");
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
    }

    async fn call(&mut self, id: u64, args: Value) {
        self.call_tool(id, "hqc_list_sessions", args).await;
    }

    async fn call_tool(&mut self, id: u64, tool: &str, args: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                         "params": {"name": tool, "arguments": args}}))
            .await;
    }
}

fn start(env: &Env, sid: Uuid, token: &str) -> Value {
    let mut p = env.start_payload(sid);
    p["session_token"] = json!(token);
    p
}

async fn start_session(c: &mut HubConn, env: &Env, sid: Uuid, token: &str) {
    let id = c.frame("session.start", start(env, sid, token)).await;
    let ack = c.expect("ack").await;
    assert_eq!(ack["id"], id.as_str(), "{ack}");
}

fn result_text(r: &Value) -> (bool, String) {
    (
        r["result"]["isError"].as_bool().unwrap(),
        r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string(),
    )
}

/// No file under `dir` contains `needle`.
fn not_on_disk(dir: &std::path::Path, needle: &str) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_file() {
            let b = std::fs::read(&p).unwrap();
            assert!(
                !b.windows(needle.len()).any(|w| w == needle.as_bytes()),
                "{} holds the token",
                p.display()
            );
        }
    }
}

async fn setup() -> (Env, FakeHub, RunningWorker, HubConn, Uuid) {
    let env = Env::new();
    let mut hub = FakeHub::start().await;
    let adapters = FakeAdapters::new();
    let mut w = start_worker(&env, &hub.url(), &adapters);
    let mut c = hub.accept().await;
    c.handshake().await;
    w.wait_status(|s| matches!(s, hqc_worker::connection::Status::Connected { .. }))
        .await;
    let sid = Uuid::now_v7();
    start_session(&mut c, &env, sid, TOKEN_A).await;
    (env, hub, w, c, sid)
}

#[tokio::test]
async fn a_call_goes_to_the_hub_and_back() {
    let (env, _hub, w, mut c, _sid) = setup().await;
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(env.mcp_socket())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);

    let mut rt = Runtime::spawn(&env, Some(TOKEN_A));
    rt.initialize().await;
    let r = rt.request(2, "tools/list", json!({})).await;
    let names: Vec<&str> = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "hqc_list_sessions",
            "hqc_attach",
            "hqc_send",
            "hqc_ask",
            "hqc_schedule"
        ],
        "only tools that landed"
    );
    assert_eq!(
        r["result"]["tools"][0]["inputSchema"]["type"], "object",
        "{r}"
    );

    rt.call(3, json!({"project": "hqc"})).await;
    let f = c.expect("mcp.call").await;
    let p = &f["payload"];
    assert_eq!(p["session_token"], TOKEN_A, "{f}");
    assert_eq!(p["tool"], "hqc_list_sessions");
    assert_eq!(p["args"], json!({"project": "hqc"}));
    let rid = p["request_id"].as_str().unwrap().to_string();
    assert!(rid.parse::<Uuid>().is_ok());
    // The call sits in the outbox until answered, without its token.
    let ob = Outbox::open(&env.outbox()).unwrap();
    let e = ob.after(0).unwrap();
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].frame, FRAME_MCP_CALL);
    assert!(e[0].event.get("session_token").is_none());
    not_on_disk(&env.outbox(), TOKEN_A);

    let sessions = json!({"sessions": [{"address": "hqc/x", "title": "X", "status": "running", "current": true}],
                          "truncated": false});
    c.send(json!({"id": f["id"], "type": "mcp.result",
                  "payload": {"request_id": rid, "result": sessions}}))
        .await;
    let r = rt.recv().await;
    assert_eq!(r["id"], 3);
    let (is_err, text) = result_text(&r);
    assert!(!is_err, "{r}");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), sessions);
    let deadline = std::time::Instant::now() + WAIT;
    while !ob.is_empty().unwrap() {
        assert!(
            std::time::Instant::now() < deadline,
            "answered call left the outbox"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // An error comes back as a tool result with `isError`.
    rt.call(4, json!({"project": "nope"})).await;
    let f = c.expect("mcp.call").await;
    c.send(json!({"id": f["id"], "type": "mcp.result",
                  "payload": {"request_id": f["payload"]["request_id"],
                              "error": {"code": "not_found", "message": "no project nope"}}}))
        .await;
    let r = rt.recv().await;
    assert_eq!(result_text(&r), (true, "not_found: no project nope".into()));

    // A deferred result (its own frame) is acked; nobody waits for this one.
    let id = c
        .frame(
            "mcp.result",
            json!({"request_id": Uuid::now_v7(), "result": {}}),
        )
        .await;
    let ack = c.expect("ack").await;
    assert_eq!(ack["id"], id.as_str());

    // Protocol odds and ends.
    let r = rt.request(5, "resources/list", json!({})).await;
    assert_eq!(r["error"]["code"], -32601);
    rt.send(json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call",
                   "params": {"name": "hqc_spawn", "arguments": {}}}))
        .await;
    let r = rt.recv().await;
    assert_eq!(r["error"]["code"], -32602, "no stubs: {r}");
    let r = rt.request(7, "ping", json!({})).await;
    assert_eq!(r["result"], json!({}));
    drop(rt);
    w.stop().await;
}

#[tokio::test]
async fn a_wrong_or_missing_token_is_refused() {
    let (env, _hub, w, mut c, _sid) = setup().await;
    for (token, want) in [
        (
            Some(TOKEN_B),
            "unauthorized: the session token is not one of a session this worker runs",
        ),
        (None, "unauthorized: HQC_SESSION_TOKEN is not set"),
    ] {
        let mut rt = Runtime::spawn(&env, token);
        rt.initialize().await;
        rt.call(2, json!({})).await;
        let r = rt.recv().await;
        assert_eq!(result_text(&r), (true, want.into()));
    }
    assert!(c
        .maybe("mcp.call", Duration::from_millis(300))
        .await
        .is_none());
    w.stop().await;
}

#[tokio::test]
async fn cancelled_and_eof_calls_send_mcp_cancel() {
    let (env, _hub, w, mut c, _sid) = setup().await;
    let mut rt = Runtime::spawn(&env, Some(TOKEN_A));
    rt.initialize().await;

    rt.call(2, json!({})).await;
    let f = c.expect("mcp.call").await;
    let rid = f["payload"]["request_id"].clone();
    rt.send(
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                   "params": {"requestId": 2, "reason": "timeout"}}),
    )
    .await;
    let x = c.expect("mcp.cancel").await;
    assert_eq!(
        x["payload"],
        json!({"request_id": rid, "reason": "cancelled"})
    );
    // A late answer reaches nobody, and the cancelled call gets no response (MCP).
    c.send(json!({"id": f["id"], "type": "mcp.result",
                  "payload": {"request_id": rid, "result": {}}}))
        .await;
    let r = rt.request(3, "ping", json!({})).await;
    assert_eq!(r["result"], json!({}));

    rt.call(4, json!({})).await;
    let f = c.expect("mcp.call").await;
    let rid = f["payload"]["request_id"].clone();
    drop(rt.stdin.take());
    let status = tokio::time::timeout(WAIT, rt.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "{status}");
    let x = c.expect("mcp.cancel").await;
    assert_eq!(x["payload"], json!({"request_id": rid, "reason": "eof"}));
    w.stop().await;
}

#[tokio::test]
async fn calls_carry_the_latest_token_until_the_agent_stops() {
    let (env, _hub, w, mut c, sid) = setup().await;
    let mut rt = Runtime::spawn(&env, Some(TOKEN_A));
    rt.initialize().await;

    // A reconnect's `session.start` for the running agent brings a new token: the MCP
    // process keeps authenticating with its own, the hub gets the new one.
    start_session(&mut c, &env, sid, TOKEN_B).await;
    rt.call(2, json!({})).await;
    let f = c.expect("mcp.call").await;
    assert_eq!(f["payload"]["session_token"], TOKEN_B);
    c.send(json!({"id": f["id"], "type": "mcp.result",
                  "payload": {"request_id": f["payload"]["request_id"], "result": {"ok": 1}}}))
        .await;
    assert!(!result_text(&rt.recv().await).0);
    let mut fresh = Runtime::spawn(&env, Some(TOKEN_B));
    fresh.initialize().await;
    fresh.call(2, json!({})).await;
    assert_eq!(
        result_text(&fresh.recv().await).1,
        "unauthorized: the session token is not one of a session this worker runs",
        "only the process token opens the socket"
    );

    // A call in flight when the agent stops fails; later ones are refused.
    rt.call(3, json!({})).await;
    c.expect("mcp.call").await;
    let id = c.frame("session.kill", json!({"session_id": sid})).await;
    let ack = c.expect("ack").await;
    assert_eq!(ack["id"], id.as_str());
    let (is_err, text) = result_text(&rt.recv().await);
    assert!(is_err && text.starts_with("unavailable:"), "{text}");
    rt.call(4, json!({})).await;
    assert_eq!(
        result_text(&rt.recv().await),
        (
            true,
            "unauthorized: the session's agent no longer runs".into()
        )
    );
    w.stop().await;
}

#[tokio::test]
async fn a_call_waits_in_the_outbox_across_a_reconnect() {
    let (env, mut hub, w, c, sid) = setup().await;
    let mut rt = Runtime::spawn(&env, Some(TOKEN_A));
    rt.initialize().await;
    c.close().await;
    rt.call(2, json!({})).await;

    let mut c = hub.accept().await;
    c.handshake().await;
    let f = c.expect("mcp.call").await;
    assert_eq!(f["payload"]["session_token"], TOKEN_A);
    c.send(json!({"id": f["id"], "type": "mcp.result",
                  "payload": {"request_id": f["payload"]["request_id"], "result": {"n": 2}}}))
        .await;
    let (is_err, text) = result_text(&rt.recv().await);
    assert!(!is_err);
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({"n": 2})
    );
    let _ = sid;
    w.stop().await;
}

/// T32: a deferred call (`hqc_ask`) is acked by the hub, leaves the outbox and waits; its
/// `mcp.result` comes later as a frame of its own, also on a new connection, and reaches the
/// runtime.
#[tokio::test]
async fn a_deferred_answer_comes_after_a_reconnect() {
    let (env, mut hub, w, mut c, _sid) = setup().await;
    let mut rt = Runtime::spawn(&env, Some(TOKEN_A));
    rt.initialize().await;
    rt.send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "hqc_ask",
                              "arguments": {"text": "green?", "session": "hqc/ci"}}}))
        .await;
    let f = c.expect("mcp.call").await;
    assert_eq!(f["payload"]["tool"], "hqc_ask");
    let rid = f["payload"]["request_id"].clone();
    c.send(json!({"id": f["id"], "type": "ack", "payload": {}}))
        .await;
    let ob = Outbox::open(&env.outbox()).unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    while !ob.is_empty().unwrap() {
        assert!(
            std::time::Instant::now() < deadline,
            "acked call left the outbox"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    c.close().await;

    let mut c = hub.accept().await;
    c.handshake().await;
    assert!(
        c.maybe("mcp.call", Duration::from_millis(300))
            .await
            .is_none(),
        "not sent again"
    );
    let id = c
        .frame(
            "mcp.result",
            json!({"request_id": rid, "result": {"from": "hqc/ci", "reply": "yes"}}),
        )
        .await;
    let ack = c.expect("ack").await;
    assert_eq!(ack["id"], id.as_str());
    let r = rt.recv().await;
    assert_eq!(r["id"], 2);
    let (is_err, text) = result_text(&r);
    assert!(!is_err, "{r}");
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({"from": "hqc/ci", "reply": "yes"})
    );
    w.stop().await;
}

/// A call left in the outbox by a previous run of the worker has no caller any more: it is
/// dropped, not sent (the token is not on disk to send it with anyway).
#[tokio::test]
async fn a_call_of_a_previous_run_is_not_sent() {
    let env = Env::new();
    let stale = Uuid::now_v7();
    {
        let ob = Outbox::open(&env.outbox()).unwrap();
        ob.push_frame(
            stale,
            FRAME_MCP_CALL,
            &json!({"request_id": stale, "tool": "hqc_list_sessions", "args": {}}),
        )
        .unwrap();
    }
    let mut hub = FakeHub::start().await;
    let w = start_worker(&env, &hub.url(), &FakeAdapters::new());
    let mut c = hub.accept().await;
    c.handshake().await;
    assert!(c
        .maybe("mcp.call", Duration::from_millis(500))
        .await
        .is_none());
    assert!(Outbox::open(&env.outbox()).unwrap().is_empty().unwrap());
    w.stop().await;
}

#[tokio::test]
async fn attach_uploads_the_file_before_the_call() {
    let (env, hub, w, mut c, sid) = setup().await;
    let ws = env.workspace();
    std::fs::write(ws.join("sub/shot.png"), b"\x89PNG small").unwrap();
    let mut rt = Runtime::spawn(&env, Some(TOKEN_A));
    rt.initialize().await;

    // Relative to the working directory (here the workspace itself).
    rt.call_tool(
        2,
        "hqc_attach",
        json!({"path": "sub/shot.png", "caption": "The layout", "kind": "image"}),
    )
    .await;
    let f = c.expect("mcp.call").await;
    let ups = hub.artifacts.uploads();
    assert_eq!(ups.len(), 1, "uploaded before the call");
    let up = &ups[0];
    assert_eq!(up.body, b"\x89PNG small");
    assert_eq!(up.headers["authorization"], format!("Bearer {TOKEN}"));
    assert_eq!(up.headers["x-hqc-session-id"], sid.to_string());
    assert_eq!(up.headers["x-hqc-artifact-name"], "shot.png");
    assert_eq!(up.headers["x-hqc-artifact-kind"], "image");
    assert!(!up.headers.values().any(|v| v.contains(TOKEN_A)));
    let p = &f["payload"];
    assert_eq!(p["tool"], "hqc_attach");
    assert_eq!(
        p["args"],
        json!({"artifact_id": up.id, "caption": "The layout"}),
        "the hub gets the artifact, not the path"
    );
    let result = json!({"event_id": Uuid::now_v7(), "artifact_id": up.id, "name": "shot.png", "kind": "image"});
    c.send(json!({"id": f["id"], "type": "mcp.result",
                  "payload": {"request_id": p["request_id"], "result": result}}))
        .await;
    let r = rt.recv().await;
    let (is_err, text) = result_text(&r);
    assert!(!is_err, "{r}");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), result);

    // An absolute path and another name.
    let abs = ws.join("sub/shot.png").display().to_string();
    rt.call_tool(3, "hqc_attach", json!({"path": abs, "name": "layout.png"}))
        .await;
    let f = c.expect("mcp.call").await;
    let ups = hub.artifacts.uploads();
    assert_eq!(ups[1].headers["x-hqc-artifact-name"], "layout.png");
    assert!(!ups[1].headers.contains_key("x-hqc-artifact-kind"));
    assert_eq!(f["payload"]["args"], json!({"artifact_id": ups[1].id}));

    // Refused on the worker: nothing is uploaded and no call goes to the hub.
    let outside = env.dir.path().join("secret.txt");
    std::fs::write(&outside, b"secret").unwrap();
    std::os::unix::fs::symlink(&outside, ws.join("link.txt")).unwrap();
    std::fs::write(env.root().join("beside.txt"), b"x").unwrap();
    let cases = [
        (
            json!({"path": outside.display().to_string()}),
            "invalid_args: ",
        ),
        (json!({"path": "link.txt"}), "invalid_args: "),
        (json!({"path": "../beside.txt"}), "invalid_args: "),
        (json!({"path": "sub"}), "invalid_args: "),
        (json!({"path": "missing.txt"}), "not_found: "),
        (
            json!({"content": "hello", "name": "a.txt"}),
            "invalid_args: ",
        ),
        (
            json!({"path": "sub/shot.png", "kind": "audio"}),
            "invalid_args: ",
        ),
    ];
    for (n, (args, want)) in cases.into_iter().enumerate() {
        rt.call_tool(10 + n as u64, "hqc_attach", args.clone())
            .await;
        let r = rt.recv().await;
        let (is_err, text) = result_text(&r);
        assert!(is_err && text.starts_with(want), "{args}: {text}");
        assert!(!text.contains("secret\n"), "{text}");
    }
    assert_eq!(hub.artifacts.uploads().len(), 2);
    assert!(c
        .maybe("mcp.call", Duration::from_millis(300))
        .await
        .is_none());
    drop(rt);
    w.stop().await;
}

#[tokio::test]
async fn a_call_cancelled_during_its_upload_is_not_sent() {
    let (env, hub, w, mut c, _sid) = setup().await;
    std::fs::write(env.workspace().join("a.txt"), b"a").unwrap();
    hub.artifacts
        .stall_uploads
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut rt = Runtime::spawn(&env, Some(TOKEN_A));
    rt.initialize().await;
    let uploaded = async |n: usize| {
        let deadline = std::time::Instant::now() + WAIT;
        while hub.artifacts.uploads().len() < n {
            assert!(std::time::Instant::now() < deadline, "no upload");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };

    // The runtime cancels while the file is on its way: no `mcp.call`, no answer (MCP).
    rt.call_tool(2, "hqc_attach", json!({"path": "a.txt"}))
        .await;
    uploaded(1).await;
    rt.send(
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                   "params": {"requestId": 2, "reason": "timeout"}}),
    )
    .await;
    let r = rt.request(3, "ping", json!({})).await;
    assert_eq!(r["result"], json!({}));

    // The MCP process exits during an upload: the same.
    rt.call_tool(4, "hqc_attach", json!({"path": "a.txt"}))
        .await;
    uploaded(2).await;
    drop(rt.stdin.take());
    tokio::time::timeout(WAIT, rt.child.wait())
        .await
        .unwrap()
        .unwrap();
    for ty in ["mcp.call", "mcp.cancel"] {
        assert!(
            c.maybe(ty, Duration::from_millis(800)).await.is_none(),
            "{ty}"
        );
    }
    w.stop().await;
}
