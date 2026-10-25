//! The stdio server against a scripted runtime (an in-memory pipe) and a scripted daemon (a
//! socket in a temp dir). The real daemon is in `tests/mcp.rs`.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};
use tokio::net::UnixListener;

use super::*;

const WAIT: Duration = Duration::from_secs(5);

struct Pipe {
    to_server: DuplexStream,
    from_server: Lines<BufReader<DuplexStream>>,
    server: tokio::task::JoinHandle<anyhow::Result<()>>,
}

fn serve(token: Option<&str>, socket: PathBuf) -> Pipe {
    let (to_server, server_in) = tokio::io::duplex(1 << 20);
    let (server_out, from_server) = tokio::io::duplex(1 << 20);
    let server =
        tokio::spawn(McpServer::new(token.map(String::from), socket).serve(server_in, server_out));
    Pipe {
        to_server,
        from_server: BufReader::new(from_server).lines(),
        server,
    }
}

impl Pipe {
    async fn send_raw(&mut self, line: &str) {
        self.to_server
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
    }

    async fn recv(&mut self) -> Value {
        let l = tokio::time::timeout(WAIT, self.from_server.next_line())
            .await
            .expect("an answer")
            .unwrap()
            .expect("open");
        serde_json::from_str(&l).unwrap()
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send_raw(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
        )
        .await;
        self.recv().await
    }
}

#[tokio::test]
async fn initialize_and_malformed_input() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = serve(Some("t"), dir.path().join("none.sock"));
    // A version the server does not know gets its newest one.
    let r = p
        .request(1, "initialize", json!({"protocolVersion": "1999-01-01"}))
        .await;
    assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    assert_eq!(r["result"]["serverInfo"]["name"], "hqc");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    for v in PROTOCOL_VERSIONS {
        let r = p
            .request(2, "initialize", json!({"protocolVersion": v}))
            .await;
        assert_eq!(r["result"]["protocolVersion"], *v);
    }

    p.send_raw("{not json").await;
    assert_eq!(p.recv().await["error"]["code"], PARSE_ERROR);
    p.send_raw(&"x".repeat(MAX_LINE + 10)).await;
    assert_eq!(p.recv().await["error"]["code"], INVALID_REQUEST);
    // Still serving after both.
    let r = p.request(3, "ping", json!({})).await;
    assert_eq!(r["id"], 3);

    // Over the size cap: refused before the daemon is asked.
    let big = "y".repeat(MAX_ARGS);
    let r = p
        .request(
            4,
            "tools/call",
            json!({"name": "hqc_list_sessions", "arguments": {"project": big}}),
        )
        .await;
    assert_eq!(r["result"]["isError"], true);
    assert!(r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with("invalid_args:"));
    // No daemon at that path.
    let r = p
        .request(5, "tools/call", json!({"name": "hqc_list_sessions"}))
        .await;
    assert!(r["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with("unavailable: the hqc worker daemon is not reachable"));

    drop(p.to_server);
    tokio::time::timeout(WAIT, p.server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// What a scripted daemon reads, line by line.
async fn read_msg(r: &mut Lines<BufReader<tokio::net::unix::OwnedReadHalf>>) -> Option<ToDaemon> {
    let l = tokio::time::timeout(WAIT, r.next_line())
        .await
        .ok()?
        .ok()??;
    Some(serde_json::from_str(&l).unwrap())
}

#[tokio::test]
async fn the_token_goes_only_to_the_daemon_and_a_lost_daemon_is_reconnected() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("d.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let mut p = serve(Some("hqcs_secret"), sock);
    p.request(
        1,
        "initialize",
        json!({"protocolVersion": "2025-06-18", "clientInfo": {"name": "claude-code"}}),
    )
    .await;

    for round in 0..2 {
        p.send_raw(
            &json!({"jsonrpc": "2.0", "id": 10 + round, "method": "tools/call",
                    "params": {"name": "hqc_list_sessions", "arguments": {"project": "p"}}})
            .to_string(),
        )
        .await;
        let (s, _) = tokio::time::timeout(WAIT, listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (rd, mut wr) = s.into_split();
        let mut rd = BufReader::new(rd).lines();
        match read_msg(&mut rd).await.unwrap() {
            ToDaemon::Hello {
                token,
                protocol_version,
                client,
            } => {
                assert_eq!(token, "hqcs_secret");
                assert_eq!(protocol_version.as_deref(), Some("2025-06-18"));
                assert_eq!(client.unwrap()["name"], "claude-code");
            }
            other => panic!("{other:?}"),
        }
        wr.write_all(b"{\"type\":\"ready\"}\n").await.unwrap();
        let Some(ToDaemon::Call { id, tool, args }) = read_msg(&mut rd).await else {
            panic!("a call");
        };
        assert_eq!(
            (tool.as_str(), &args),
            ("hqc_list_sessions", &json!({"project": "p"}))
        );
        if round == 0 {
            // The daemon goes away with the call open: it fails, the next call reconnects.
            drop((rd, wr));
            let r = p.recv().await;
            assert_eq!(r["id"], 10);
            let text = r["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.starts_with("unavailable:"), "{text}");
            continue;
        }
        let answer = FromDaemon::Result {
            id,
            result: Some(json!({"sessions": []})),
            error: None,
        };
        wr.write_all(format!("{}\n", serde_json::to_string(&answer).unwrap()).as_bytes())
            .await
            .unwrap();
        let r = p.recv().await;
        assert_eq!(r["id"], 11);
        assert_eq!(r["result"]["isError"], false);
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("hqcs_secret"));
    }
}

#[tokio::test]
async fn a_waiting_call_reports_progress_until_it_is_answered() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("d.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let (mut to_server, server_in) = tokio::io::duplex(1 << 20);
    let (server_out, from_server) = tokio::io::duplex(1 << 20);
    tokio::spawn(
        McpServer::new(Some("hqcs_t".into()), sock)
            .with_progress_every(Duration::from_millis(30))
            .serve(server_in, server_out),
    );
    let mut from_server = BufReader::new(from_server).lines();
    let call = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {"name": "hqc_ask", "arguments": {"text": "hi", "session": "p/s"},
                   "_meta": {"progressToken": "tok-7"}}});
    to_server
        .write_all(format!("{call}\n").as_bytes())
        .await
        .unwrap();
    let (s, _) = tokio::time::timeout(WAIT, listener.accept())
        .await
        .unwrap()
        .unwrap();
    let (rd, mut wr) = s.into_split();
    let mut rd = BufReader::new(rd).lines();
    assert!(matches!(
        read_msg(&mut rd).await,
        Some(ToDaemon::Hello { .. })
    ));
    wr.write_all(b"{\"type\":\"ready\"}\n").await.unwrap();
    let Some(ToDaemon::Call { id, tool, .. }) = read_msg(&mut rd).await else {
        panic!("a call");
    };
    assert_eq!(tool, "hqc_ask");

    for n in 1..=3 {
        let p = next(&mut from_server).await;
        assert_eq!(p["method"], "notifications/progress", "{p}");
        assert_eq!(p["params"]["progressToken"], "tok-7");
        assert_eq!(p["params"]["progress"], n, "progress only grows");
    }
    let answer = FromDaemon::Result {
        id,
        result: Some(json!({"from": "p/s", "reply": "hello"})),
        error: None,
    };
    wr.write_all(format!("{}\n", serde_json::to_string(&answer).unwrap()).as_bytes())
        .await
        .unwrap();
    let r = loop {
        let m = next(&mut from_server).await;
        if m.get("method").is_none() {
            break m;
        }
    };
    assert_eq!(r["id"], 7);
    assert_eq!(r["result"]["isError"], false);
    // Nothing after the answer.
    let quiet = tokio::time::timeout(Duration::from_millis(150), from_server.next_line()).await;
    assert!(quiet.is_err(), "{quiet:?}");
}

async fn next(r: &mut Lines<BufReader<DuplexStream>>) -> Value {
    let l = tokio::time::timeout(WAIT, r.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(&l).unwrap()
}

#[test]
fn tool_results() {
    let ok = tool_result(Ok(json!({"a": 1})));
    assert_eq!(ok["isError"], false);
    assert_eq!(
        serde_json::from_str::<Value>(ok["content"][0]["text"].as_str().unwrap()).unwrap(),
        json!({"a": 1})
    );
    let e = tool_result(Err(McpError::new("not_found", "no project x")));
    assert_eq!(e["isError"], true);
    assert_eq!(e["content"][0]["text"], "not_found: no project x");
}
