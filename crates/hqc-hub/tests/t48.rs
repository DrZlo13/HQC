//! T48 (SPEC §3 Workspace, §5.3, §6, §14): hqc's allow rules of a workspace. Owner only,
//! checked by the syntax of `hqc_proto::rules`, at most 200 per folder; the worker gets them
//! with `session.start`, as `workspace.rules` on every change and after `hello`; the rules
//! screen reads the worker's `permissions.inspect` through the hub.

mod common;

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const ROOT: &str = "/w/t48";

async fn workspace(hub: &TestHub) -> Value {
    let (_, list) = hub.get("/api/workspaces").await;
    list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["path"] == ROOT)
        .unwrap()
        .clone()
}

fn texts(list: &Value) -> Vec<String> {
    list["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rule"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn rules_crud_reach_the_worker() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("w").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, ROOT).await;
    w.see_rules = true;
    let ws = workspace(&hub).await;
    assert_eq!(ws["hqc_rules"], 0);
    let wid = ws["id"].as_str().unwrap().to_string();
    let rules = format!("/api/workspaces/{wid}/rules");

    // Invalid rules save nothing.
    for bad in ["Bash", "Bash(a && b)", "Read(./src/**)", ""] {
        let (s, v) = hub
            .post(&rules, json!({"rules": ["Bash(python3 *)", bad]}))
            .await;
        assert_eq!(
            (s, v["error"].as_str()),
            (422, Some("invalid_rule")),
            "{bad:?}: {v}"
        );
    }
    let (s, v) = hub.post(&rules, json!({"rules": []})).await;
    assert_eq!(s, 400, "{v}");
    let (s, v) = hub.get(&rules).await;
    assert_eq!((s, texts(&v).len()), (200, 0));

    let (s, v) = hub
        .post(
            &rules,
            json!({"rules": ["Bash(python3 *)", "Bash(git fetch:*)", "Bash(python3 *)"]}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        texts(&v),
        ["Bash(git fetch:*)", "Bash(python3 *)"],
        "newest first"
    );
    let f = w.expect("workspace.rules").await;
    assert_eq!(
        f["payload"],
        json!({"workspace_path": ROOT, "allow_rules": ["Bash(python3 *)", "Bash(git fetch:*)"]})
    );
    w.reply(&f["id"], "ack", json!({})).await;

    // A rule the folder has is kept as it is.
    let (s, v) = hub
        .post(&rules, json!({"rules": ["Bash(python3 *)", "Edit"]}))
        .await;
    assert_eq!((s, texts(&v).len()), (200, 3), "{v}");
    let f = w.expect("workspace.rules").await;
    assert_eq!(f["payload"]["allow_rules"].as_array().unwrap().len(), 3);
    assert_eq!(workspace(&hub).await["hqc_rules"], 3);

    let gone = v["rules"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(v["rules"][0]["rule"], "Edit");
    assert_eq!(hub.delete(&format!("{rules}/{gone}")).await, 204);
    let f = w.expect("workspace.rules").await;
    assert_eq!(
        f["payload"]["allow_rules"],
        json!(["Bash(python3 *)", "Bash(git fetch:*)"])
    );
    assert_eq!(hub.delete(&format!("{rules}/{gone}")).await, 404);
    assert_eq!(
        hub.delete(&format!("{rules}/{}", Uuid::now_v7())).await,
        404
    );

    // At most 200 per folder: nothing of a list that would go over is saved.
    let many: Vec<String> = (0..199).map(|i| format!("Bash(tool{i} *)")).collect();
    let (s, v) = hub.post(&rules, json!({"rules": many})).await;
    assert_eq!(
        (s, v["error"].as_str()),
        (422, Some("too_many_rules")),
        "{v}"
    );
    let (_, v) = hub.get(&rules).await;
    assert_eq!(texts(&v).len(), 2);

    // A session in the folder starts with the rules.
    let sid = hub.create_session("rules").await;
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "hi", "artifact_ids": [], "client_op_id": "c1"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(
        start["payload"]["allow_rules"],
        json!(["Bash(python3 *)", "Bash(git fetch:*)"])
    );

    // After a reconnect every workspace of the worker gets its list again, an emptied one
    // too.
    for r in hub.get(&rules).await.1["rules"].as_array().unwrap() {
        assert_eq!(
            hub.delete(&format!("{rules}/{}", r["id"].as_str().unwrap()))
                .await,
            204
        );
    }
    w.close().await;
    let (mut w, _) = FakeWorker::online(&hub, &token, ROOT).await;
    w.see_rules = true;
    let f = loop {
        let f = w.recv().await;
        if f["type"] == "workspace.rules" {
            break f;
        }
    };
    assert_eq!(
        f["payload"],
        json!({"workspace_path": ROOT, "allow_rules": []})
    );
    hub.finish().await;
}

#[tokio::test]
async fn rules_are_the_owners_only() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("w").await;
    let (_w, _) = FakeWorker::online(&hub, &token, ROOT).await;
    let wid = workspace(&hub).await["id"].as_str().unwrap().to_string();
    let rules = format!("/api/workspaces/{wid}/rules");
    let (s, v) = hub.post(&rules, json!({"rules": ["Edit"]})).await;
    assert_eq!(s, 200, "{v}");
    let rule_id = v["rules"][0]["id"].as_str().unwrap().to_string();

    let other = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, email, display_name) VALUES ($1, $2, 'other')")
        .bind(other)
        .bind(format!("other-{other}@example.com"))
        .execute(&hub.app.db)
        .await
        .unwrap();
    sqlx::query("UPDATE workspaces SET owner_id = $2 WHERE id = $1::uuid")
        .bind(&wid)
        .bind(other)
        .execute(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(hub.get(&rules).await.0, 404);
    assert_eq!(hub.post(&rules, json!({"rules": ["Read"]})).await.0, 404);
    assert_eq!(hub.delete(&format!("{rules}/{rule_id}")).await, 404);
    assert_eq!(
        hub.get(&format!("/api/workspaces/{wid}/permissions"))
            .await
            .0,
        404
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM workspace_rules")
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(n, 1, "nothing changed");
    hub.finish().await;
}

#[tokio::test]
async fn permissions_come_from_the_worker() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("w").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, ROOT).await;
    let wid = workspace(&hub).await["id"].as_str().unwrap().to_string();
    let url = format!("/api/workspaces/{wid}/permissions?command=git%20-C%20sub%20fetch");

    let answer = async {
        let f = w.expect("permissions.inspect").await;
        assert_eq!(f["payload"]["path"], ROOT);
        assert_eq!(f["payload"]["command"], "git -C sub fetch");
        assert_eq!(f["payload"]["request_id"], f["id"]);
        let result = json!({"request_id": f["id"], "mode": "ask", "allow_auto": true,
            "sources": [{"source": "worker.toml", "allow": [], "deny": ["Bash(ssh:*)"], "ask": []}],
            "hqc": ["Bash(git fetch:*)"],
            "check": {"verdict": "allow", "rule": "Bash(git fetch:*)", "source": "hqc", "parts": 1, "uncovered": 0}});
        w.reply(&f["id"], "result", result).await;
        w
    };
    let ((s, v), mut w) = tokio::join!(hub.get(&url), answer);
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["check"]["verdict"], "allow");
    assert_eq!(v["sources"][0]["deny"], json!(["Bash(ssh:*)"]));
    assert!(v.get("request_id").is_none(), "{v}");

    // The worker refuses the path: the screen says so.
    let answer = async {
        let f = w.expect("permissions.inspect").await;
        w.reply(
            &f["id"],
            "error",
            json!({"code": "invalid_frame", "message": "path is not a workspace of this worker", "retryable": false}),
        )
        .await;
        w
    };
    let plain = format!("/api/workspaces/{wid}/permissions");
    let ((s, v), w) = tokio::join!(hub.get(&plain), answer);
    assert_eq!(s, 400, "{v}");

    w.close().await;
    // Offline: the screen shows hqc's rules only.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let (s, v) = hub.get(&format!("/api/workspaces/{wid}/permissions")).await;
        if s == 409 {
            assert_eq!(v["error"], "worker_offline");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "still online: {s} {v}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    hub.finish().await;
}
