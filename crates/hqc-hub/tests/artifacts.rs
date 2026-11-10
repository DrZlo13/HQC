//! T16: artifacts (SPEC §3 Artifact, §5.3 `session.input`, §6, §12): upload, the size limit,
//! deduplication, signed links and their expiry, the owner check, the worker download and
//! the headers that keep artifact HTML from running on the hub's domain.

mod common;

use common::*;
use hqc_hub::artifacts;
use serde_json::json;
use uuid::Uuid;

const WS_PATH: &str = "/w/hqc";

async fn raw_get(hub: &TestHub, url: &str, host: Option<&str>) -> reqwest::Response {
    let url = if url.starts_with('/') {
        format!("{}{url}", hub.base)
    } else {
        url.to_string()
    };
    let mut r = hub.http.get(url);
    if let Some(h) = host {
        r = r.header("host", h);
    }
    r.send().await.unwrap()
}

fn header(r: &reqwest::Response, k: &str) -> String {
    r.headers()
        .get(k)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

async fn signed_url(hub: &TestHub, id: &str) -> String {
    let (s, v) = hub.get(&format!("/api/artifacts/{id}/url")).await;
    assert_eq!(s, 200, "{v}");
    v["url"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn upload_kinds_and_dedup() {
    let hub = TestHub::start().await;
    let png = b"\x89PNG\r\n\x1a\nfake".to_vec();
    let (s, a) = hub.upload("shot.png", "image/png", png.clone(), None).await;
    assert_eq!(s, 201, "{a}");
    assert_eq!(
        (a["kind"].as_str(), a["name"].as_str(), a["size"].as_u64()),
        (Some("image"), Some("shot.png"), Some(png.len() as u64))
    );
    let sha = a["sha256"].as_str().unwrap();
    assert_eq!(sha.len(), 64);
    let blob = hub.app.config.artifacts.dir.join(&sha[..2]).join(sha);
    assert_eq!(std::fs::read(&blob).unwrap(), png);

    // The same content and name again: the same artifact, 200.
    let (s, again) = hub.upload("shot.png", "image/png", png.clone(), None).await;
    assert_eq!((s, &again["id"]), (200, &a["id"]), "{again}");
    // Another name: another artifact over the same stored content.
    let (s, copy) = hub.upload("copy.png", "image/png", png.clone(), None).await;
    assert_eq!(s, 201);
    assert_ne!(copy["id"], a["id"]);
    assert_eq!(copy["sha256"], a["sha256"]);

    for (name, mime, kind) in [
        ("fix.patch", "application/octet-stream", "diff"),
        ("page.html", "text/html", "html"),
        ("notes.md", "", "text"),
        ("main.rs", "application/octet-stream", "text"),
        ("data.bin", "application/octet-stream", "file"),
    ] {
        let (s, v) = hub
            .upload(name, mime, format!("{name}\n").into_bytes(), None)
            .await;
        assert_eq!((s, v["kind"].as_str()), (201, Some(kind)), "{name}: {v}");
    }
    // An explicit kind wins; an unknown one is refused.
    let (s, v) = hub
        .upload("x.txt", "text/plain", b"+a\n-b\n".to_vec(), Some("diff"))
        .await;
    assert_eq!((s, v["kind"].as_str()), (201, Some("diff")), "{v}");
    let (s, _) = hub
        .upload("y.txt", "text/plain", b"y".to_vec(), Some("audio"))
        .await;
    assert_eq!(s, 400);
    // A path in the name is cut to its last component.
    let (_, v) = hub
        .upload("../../etc/passwd", "text/plain", b"p".to_vec(), None)
        .await;
    assert_eq!(v["name"], "passwd");

    // No `file` field.
    let form = reqwest::multipart::Form::new().text("kind", "text");
    let r = hub
        .http
        .post(format!("{}/api/artifacts", hub.base))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);

    // A broken body is answered with the parser's reason, not axum's generic "Error parsing
    // `multipart/form-data` request": one that ends before its first part (an iPhone's upload
    // failed so), then one cut off in the middle of the file.
    let r = hub
        .http
        .post(format!("{}/api/artifacts", hub.base))
        .header("content-type", "multipart/form-data; boundary=XYZ")
        .body("abc")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["message"], "multipart: incomplete multipart stream");
    let r = hub
        .http
        .post(format!("{}/api/artifacts", hub.base))
        .header("content-type", "multipart/form-data; boundary=XYZ")
        .body("--XYZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.heic\"\r\nContent-Type: image/heic\r\n\r\nabc")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(
        v["message"],
        "multipart: field \"file\" received with incomplete data"
    );

    let (s, m) = hub
        .get(&format!("/api/artifacts/{}", a["id"].as_str().unwrap()))
        .await;
    assert_eq!((s, &m), (200, &a));
    hub.finish().await;
}

#[tokio::test]
async fn size_limit() {
    let hub = TestHub::start_artifacts(|c| c.max_bytes = 1000).await;
    let (s, v) = hub
        .upload("ok.bin", "application/octet-stream", vec![1; 1000], None)
        .await;
    assert_eq!(s, 201, "{v}");
    let (s, v) = hub
        .upload("big.bin", "application/octet-stream", vec![1; 1001], None)
        .await;
    assert_eq!((s, v["error"].as_str()), (413, Some("too_large")), "{v}");
    // Far over the limit: cut by the body limit, the same answer.
    let (s, _) = hub
        .upload(
            "huge.bin",
            "application/octet-stream",
            vec![1; 200_000],
            None,
        )
        .await;
    assert_eq!(s, 413);
    // Nothing of the refused uploads is left behind.
    let tmp = hub.app.config.artifacts.dir.join("tmp");
    assert_eq!(std::fs::read_dir(tmp).unwrap().count(), 0);
    hub.finish().await;
}

#[tokio::test]
async fn signed_links() {
    let hub = TestHub::start().await;
    let (_, a) = hub
        .upload("notes.txt", "text/plain", b"hello".to_vec(), None)
        .await;
    let id = a["id"].as_str().unwrap();
    let (s, v) = hub.get(&format!("/api/artifacts/{id}/url")).await;
    assert_eq!(s, 200, "{v}");
    let url = v["url"].as_str().unwrap();
    assert!(url.starts_with(&format!("/artifacts/{id}?exp=")), "{url}");
    let exp: chrono::DateTime<chrono::Utc> = v["expires_at"].as_str().unwrap().parse().unwrap();
    let ttl = exp - chrono::Utc::now();
    assert!(ttl.num_seconds() > 590 && ttl.num_seconds() <= 600, "{ttl}");

    let r = raw_get(&hub, url, None).await;
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(header(&r, "content-type"), "text/plain; charset=utf-8");
    assert!(header(&r, "content-disposition").starts_with("inline;"));
    assert!(header(&r, "content-security-policy").starts_with("sandbox;"));
    assert_eq!(header(&r, "x-content-type-options"), "nosniff");
    assert_eq!(header(&r, "cache-control"), "private, max-age=600");
    assert_eq!(r.text().await.unwrap(), "hello");
    let r = raw_get(&hub, &format!("{url}&dl=1"), None).await;
    assert!(header(&r, "content-disposition").starts_with("attachment;"));

    // Tampered, unsigned, expired.
    let bad = format!("{}A", &url[..url.len() - 1]);
    let bad = if bad == url {
        format!("{}B", &url[..url.len() - 1])
    } else {
        bad
    };
    assert_eq!(raw_get(&hub, &bad, None).await.status().as_u16(), 403);
    assert_eq!(
        raw_get(&hub, &format!("/artifacts/{id}"), None)
            .await
            .status()
            .as_u16(),
        403
    );
    let other = Uuid::now_v7();
    let swapped = url.replace(id, &other.to_string());
    assert_eq!(raw_get(&hub, &swapped, None).await.status().as_u16(), 403);
    let past = chrono::Utc::now().timestamp() - 1;
    let expired = artifacts::signed_url(&hub.app.config.artifacts, id.parse().unwrap(), past);
    let r = raw_get(&hub, &expired, None).await;
    assert_eq!(r.status().as_u16(), 410);
    assert!(header(&r, "content-security-policy").starts_with("sandbox;"));
    hub.finish().await;
}

/// Another user's artifact: no metadata, no link, not usable in an input.
#[tokio::test]
async fn owner_only() {
    let hub = TestHub::start().await;
    let other = hqc_hub::db::ensure_user(&hub.app.db, "someone-else@example.com")
        .await
        .unwrap();
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO artifacts (id, owner_id, kind, mime, size, sha256, name, storage_key)
         VALUES ($1, $2, 'text', 'text/plain', 1, $3, 'x.txt', $3)",
    )
    .bind(id)
    .bind(other.id)
    .bind("0".repeat(64))
    .execute(&hub.app.db)
    .await
    .unwrap();
    for path in [
        format!("/api/artifacts/{id}/url"),
        format!("/api/artifacts/{id}"),
    ] {
        let (s, v) = hub.get(&path).await;
        assert_eq!((s, v["error"].as_str()), (404, Some("not_found")), "{path}");
    }
    let (_, token) = hub.enroll("home-mac").await;
    let (_w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("other").await;
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "look", "artifact_ids": [id]}),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (400, Some("unknown_artifact")),
        "{v}"
    );
    hub.finish().await;
}

/// HTML is a download on the hub's domain and runs (sandboxed) only on the artifact domain;
/// the artifact domain serves nothing but artifacts.
#[tokio::test]
async fn html_and_the_artifact_domain() {
    let hub = TestHub::start_artifacts(|c| {
        c.public_base = Some("https://artifacts.example.com".into());
    })
    .await;
    let (_, a) = hub
        .upload(
            "report.html",
            "text/html",
            b"<script>alert(1)</script>".to_vec(),
            None,
        )
        .await;
    assert_eq!(a["kind"], "html");
    let url = signed_url(&hub, a["id"].as_str().unwrap()).await;
    let path = url
        .strip_prefix("https://artifacts.example.com")
        .unwrap_or_else(|| panic!("{url}"));

    // On the hub's own host: attachment, sandbox without scripts, no framing.
    let r = raw_get(&hub, path, None).await;
    assert_eq!(r.status().as_u16(), 200);
    assert!(header(&r, "content-disposition").starts_with("attachment;"));
    assert_eq!(header(&r, "content-security-policy"), artifacts::CSP);
    assert_eq!(header(&r, "x-frame-options"), "DENY");

    // On the artifact domain: inline in a sandbox that may run scripts but has no origin.
    let r = raw_get(&hub, path, Some("artifacts.example.com")).await;
    assert_eq!(r.status().as_u16(), 200);
    assert!(header(&r, "content-disposition").starts_with("inline;"));
    assert_eq!(header(&r, "content-security-policy"), artifacts::CSP_HTML);
    assert_eq!(header(&r, "x-frame-options"), "");
    assert_eq!(header(&r, "access-control-allow-origin"), "*");

    // The artifact domain is not the hub.
    for p in ["/api/me", "/", "/worker/enroll"] {
        let r = raw_get(&hub, p, Some("artifacts.example.com")).await;
        assert_eq!(r.status().as_u16(), 404, "{p}");
    }
    // The hub's own CSP lets the client show and frame artifacts from that domain.
    let r = raw_get(&hub, "/api/me", None).await;
    let csp = header(&r, "content-security-policy");
    assert!(
        csp.contains("img-src 'self' data: https://artifacts.example.com"),
        "{csp}"
    );
    assert!(
        csp.contains("frame-src https://artifacts.example.com"),
        "{csp}"
    );
    hub.finish().await;
}

/// `user.message` with `artifact_ids` goes to the worker in `session.input`; that worker, and
/// no other, downloads the content with its token.
#[tokio::test]
async fn worker_downloads_input_artifacts() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("home-mac").await;
    let (_, other_token) = hub.enroll("other").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("files").await;
    ack_start(&mut w, &sid).await;

    let (_, a) = hub
        .upload("отчёт 1.txt", "text/plain", b"report".to_vec(), None)
        .await;
    let aid = a["id"].as_str().unwrap();
    let get = |tok: &str| {
        hub.http
            .get(format!("{}/worker/artifacts/{aid}", hub.base))
            .bearer_auth(tok.to_string())
            .send()
    };
    // Not given to any session yet.
    assert_eq!(get(&token).await.unwrap().status().as_u16(), 404);

    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/input"),
            json!({"text": "", "artifact_ids": [aid], "client_op_id": "op-a"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(v["event"]["payload"]["artifact_ids"], json!([aid]));
    let si = w.expect("session.input").await;
    assert_eq!(si["payload"]["artifact_ids"], json!([aid]));
    assert_eq!(si["payload"]["text"], "");

    let r = get(&token).await.unwrap();
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(
        header(&r, "x-hqc-artifact-name"),
        artifacts::pct("отчёт 1.txt")
    );
    assert_eq!(header(&r, "x-hqc-artifact-kind"), "text");
    assert!(header(&r, "content-disposition").starts_with("attachment;"));
    assert!(header(&r, "content-security-policy").starts_with("sandbox;"));
    assert_eq!(r.bytes().await.unwrap().as_ref(), b"report");

    // Another worker of the same user, a bad token, no token.
    assert_eq!(get(&other_token).await.unwrap().status().as_u16(), 404);
    let bad = format!("hqcw_{}", "A".repeat(43));
    assert_eq!(get(&bad).await.unwrap().status().as_u16(), 401);
    let r = hub
        .http
        .get(format!("{}/worker/artifacts/{aid}", hub.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);

    // The link from the session: the artifact remembers it.
    let first: Option<Uuid> = sqlx::query_scalar("SELECT session_id FROM artifacts WHERE id = $1")
        .bind(aid.parse::<Uuid>().unwrap())
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(first.map(|u| u.to_string()), Some(sid.clone()));
    hub.finish().await;
}
