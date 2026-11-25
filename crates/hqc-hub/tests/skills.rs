//! SPEC 0.245 (iteration 8, T68): skills the owner keeps on the hub. They are uploaded as a
//! folder or a `.zip`, go to the sessions of their projects as names and hashes in
//! `session.start.skills`, and a worker of their owner fetches them by hash.

mod common;

use std::io::Write;

use common::*;
use serde_json::{json, Value};
use sha2::Digest;

const WS_PATH: &str = "/Users/home-mac/work/hqc";
const SKILL: &str = "---\ndescription: Bench it.\nallowed-tools: Read\n---\nRun scripts/run.sh.\n";

/// `POST /api/skills` with `parts` (field, file name, content) and a `name`.
async fn upload(
    hub: &TestHub,
    parts: &[(&str, &str, Vec<u8>)],
    name: Option<&str>,
) -> (u16, Value) {
    let mut form = reqwest::multipart::Form::new();
    for (field, file, data) in parts {
        form = form.part(
            field.to_string(),
            reqwest::multipart::Part::bytes(data.clone()).file_name(file.to_string()),
        );
    }
    if let Some(n) = name {
        form = form.text("name", n.to_string());
    }
    let r = hub
        .http
        .post(format!("{}/api/skills", hub.base))
        .multipart(form)
        .send()
        .await
        .unwrap();
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

async fn upload_folder(hub: &TestHub, skill_md: &str, name: &str) -> (u16, Value) {
    let parts = [
        ("file", "SKILL.md", skill_md.as_bytes().to_vec()),
        ("file", "scripts/run.sh", b"echo hi\n".to_vec()),
        ("file", ".DS_Store", b"junk".to_vec()),
    ];
    upload(hub, &parts, Some(name)).await
}

fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
    for (p, c) in files {
        z.start_file(*p, o).unwrap();
        z.write_all(c.as_bytes()).unwrap();
    }
    z.finish().unwrap().into_inner()
}

async fn fetch(hub: &TestHub, token: &str, sha: &str) -> (u16, Vec<u8>) {
    let r = hub
        .http
        .get(format!("{}/worker/skills/{sha}", hub.base))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    (r.status().as_u16(), r.bytes().await.unwrap().to_vec())
}

#[tokio::test]
async fn a_skill_goes_to_its_projects_and_its_owners_workers() {
    let hub = TestHub::start().await;
    let (s, a) = upload_folder(&hub, SKILL, "bench").await;
    assert_eq!(s, 201, "{a}");
    assert_eq!(a["name"], "bench");
    assert_eq!(a["files"], 2, "{a}");
    assert_eq!(a["description"], "Bench it.");
    assert_eq!(a["allowed_tools"], json!(["Read"]));
    assert_eq!(a["all_projects"], true);
    let sha = a["sha256"].as_str().unwrap().to_string();

    // The same files as a zip under a top folder: the same skill, the same hash, replaced.
    let z = zip_of(&[
        ("bench/SKILL.md", SKILL),
        ("bench/scripts/run.sh", "echo hi\n"),
    ]);
    let (s, b) = upload(&hub, &[("zip", "bench.zip", z)], Some("bench")).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        (b["id"].clone(), b["sha256"].clone()),
        (a["id"].clone(), a["sha256"].clone())
    );
    let (s, f) = hub
        .get(&format!("/api/skills/{}/files", a["id"].as_str().unwrap()))
        .await;
    assert_eq!(s, 200, "{f}");
    assert_eq!(f["files"][1]["path"], "scripts/run.sh");

    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("with skills").await;
    let start = ack_start(&mut w, &sid).await;
    assert_eq!(
        start["payload"]["skills"],
        json!([{"name": "bench", "sha256": sha}]),
        "{start}"
    );

    // The worker fetches it by hash; it is the archive whose hash it is.
    let (s, body) = fetch(&hub, &token, &sha).await;
    assert_eq!(s, 200);
    let got: String = sha2::Sha256::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(got, sha);
    assert_eq!(fetch(&hub, "hqcw_not_a_token", &sha).await.0, 401);
    assert_eq!(fetch(&hub, &token, &"0".repeat(64)).await.0, 404);
    assert_eq!(fetch(&hub, &token, "..%2F..").await.0, 404);

    // Another owner's skill is not served to this worker.
    let other = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, email, display_name) VALUES ($1, 'o@example.com', 'O')")
        .bind(other)
        .execute(&hub.app.db)
        .await
        .unwrap();
    let foreign = "f".repeat(64);
    sqlx::query(
        "INSERT INTO skills (id, owner_id, name, archive, sha256, size, files)
         VALUES ($1, $2, 'theirs', '\\x00', $3, 1, 1)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(other)
    .bind(&foreign)
    .execute(&hub.app.db)
    .await
    .unwrap();
    assert_eq!(fetch(&hub, &token, &foreign).await.0, 404);

    // Given to another project only: this session's next start has none.
    let (s, p) = hub.post("/api/projects", json!({"name": "Other"})).await;
    assert_eq!(s, 201, "{p}");
    let (s, v) = hub
        .patch(
            &format!("/api/skills/{}", a["id"].as_str().unwrap()),
            json!({"all_projects": false, "project_ids": [p["id"]]}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["project_ids"], json!([p["id"]]));
    let sid2 = hub.create_session("without").await;
    let start = ack_start(&mut w, &sid2).await;
    assert!(start["payload"].get("skills").is_none(), "{start}");

    // Deleted: gone from the list and from the worker.
    assert_eq!(
        hub.delete(&format!("/api/skills/{}", a["id"].as_str().unwrap()))
            .await,
        204
    );
    let (_, list) = hub.get("/api/skills").await;
    assert_eq!(list["skills"], json!([]));
    assert_eq!(fetch(&hub, &token, &sha).await.0, 404);
    hub.finish().await;
}

#[tokio::test]
async fn what_the_owner_uploads_is_checked() {
    let hub = TestHub::start().await;
    for (parts, code, why) in [
        (
            vec![(
                "file",
                "SKILL.md",
                b"---\nhooks:\n  Stop: []\n---\n".to_vec(),
            )],
            422,
            "hooks",
        ),
        (
            vec![("file", "README.md", b"".to_vec())],
            400,
            "no SKILL.md",
        ),
        (
            vec![
                ("file", "SKILL.md", b"".to_vec()),
                ("file", "../x", b"".to_vec()),
            ],
            400,
            "a path out",
        ),
        (
            vec![("file", "SKILL.md", b"---\nname: Bad Name\n---\n".to_vec())],
            400,
            "a bad name",
        ),
        (
            vec![("zip", "x.zip", b"not a zip".to_vec())],
            400,
            "not a zip",
        ),
        (
            vec![
                ("file", "SKILL.md", b"".to_vec()),
                ("file", "big", vec![b'x'; 5 << 20]),
            ],
            413,
            "too large",
        ),
    ] {
        let (s, v) = upload(&hub, &parts, Some("x")).await;
        assert_eq!(s, code, "{why}: {v}");
    }
    let (s, v) = upload(
        &hub,
        &[("file", "SKILL.md", b"---\nhooks: {}\n---\n".to_vec())],
        Some("x"),
    )
    .await;
    assert_eq!((s, v["error"].as_str()), (422, Some("skill_hooks")), "{v}");
    let (_, list) = hub.get("/api/skills").await;
    assert_eq!(list["skills"], json!([]));
    hub.finish().await;
}
