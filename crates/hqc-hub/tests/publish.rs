//! `hqc_publish` (SPEC §7, §10.1, §12; 0.209): the archive the worker uploaded is published by
//! a public link once the session's owner allows it, served on the publication domain
//! (`HQC_PUBLISH_BASE`), and unpublished by its owner.

mod common;

use std::{io::Write, time::Duration};

use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const WS_PATH: &str = "/w/hqc";
const BASE: &str = "https://view.example.net";
const VIEW_HOST: &str = "view.example.net";

async fn hub() -> TestHub {
    TestHub::start_artifacts(|c| c.publish_base = Some(BASE.into())).await
}

/// A zip of `entries`, deflated, in this order.
fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (n, b) in entries {
        z.start_file(*n, o).unwrap();
        z.write_all(b).unwrap();
    }
    z.finish().unwrap().into_inner()
}

fn page() -> Vec<u8> {
    zip_of(&[
        ("css/a.css", b"p{color:red}"),
        ("img/x.png", b"\x89PNG fake"),
        ("index.html", b"<p>hi</p>"),
    ])
}

/// `PUT /worker/artifacts` of a `site` for session `sid`, as the worker's daemon sends it.
async fn upload_site(hub: &TestHub, worker: &str, sid: &str, body: &[u8]) -> Value {
    put(hub, worker, sid, "report", Some("site"), body).await
}

async fn put(
    hub: &TestHub,
    worker: &str,
    sid: &str,
    name: &str,
    kind: Option<&str>,
    body: &[u8],
) -> Value {
    let mut r = hub
        .http
        .put(format!("{}/worker/artifacts", hub.base))
        .bearer_auth(worker)
        .header("x-hqc-artifact-name", hqc_hub::artifacts::pct(name))
        .header("x-hqc-session-id", sid)
        .body(body.to_vec());
    if let Some(k) = kind {
        r = r.header("x-hqc-artifact-kind", k);
    }
    let r = r.send().await.unwrap();
    let s = r.status().as_u16();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    assert_eq!(s, 201, "{v}");
    v
}

/// A worker online with a session started: the worker's token, the worker, the session and
/// its token.
async fn agent(hub: &TestHub) -> (String, FakeWorker, String, String) {
    let (_, worker) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(hub, &worker, WS_PATH).await;
    let (sid, token) = session(hub, &mut w, "Report").await;
    (worker, w, sid, token)
}

async fn session(hub: &TestHub, w: &mut FakeWorker, title: &str) -> (String, String) {
    let sid = hub.create_session(title).await;
    let f = ack_start(w, &sid).await;
    (sid, f["payload"]["session_token"].as_str().unwrap().into())
}

fn rid() -> String {
    Uuid::now_v7().to_string()
}

/// `mcp.call` of `tool` with `request_id` `rid`: the hub's answer to the frame, `ack` (the call
/// waits for the owner) or its `mcp.result`.
async fn call(w: &mut FakeWorker, token: &str, tool: &str, rid: &str, args: Value) -> Value {
    let id = w
        .send(
            "mcp.call",
            json!({"request_id": rid, "session_token": token, "tool": tool, "args": args}),
        )
        .await;
    loop {
        let f = w.recv().await;
        if f["id"] == id.as_str() && matches!(f["type"].as_str(), Some("ack" | "mcp.result")) {
            return f;
        }
        assert_eq!(f["type"], "ack", "unexpected frame {f}");
    }
}

async fn publish(w: &mut FakeWorker, token: &str, rid: &str, args: Value) -> Value {
    call(w, token, "hqc_publish", rid, args).await
}

/// The error code and message of a call refused at once.
fn refused(f: &Value) -> (String, String) {
    assert_eq!(f["type"], "mcp.result", "{f}");
    let e = &f["payload"]["error"];
    (
        e["code"].as_str().unwrap_or_else(|| panic!("{f}")).into(),
        e["message"].as_str().unwrap().into(),
    )
}

/// The owner's answer `option` to request `rid`, then the call's `mcp.result`, acked.
async fn answer(hub: &TestHub, w: &mut FakeWorker, sid: &str, rid: &str, option: &str) -> Value {
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/{rid}"),
            json!({"option_id": option}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    // `once` closes the request with the owner's mark, any other answer without it.
    let (mark, owner): (Option<String>, String) = sqlx::query_as(
        "SELECT p.payload ->> 'allowed_by', s.owner_id::text FROM pending_requests p
         JOIN sessions s ON s.id = p.session_id WHERE p.kind = 'publish' AND p.request_id = $1",
    )
    .bind(rid)
    .fetch_one(&hub.app.db)
    .await
    .unwrap();
    assert_eq!(mark, (option == "once").then_some(owner));
    let f = w.expect("mcp.result").await;
    w.reply(&f["id"], "ack", json!({})).await;
    assert_eq!(f["payload"]["request_id"], rid, "{f}");
    f["payload"].clone()
}

fn of_type<'a>(events: &'a [Value], ty: &str) -> Vec<&'a Value> {
    events.iter().filter(|e| e["type"] == ty).collect()
}

/// `GET` on the publication domain.
async fn view(hub: &TestHub, path: &str, headers: &[(&str, &str)]) -> reqwest::Response {
    let mut r = hub
        .http
        .get(format!("{}{path}", hub.base))
        .header("host", VIEW_HOST);
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    r.send().await.unwrap()
}

fn header(r: &reqwest::Response, k: &str) -> String {
    r.headers()
        .get(k)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

/// `public_id` of a link of this hub.
fn public_id(url: &str) -> String {
    let rest = url.strip_prefix(&format!("{BASE}/")).unwrap();
    let id = rest.split('/').next().unwrap();
    assert!(
        id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "{url}"
    );
    id.to_string()
}

async fn row(hub: &TestHub, aid: &str) -> (Option<chrono::DateTime<chrono::Utc>>, Option<String>) {
    sqlx::query_as("SELECT published_at, public_id FROM artifacts WHERE id = $1")
        .bind(aid.parse::<Uuid>().unwrap())
        .fetch_one(&hub.app.db)
        .await
        .unwrap()
}

async fn open_publications(hub: &TestHub, sid: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM pending_requests
         WHERE session_id = $1 AND kind = 'publish' AND closed_at IS NULL",
    )
    .bind(sid.parse::<Uuid>().unwrap())
    .fetch_one(&hub.app.db)
    .await
    .unwrap()
}

/// A page published by the owner's `once`: the artifact's id and the link.
async fn published(
    hub: &TestHub,
    w: &mut FakeWorker,
    worker: &str,
    sid: &str,
    token: &str,
) -> (String, String) {
    let a = upload_site(hub, worker, sid, &page()).await;
    let aid = a["id"].as_str().unwrap().to_string();
    let rid = rid();
    let f = publish(
        w,
        token,
        &rid,
        json!({"artifact_id": aid, "path": "/w/hqc/report"}),
    )
    .await;
    assert_eq!(f["type"], "ack", "{f}");
    let r = answer(hub, w, sid, &rid, "once").await;
    let url = r["result"]["url"].as_str().unwrap_or_else(|| panic!("{r}"));
    (aid, url.to_string())
}

/// The call waits for the owner: the feed has the hub's card (the path, the files and their
/// size, that the link is public; Publish / Deny), Requests lists it as `publish`; a repeat
/// waits without a second card, another call of the session is refused while it waits, and a
/// text answer is refused. Allowed: the artifact is published once, the feed has its
/// `attachment` of kind `site`, the call gets `{url, event_id}` (again on a repeat), and the
/// link serves the archive's files with the headers of SPEC §12 and nothing else.
#[tokio::test]
async fn the_owner_allows_and_the_page_is_served() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let a = upload_site(&hub, &worker, &sid, &page()).await;
    assert_eq!(
        (a["kind"].as_str(), a["mime"].as_str()),
        (Some("site"), Some("application/zip")),
        "{a}"
    );
    assert!(a.get("public_url").is_none(), "{a}");
    let aid = a["id"].as_str().unwrap();
    let rid = rid();
    let args = json!({"artifact_id": aid, "path": "/w/hqc/report", "caption": "The weekly report"});

    let f = publish(&mut w, &token, &rid, args.clone()).await;
    assert_eq!(f["type"], "ack", "{f}");
    let events = hub.events(&sid).await;
    let cards = of_type(&events, "approval.request");
    assert_eq!(cards.len(), 1, "{events:?}");
    assert_eq!(cards[0]["author"], json!({"kind": "system"}));
    let p = &cards[0]["payload"];
    assert_eq!(
        (&p["request_id"], &p["tool"]),
        (&json!(rid), &json!("hqc_publish"))
    );
    assert_eq!(
        p["description"],
        "The agent asks to publish /w/hqc/report at a public link: anyone who has the link can \
         open it, without signing in to hqc, until you unpublish it or delete this session.\n\
         3 files, 30 B:\n\
         css/a.css\nimg/x.png\nindex.html"
    );
    assert_eq!(
        p["options"],
        json!([{"id": "once", "label": "Publish", "canonical": "once"},
               {"id": "deny", "label": "Deny", "canonical": "deny"}])
    );
    let (s, list) = hub.get("/api/requests").await;
    assert_eq!(s, 200, "{list}");
    let r = &list["requests"].as_array().unwrap()[0];
    assert_eq!(
        (&r["kind"], &r["request_id"]),
        (&json!("publish"), &json!(rid))
    );
    assert_eq!(row(&hub, aid).await, (None, None));

    // A repeat (the `ack` lost, SPEC §5.2), another publication meanwhile, a text answer.
    let again = publish(&mut w, &token, &rid, args.clone()).await;
    assert_eq!(again["type"], "ack", "{again}");
    assert_eq!(
        of_type(&hub.events(&sid).await, "approval.request").len(),
        1
    );
    let b = upload_site(&hub, &worker, &sid, &page()).await;
    assert_ne!(
        b["id"], a["id"],
        "every publication is an artifact of its own"
    );
    let other = publish(
        &mut w,
        &token,
        &self::rid(),
        json!({"artifact_id": b["id"], "path": "/w/hqc/report"}),
    )
    .await;
    let (code, msg) = refused(&other);
    assert_eq!(code, "unavailable");
    assert!(msg.contains("already waits"), "{msg}");
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/{rid}"),
            json!({"option_id": "deny", "message": "no"}),
        )
        .await;
    assert_eq!(s, 400, "{v}");

    let r = answer(&hub, &mut w, &sid, &rid, "once").await;
    let result = r["result"].clone();
    let url = result["url"].as_str().unwrap_or_else(|| panic!("{r}"));
    let id = public_id(url);
    assert_eq!(url, format!("{BASE}/{id}/"));
    let events = hub.events(&sid).await;
    let att = of_type(&events, "attachment");
    assert_eq!(att.len(), 1, "{events:?}");
    assert_eq!(att[0]["event_id"], result["event_id"]);
    assert_eq!(att[0]["author"], json!({"kind": "agent", "id": sid}));
    assert_eq!(
        att[0]["payload"],
        json!({"artifact_id": aid, "kind": "site", "name": "report", "caption": "The weekly report"})
    );
    let (at, pid) = row(&hub, aid).await;
    assert!(at.is_some());
    assert_eq!(pid.as_deref(), Some(id.as_str()));
    let (s, meta) = hub.get(&format!("/api/artifacts/{aid}")).await;
    assert_eq!(s, 200, "{meta}");
    assert_eq!(meta["public_url"], url);
    assert_eq!(open_publications(&hub, &sid).await, 0);
    // A repeat now: the stored result, nothing new.
    let again = publish(&mut w, &token, &rid, args).await;
    assert_eq!(again["type"], "mcp.result", "{again}");
    assert_eq!(again["payload"]["result"], result);
    assert_eq!(of_type(&hub.events(&sid).await, "attachment").len(), 1);

    // The page, with every header.
    let r = view(&hub, &format!("/{id}/"), &[]).await;
    assert_eq!(r.status().as_u16(), 200);
    let etag = format!("\"{}\"", a["sha256"].as_str().unwrap());
    for (k, v) in [
        ("content-type", "text/html; charset=utf-8"),
        ("cache-control", "public, max-age=300"),
        ("etag", etag.as_str()),
        ("x-content-type-options", "nosniff"),
        ("content-security-policy", hqc_hub::publish::CSP_SITE),
        ("x-robots-tag", "noindex"),
        ("referrer-policy", "no-referrer"),
        // The page's own requests come from its opaque origin (SPEC §12).
        ("access-control-allow-origin", "*"),
    ] {
        assert_eq!(header(&r, k), v, "{k}");
    }
    assert!(header(&r, "set-cookie").is_empty());
    assert_eq!(r.text().await.unwrap(), "<p>hi</p>");
    for (path, ctype, body) in [
        ("index.html", "text/html; charset=utf-8", &b"<p>hi</p>"[..]),
        ("css/a.css", "text/css; charset=utf-8", b"p{color:red}"),
        ("img/x.png", "image/png", b"\x89PNG fake"),
        ("img/%78.png", "image/png", b"\x89PNG fake"),
    ] {
        let r = view(&hub, &format!("/{id}/{path}"), &[]).await;
        assert_eq!(r.status().as_u16(), 200, "{path}");
        assert_eq!(header(&r, "content-type"), ctype, "{path}");
        assert_eq!(r.bytes().await.unwrap().as_ref(), body, "{path}");
    }
    // A cache asks again.
    let r = view(
        &hub,
        &format!("/{id}/"),
        &[("if-none-match", etag.as_str())],
    )
    .await;
    assert_eq!(r.status().as_u16(), 304);
    assert_eq!(header(&r, "etag"), etag);
    assert_eq!(header(&r, "access-control-allow-origin"), "*");
    // Nothing else on that domain.
    let other_id = "0".repeat(32);
    for path in [
        format!("/{id}/missing.js"),
        format!("/{id}/css/"),
        format!("/{id}/..%2Findex.html"),
        format!("/{id}"),
        format!("/{other_id}/"),
        format!("/{}/", id.to_uppercase()),
        "/".into(),
        "/api/me".into(),
        format!("/api/artifacts/{aid}"),
        format!("/artifacts/{aid}"),
    ] {
        let r = view(&hub, &path, &[]).await;
        assert_eq!(r.status().as_u16(), 404, "{path}");
        assert_eq!(header(&r, "x-robots-tag"), "noindex", "{path}");
        assert_eq!(header(&r, "cache-control"), "no-store", "{path}");
    }
    let r = view(&hub, "/health", &[]).await;
    assert_eq!(r.status().as_u16(), 200);
    // The hub's own host does not serve it.
    let r = hub
        .http
        .get(format!("{}/{id}/", hub.base))
        .send()
        .await
        .unwrap();
    assert_ne!(r.text().await.unwrap(), "<p>hi</p>");
    hub.finish().await;
}

/// The archive of one file: its link names the file.
#[tokio::test]
async fn one_file_is_published_under_its_name() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let a = upload_site(
        &hub,
        &worker,
        &sid,
        &zip_of(&[("отчёт 1.pdf", b"%PDF-1.7")]),
    )
    .await;
    let aid = a["id"].as_str().unwrap();
    let rid = rid();
    let f = publish(
        &mut w,
        &token,
        &rid,
        json!({"artifact_id": aid, "path": "/w/hqc/отчёт 1.pdf"}),
    )
    .await;
    assert_eq!(f["type"], "ack", "{f}");
    let r = answer(&hub, &mut w, &sid, &rid, "once").await;
    let url = r["result"]["url"].as_str().unwrap();
    let id = public_id(url);
    assert_eq!(
        url,
        format!("{BASE}/{id}/%D0%BE%D1%82%D1%87%D1%91%D1%82%201.pdf")
    );
    let (_, meta) = hub.get(&format!("/api/artifacts/{aid}")).await;
    assert_eq!(meta["public_url"], url);
    let r = view(&hub, url.strip_prefix(BASE).unwrap(), &[]).await;
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(header(&r, "content-type"), "application/pdf");
    assert_eq!(r.bytes().await.unwrap().as_ref(), b"%PDF-1.7");
    assert_eq!(
        view(&hub, &format!("/{id}/"), &[]).await.status().as_u16(),
        404
    );
    hub.finish().await;
}

/// The owner denies: the call gets `unavailable`, nothing is published, no attachment.
#[tokio::test]
async fn the_owner_denies_a_publication() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let a = upload_site(&hub, &worker, &sid, &page()).await;
    let aid = a["id"].as_str().unwrap();
    let rid = rid();
    let f = publish(
        &mut w,
        &token,
        &rid,
        json!({"artifact_id": aid, "path": "/w/hqc/report"}),
    )
    .await;
    assert_eq!(f["type"], "ack", "{f}");
    let r = answer(&hub, &mut w, &sid, &rid, "deny").await;
    let e = &r["error"];
    assert_eq!(e["code"], "unavailable", "{r}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("the user did not allow the publication"),
        "{r}"
    );
    assert_eq!(row(&hub, aid).await, (None, None));
    assert!(of_type(&hub.events(&sid).await, "attachment").is_empty());
    let (_, meta) = hub.get(&format!("/api/artifacts/{aid}")).await;
    assert!(meta.get("public_url").is_none(), "{meta}");
    hub.finish().await;
}

/// Another user cannot answer the card: `403`, the request stays open, nothing is sent.
#[tokio::test]
async fn only_the_owner_answers_a_publication() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let a = upload_site(&hub, &worker, &sid, &page()).await;
    let rid = rid();
    let f = publish(
        &mut w,
        &token,
        &rid,
        json!({"artifact_id": a["id"], "path": "/w/hqc/report"}),
    )
    .await;
    assert_eq!(f["type"], "ack", "{f}");

    let other = hqc_hub::db::ensure_user(&hub.app.db, "other@example.com")
        .await
        .unwrap();
    let user = hqc_hub::auth::CurrentUser {
        id: other.id,
        email: other.email,
        display_name: other.display_name,
        timezone: other.timezone,
        personal_project_id: other.personal_project_id.unwrap(),
        theme: Default::default(),
        look: Default::default(),
        project_sort: Default::default(),
        new_session_project_id: None,
        quick_messages: Vec::new(),
    };
    let s = hqc_hub::db::session(&hub.app.db, sid.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let body = hqc_hub::requests::AnswerBody {
        option_id: Some("once".into()),
        answers: None,
        message: None,
        action: None,
    };
    let r = hqc_hub::requests::answer(&hub.app, &user, &s, &rid, body).await;
    assert!(
        matches!(r, Err(hqc_hub::error::ApiError::Forbidden(_))),
        "{r:?}"
    );
    w.expect_silence(Duration::from_millis(300)).await;
    assert_eq!(open_publications(&hub, &sid).await, 1);
    assert_eq!(row(&hub, a["id"].as_str().unwrap()).await, (None, None));
    hub.finish().await;
}

/// The bytes of `zip` with the size unpacked its central directory gives the first entry set
/// to `size`: the entry claims to unpack to that much.
fn claimed(mut zip: Vec<u8>, size: u32) -> Vec<u8> {
    let at = zip
        .windows(4)
        .position(|w| w == b"PK\x01\x02")
        .expect("a central directory");
    zip[at + 24..at + 28].copy_from_slice(&size.to_le_bytes());
    zip
}

/// What the hub refuses before any card: archives over the limits of SPEC §20, names it
/// cannot show or that leave the archive, a folder without `index.html`, something that is no
/// zip; an artifact that is not an unpublished `site` uploaded for this session; a path the
/// card cannot show. A `site` cannot be attached either.
#[tokio::test]
async fn the_hub_refuses_what_it_will_not_publish() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let (other_sid, _) = session(&hub, &mut w, "Other").await;
    let many: Vec<(String, Vec<u8>)> = (0..=hqc_proto::mcp::MAX_SITE_FILES)
        .map(|i| (format!("f{i}.txt"), Vec::new()))
        .collect();
    let many: Vec<(&str, &[u8])> = many
        .iter()
        .map(|(n, b)| (n.as_str(), b.as_slice()))
        .collect();
    for (body, why) in [
        (zip_of(&many), "more than 10000 entries"),
        (
            claimed(zip_of(&[("index.html", b"<p>")]), 0x7000_0000),
            "more than 524288000 bytes unpacked",
        ),
        (
            zip_of(&[("../x.html", b"x"), ("index.html", b"i")]),
            "component",
        ),
        (
            zip_of(&[("/etc/x", b"x"), ("index.html", b"i")]),
            "not a relative path",
        ),
        (
            zip_of(&[("a\u{202e}lmth.exe", b"x"), ("index.html", b"i")]),
            "bidirectional",
        ),
        (zip_of(&[("a.html", b"a"), ("b.html", b"b")]), "index.html"),
        (b"not a zip".to_vec(), "not a zip archive"),
    ] {
        let a = upload_site(&hub, &worker, &sid, &body).await;
        let f = publish(
            &mut w,
            &token,
            &rid(),
            json!({"artifact_id": a["id"], "path": "/w/hqc/report"}),
        )
        .await;
        let (code, msg) = refused(&f);
        assert_eq!(code, "invalid_args", "{why}: {msg}");
        assert!(msg.contains(why), "{why}: {msg}");
    }
    let text = put(&hub, &worker, &sid, "notes.txt", None, b"n").await;
    let elsewhere = upload_site(&hub, &worker, &other_sid, &page()).await;
    for id in [text["id"].clone(), elsewhere["id"].clone(), json!(rid())] {
        let f = publish(
            &mut w,
            &token,
            &rid(),
            json!({"artifact_id": id, "path": "/w/hqc/report"}),
        )
        .await;
        assert_eq!(refused(&f).0, "not_found", "{f}");
    }
    let ok = upload_site(&hub, &worker, &sid, &page()).await;
    for path in ["", "/w/hqc/\u{202e}lmth.exe", "/w/hqc/a\nb"] {
        let f = publish(
            &mut w,
            &token,
            &rid(),
            json!({"artifact_id": ok["id"], "path": path}),
        )
        .await;
        assert_eq!(refused(&f).0, "invalid_args", "{path:?}: {f}");
    }
    let f = call(
        &mut w,
        &token,
        "hqc_attach",
        &rid(),
        json!({"artifact_id": ok["id"]}),
    )
    .await;
    assert_eq!(refused(&f).0, "invalid_args", "{f}");
    // The user cannot upload one.
    let (s, v) = hub
        .upload("report", "application/zip", page(), Some("site"))
        .await;
    assert_eq!(s, 400, "{v}");
    let events = hub.events(&sid).await;
    assert!(
        of_type(&events, "approval.request").is_empty(),
        "{events:?}"
    );
    assert!(of_type(&events, "attachment").is_empty(), "{events:?}");
    assert_eq!(open_publications(&hub, &sid).await, 0);
    // A published one is not published again.
    let (aid, _) = published(&hub, &mut w, &worker, &sid, &token).await;
    let f = publish(
        &mut w,
        &token,
        &rid(),
        json!({"artifact_id": aid, "path": "/w/hqc/report"}),
    )
    .await;
    assert_eq!(refused(&f).0, "not_found", "{f}");
    hub.finish().await;
}

/// Without `HQC_PUBLISH_BASE` the tool is `unavailable` before any card.
#[tokio::test]
async fn without_a_publication_domain_nothing_is_published() {
    let hub = TestHub::start().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let a = upload_site(&hub, &worker, &sid, &page()).await;
    let f = publish(
        &mut w,
        &token,
        &rid(),
        json!({"artifact_id": a["id"], "path": "/w/hqc/report"}),
    )
    .await;
    let (code, msg) = refused(&f);
    assert_eq!(code, "unavailable");
    assert!(msg.contains("HQC_PUBLISH_BASE"), "{msg}");
    assert!(of_type(&hub.events(&sid).await, "approval.request").is_empty());
    assert_eq!(open_publications(&hub, &sid).await, 0);
    hub.finish().await;
}

/// The owner unpublishes: the link is `404` at once, `public_url` is gone; a second time
/// changes nothing. The call's own result is not touched.
#[tokio::test]
async fn unpublishing_takes_the_link_down() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let (aid, url) = published(&hub, &mut w, &worker, &sid, &token).await;
    let path = url.strip_prefix(BASE).unwrap().to_string();
    assert_eq!(view(&hub, &path, &[]).await.status().as_u16(), 200);

    let s = hub
        .delete(&format!("/api/artifacts/{aid}/publication"))
        .await;
    assert_eq!(s, 204);
    let r = view(&hub, &path, &[]).await;
    assert_eq!(r.status().as_u16(), 404);
    assert_eq!(
        view(&hub, &format!("{path}index.html"), &[])
            .await
            .status()
            .as_u16(),
        404
    );
    assert_eq!(row(&hub, &aid).await, (None, None));
    let (_, meta) = hub.get(&format!("/api/artifacts/{aid}")).await;
    assert!(meta.get("public_url").is_none(), "{meta}");
    assert_eq!(
        hub.delete(&format!("/api/artifacts/{aid}/publication"))
            .await,
        204
    );
    assert_eq!(
        hub.delete(&format!("/api/artifacts/{}/publication", rid()))
            .await,
        404
    );
    // The feed keeps the attachment: the card shows it as no longer published.
    assert_eq!(of_type(&hub.events(&sid).await, "attachment").len(), 1);
    hub.finish().await;
}

fn blob(hub: &TestHub, sha: &str) -> std::path::PathBuf {
    hub.app.config.artifacts.dir.join(&sha[..2]).join(sha)
}

async fn age(hub: &TestHub) {
    sqlx::query("UPDATE artifacts SET updated_at = now() - interval '40 days'")
        .execute(&hub.app.db)
        .await
        .unwrap();
}

/// The daily cleanup leaves a published `site` alone while its session lives, archived too;
/// deleting the session takes the publication down and the cleanup then removes it as any
/// unpublished artifact. One never published goes as any other artifact.
#[tokio::test]
async fn cleanup_keeps_published_sites_while_their_session_lives() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let (aid, url) = published(&hub, &mut w, &worker, &sid, &token).await;
    let unpublished = upload_site(&hub, &worker, &sid, &zip_of(&[("index.html", b"x")])).await;
    let (_, meta) = hub.get(&format!("/api/artifacts/{aid}")).await;
    let sha = meta["sha256"].as_str().unwrap().to_string();
    let path = url.strip_prefix(BASE).unwrap().to_string();
    let (s, v) = hub
        .post(&format!("/api/sessions/{sid}/archive"), json!({}))
        .await;
    assert_eq!(s, 200, "{v}");
    age(&hub).await;
    let keep = Duration::from_secs(30 * 86_400);

    let n = hqc_hub::artifacts::cleanup(&hub.app, keep).await.unwrap();
    assert_eq!(n, 1, "the unpublished one");
    let left: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM artifacts")
        .fetch_all(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(left, [aid.parse::<Uuid>().unwrap()]);
    assert_ne!(unpublished["id"], aid.as_str());
    assert!(blob(&hub, &sha).exists());
    assert_eq!(view(&hub, &path, &[]).await.status().as_u16(), 200);

    assert_eq!(hub.delete(&format!("/api/sessions/{sid}")).await, 204);
    assert_eq!(view(&hub, &path, &[]).await.status().as_u16(), 404);
    age(&hub).await;
    assert_eq!(
        hqc_hub::artifacts::cleanup(&hub.app, keep).await.unwrap(),
        1
    );
    assert!(!blob(&hub, &sha).exists());
    hub.finish().await;
}

/// Deleting the session that published a page takes the page down: its link is `404` and its
/// `public_url` is gone, as after unpublishing.
#[tokio::test]
async fn deleting_the_session_takes_its_publication_down() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let (aid, url) = published(&hub, &mut w, &worker, &sid, &token).await;
    let path = url.strip_prefix(BASE).unwrap().to_string();
    assert_eq!(view(&hub, &path, &[]).await.status().as_u16(), 200);

    assert_eq!(hub.delete(&format!("/api/sessions/{sid}")).await, 204);
    assert_eq!(view(&hub, &path, &[]).await.status().as_u16(), 404);
    let page = format!("{path}index.html");
    assert_eq!(view(&hub, &page, &[]).await.status().as_u16(), 404);
    let (s, meta) = hub.get(&format!("/api/artifacts/{aid}")).await;
    assert_eq!(s, 200, "{meta}");
    assert!(meta.get("public_url").is_none(), "{meta}");

    // An input of another session that names the artifact does not bring the link back.
    w.expect("session.kill").await;
    let (sid2, _) = session(&hub, &mut w, "Other").await;
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid2}/input"),
            json!({"text": "", "artifact_ids": [aid], "client_op_id": "op-revive"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(view(&hub, &path, &[]).await.status().as_u16(), 404);
    hub.finish().await;
}

/// Deleting a project takes down the pages its sessions published.
#[tokio::test]
async fn deleting_the_project_takes_its_publications_down() {
    let hub = hub().await;
    let (_, worker) = hub.enroll("pages-mac").await;
    let (mut w, _) = FakeWorker::online_bare(&hub, &worker, "/p", hub.boot_id).await;
    let (s, project) = hub.post("/api/projects", json!({"name": "Pages"})).await;
    assert_eq!(s, 201, "{project}");
    let pid = project["id"].as_str().unwrap();
    let (s, v) = hub.add_workspace(&mut w, pid, "/p/site").await;
    assert_eq!(s, 201, "{v}");
    let (s, v) = hub
        .post(
            &format!("/api/projects/{pid}/sessions"),
            json!({"title": "Report"}),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    let sid = v["id"].as_str().unwrap().to_string();
    let f = ack_start(&mut w, &sid).await;
    let token = f["payload"]["session_token"].as_str().unwrap().to_string();
    let (_, url) = published(&hub, &mut w, &worker, &sid, &token).await;
    let path = url.strip_prefix(BASE).unwrap().to_string();
    assert_eq!(view(&hub, &path, &[]).await.status().as_u16(), 200);

    assert_eq!(hub.delete(&format!("/api/projects/{pid}")).await, 204);
    assert_eq!(view(&hub, &path, &[]).await.status().as_u16(), 404);
    hub.finish().await;
}

/// Every one of the publication streams is taken: `503` with `Retry-After: 5`; one freed, the
/// page is served again.
#[tokio::test]
async fn a_page_waits_for_a_free_stream() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let (_, url) = published(&hub, &mut w, &worker, &sid, &token).await;
    let path = url.strip_prefix(BASE).unwrap().to_string();
    let held = hub
        .app
        .publish_streams
        .clone()
        .acquire_many_owned(hqc_hub::publish::MAX_STREAMS as u32)
        .await
        .unwrap();
    let r = view(&hub, &path, &[]).await;
    assert_eq!(r.status().as_u16(), 503);
    assert_eq!(header(&r, "retry-after"), "5");
    drop(held);
    let r = view(&hub, &path, &[]).await;
    assert_eq!(r.status().as_u16(), 200);
    assert_eq!(r.text().await.unwrap(), "<p>hi</p>");
    hub.finish().await;
}

/// `auto_approve` on the session does not answer a publication: the call still waits for the
/// owner, nothing is published.
#[tokio::test]
async fn auto_approve_leaves_a_publication_to_the_owner() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let (s, v) = hub
        .patch(
            &format!("/api/sessions/{sid}"),
            json!({"auto_approve": true}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let a = upload_site(&hub, &worker, &sid, &page()).await;
    let rid = rid();
    let f = publish(
        &mut w,
        &token,
        &rid,
        json!({"artifact_id": a["id"], "path": "/w/hqc/report"}),
    )
    .await;
    assert_eq!(f["type"], "ack", "{f}");
    w.expect_silence(Duration::from_millis(300)).await;
    assert_eq!(open_publications(&hub, &sid).await, 1);
    let events = hub.events(&sid).await;
    assert_eq!(of_type(&events, "approval.request").len(), 1, "{events:?}");
    assert!(
        of_type(&events, "approval.response").is_empty(),
        "{events:?}"
    );
    assert!(of_type(&events, "attachment").is_empty(), "{events:?}");
    assert_eq!(row(&hub, a["id"].as_str().unwrap()).await, (None, None));
    hub.finish().await;
}

/// `mcp.cancel` of the waiting call (the tool timeout): `approval.cancel {stopped}`, nothing
/// goes to the worker, nothing is published, and an answer after it is `409`.
#[tokio::test]
async fn mcp_cancel_closes_a_publication_request() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let a = upload_site(&hub, &worker, &sid, &page()).await;
    let rid = rid();
    let f = publish(
        &mut w,
        &token,
        &rid,
        json!({"artifact_id": a["id"], "path": "/w/hqc/report"}),
    )
    .await;
    assert_eq!(f["type"], "ack", "{f}");
    let id = w
        .send("mcp.cancel", json!({"request_id": rid, "reason": "eof"}))
        .await;
    assert_eq!(w.expect("ack").await["id"], id.as_str());
    assert_eq!(open_publications(&hub, &sid).await, 0);
    let events = hub.events(&sid).await;
    let cancel = of_type(&events, "approval.cancel");
    assert_eq!(cancel.len(), 1, "{events:?}");
    assert_eq!(
        cancel[0]["payload"],
        json!({"request_id": rid, "reason": "stopped"})
    );
    let (s, v) = hub
        .post(
            &format!("/api/sessions/{sid}/requests/{rid}"),
            json!({"option_id": "once"}),
        )
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (409, Some("request_closed")),
        "{v}"
    );
    w.expect_silence(Duration::from_millis(300)).await;
    assert_eq!(row(&hub, a["id"].as_str().unwrap()).await, (None, None));
    hub.finish().await;
}

async fn stored_result(hub: &TestHub, rid: &str) -> Option<Value> {
    sqlx::query_scalar("SELECT result FROM pending_requests WHERE request_id = $1")
        .bind(rid)
        .fetch_one(&hub.app.db)
        .await
        .unwrap()
}

/// A worker's own approval may reuse the request id of its publication call, and the feed then
/// has an `approval.response` with `once` for that id: the hub's own (`auto`) or the owner's
/// click on the worker's card. Neither is the owner's answer to the publication, so a call
/// cancelled afterwards is not published when the hub starts again; one the owner allowed is.
#[tokio::test]
async fn a_look_alike_answer_does_not_publish_after_a_hub_restart() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let session = hqc_hub::db::session(&hub.app.db, sid.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let a = upload_site(&hub, &worker, &sid, &page()).await;
    let aid = a["id"].as_str().unwrap().to_string();
    let cancelled = rid();
    let f = publish(
        &mut w,
        &token,
        &cancelled,
        json!({"artifact_id": aid, "path": "/w/hqc/report"}),
    )
    .await;
    assert_eq!(f["type"], "ack", "{f}");
    for (author, by) in [
        (json!({"kind": "system"}), "auto".to_string()),
        (
            hqc_hub::events::user_author(session.owner_id),
            session.owner_id.to_string(),
        ),
    ] {
        hqc_hub::events::store_hub_event(
            &hub.app,
            &session,
            author,
            "approval.response",
            json!({"request_id": cancelled, "option_id": "once", "by": by}),
            None,
            None,
        )
        .await
        .unwrap();
    }
    let id = w
        .send(
            "mcp.cancel",
            json!({"request_id": cancelled, "reason": "eof"}),
        )
        .await;
    assert_eq!(w.expect("ack").await["id"], id.as_str());
    // A publication the owner allowed, whose result only is not stored.
    let b = upload_site(&hub, &worker, &sid, &page()).await;
    let allowed = rid();
    let f = publish(
        &mut w,
        &token,
        &allowed,
        json!({"artifact_id": b["id"], "path": "/w/hqc/report"}),
    )
    .await;
    assert_eq!(f["type"], "ack", "{f}");
    hqc_hub::events::store_hub_event(
        &hub.app,
        &session,
        hqc_hub::events::user_author(session.owner_id),
        "approval.response",
        json!({"request_id": allowed, "option_id": "once", "by": session.owner_id.to_string()}),
        None,
        None,
    )
    .await
    .unwrap();
    close_allowed(&hub, "publish", &allowed, session.owner_id).await;

    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let hub = hub.restart().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while stored_result(&hub, &allowed).await.is_none() {
        assert!(tokio::time::Instant::now() < deadline, "no result stored");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(row(&hub, &aid).await, (None, None));
    assert_eq!(stored_result(&hub, &cancelled).await, None);
    let (at, public_id) = row(&hub, b["id"].as_str().unwrap()).await;
    assert!(at.is_some() && public_id.is_some());
    hub.finish().await;
}

/// What the owner's `once` leaves on the request of `kind` when the hub stops before the
/// result is stored: the row closed without a result, with the owner's mark.
async fn close_allowed(hub: &TestHub, kind: &str, rid: &str, by: Uuid) {
    let id: Uuid =
        sqlx::query_scalar("SELECT id FROM pending_requests WHERE kind = $1 AND request_id = $2")
            .bind(kind)
            .bind(rid)
            .fetch_one(&hub.app.db)
            .await
            .unwrap();
    assert!(hqc_hub::db::close_allowed(&hub.app.db, id, by)
        .await
        .unwrap());
}

/// The hub stopped after the owner allowed publications, before their results were stored
/// (SPEC §3 PendingRequest). Started again, it carries them out at once, the worker away: one
/// not done yet is published, one done before the stop answers from its attachment with the
/// same link; each call gets its result after the worker's `hello`.
#[tokio::test]
async fn an_allowed_publication_is_carried_out_after_a_hub_restart() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let session = hqc_hub::db::session(&hub.app.db, sid.parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    let a = upload_site(&hub, &worker, &sid, &page()).await;
    let aid = a["id"].as_str().unwrap().to_string();
    let later = rid();
    let f = publish(
        &mut w,
        &token,
        &later,
        json!({"artifact_id": aid, "path": "/w/hqc/report"}),
    )
    .await;
    assert_eq!(f["type"], "ack", "{f}");
    // What `once` leaves when the hub stops before the publication: the answer in the feed,
    // the request closed without a result, marked with the owner.
    hqc_hub::events::store_hub_event(
        &hub.app,
        &session,
        hqc_hub::events::user_author(session.owner_id),
        "approval.response",
        json!({"request_id": later, "option_id": "once", "by": session.owner_id.to_string()}),
        None,
        None,
    )
    .await
    .unwrap();
    close_allowed(&hub, "publish", &later, session.owner_id).await;
    // One published before the stop, whose result only is not stored.
    let (done_aid, done_url) = published(&hub, &mut w, &worker, &sid, &token).await;
    let done: String = sqlx::query_scalar(
        "SELECT request_id FROM pending_requests WHERE kind = 'publish' AND result IS NOT NULL",
    )
    .fetch_one(&hub.app.db)
    .await
    .unwrap();
    // Its `ack` reached the hub first: the result is not delivered once more because of it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let acked: bool = sqlx::query_scalar(
            "SELECT result_delivered_at IS NOT NULL FROM pending_requests WHERE request_id = $1",
        )
        .bind(&done)
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
        if acked {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no ack");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    sqlx::query(
        "UPDATE pending_requests SET result = NULL, result_delivered_at = NULL
         WHERE request_id = $1",
    )
    .bind(&done)
    .execute(&hub.app.db)
    .await
    .unwrap();
    assert_eq!(row(&hub, &aid).await, (None, None));

    w.close().await;
    hub.wait_session(&sid, "worker_offline", |v| v["state"] == "worker_offline")
        .await;
    let hub = hub.restart().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while stored_result(&hub, &later).await.is_none() || stored_result(&hub, &done).await.is_none()
    {
        assert!(tokio::time::Instant::now() < deadline, "no results stored");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (at, id) = row(&hub, &aid).await;
    assert!(at.is_some() && id.is_some());
    let mut w = FakeWorker::connect(&hub, &worker).await.unwrap();
    w.hello(WS_PATH, hub.boot_id).await;
    let mut results = std::collections::HashMap::new();
    while results.len() < 2 {
        let f = w.recv().await;
        match f["type"].as_str().unwrap() {
            "session.start" => w.reply(&f["id"], "ack", json!({})).await,
            "mcp.result" => {
                w.reply(&f["id"], "ack", json!({})).await;
                let p = &f["payload"];
                results.insert(p["request_id"].as_str().unwrap().to_string(), p.clone());
            }
            _ => {}
        }
    }
    assert_eq!(
        results[&later]["result"]["url"],
        format!("{BASE}/{}/", id.unwrap()),
        "{results:?}"
    );
    assert_eq!(results[&done]["result"]["url"], done_url, "{results:?}");
    let events = hub.events(&sid).await;
    let att = of_type(&events, "attachment");
    assert_eq!(att.len(), 2, "{events:?}");
    for (rid, aid) in [(&later, &aid), (&done, &done_aid)] {
        let e = att
            .iter()
            .find(|e| e["payload"]["artifact_id"] == aid.as_str())
            .unwrap_or_else(|| panic!("{events:?}"));
        assert_eq!(results[rid]["result"]["event_id"], e["event_id"]);
    }
    hub.finish().await;
}

/// The local header of the one empty stored `index.html` of the hand-written archives below
/// (the CRC-32 of nothing is 0). The crate's writer refuses a repeated name, so their bytes
/// are written by hand.
fn local_index() -> Vec<u8> {
    let name = b"index.html";
    let mut z = Vec::new();
    z.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    z.extend_from_slice(&[10, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // version, flags, stored, time, date
    z.extend_from_slice(&[0; 12]); // CRC-32, compressed and uncompressed size
    z.extend_from_slice(&(name.len() as u16).to_le_bytes());
    z.extend_from_slice(&[0, 0]); // extra field length
    z.extend_from_slice(name);
    z
}

/// A central directory starting at `at` with one record per `names`, each with compression
/// `method` and `extra` bytes of empty zip64 blocks as its extra field, all pointing at the
/// local header at offset 0, and its end record.
fn directory(names: &[String], extra: usize, method: u16, at: usize) -> Vec<u8> {
    let mut z = Vec::new();
    for n in names {
        z.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        z.extend_from_slice(&[10, 0, 10, 0, 0, 0]); // versions, flags
        z.extend_from_slice(&method.to_le_bytes());
        z.extend_from_slice(&[0; 4]); // time, date
        z.extend_from_slice(&[0; 12]); // CRC-32, sizes
        z.extend_from_slice(&(n.len() as u16).to_le_bytes());
        z.extend_from_slice(&(extra as u16).to_le_bytes());
        z.extend_from_slice(&[0; 6]); // comment length, disk, internal attributes
        z.extend_from_slice(&[0; 4]); // external attributes
        z.extend_from_slice(&0u32.to_le_bytes()); // offset of the local header
        z.extend_from_slice(n.as_bytes());
        z.extend_from_slice(&[1, 0, 0, 0].repeat(extra / 4));
    }
    let size = z.len() as u32;
    z.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    z.extend_from_slice(&[0; 4]); // disk numbers
    z.extend_from_slice(&(names.len() as u16).to_le_bytes());
    z.extend_from_slice(&(names.len() as u16).to_le_bytes());
    z.extend_from_slice(&size.to_le_bytes());
    z.extend_from_slice(&(at as u32).to_le_bytes());
    z.extend_from_slice(&[0, 0]); // comment length
    z
}

/// A zip of the one `index.html` whose central directory has one stored record per `names`.
fn zip_with_directory(names: &[String], extra: usize) -> Vec<u8> {
    let mut z = local_index();
    let at = z.len();
    z.extend(directory(names, extra, 0, at));
    z
}

/// What `inspect` says of these archive bytes.
fn inspected_bytes(bytes: &[u8]) -> Result<hqc_hub::publish::Site, String> {
    let path = std::env::temp_dir().join(format!("hqc-inspect-{}.zip", Uuid::new_v4()));
    std::fs::write(&path, bytes).unwrap();
    let r = hqc_hub::publish::inspect(&path);
    std::fs::remove_file(&path).unwrap();
    r
}

/// What `inspect` says of the archive with these central directory names.
fn inspected(names: &[String]) -> Result<hqc_hub::publish::Site, String> {
    inspected_bytes(&zip_with_directory(names, 0))
}

#[test]
fn a_directory_that_repeats_a_name_is_refused() {
    let one = || "index.html".to_string();
    assert!(inspected(&[one()]).is_ok());
    let e = inspected(&[one(), one()]).unwrap_err();
    assert!(e.contains("more than once"), "{e}");
}

#[test]
fn a_directory_of_more_records_than_the_limit_is_refused() {
    let names = vec!["index.html".to_string(); hqc_proto::mcp::MAX_SITE_FILES + 1];
    let e = inspected(&names).unwrap_err();
    assert!(
        e.contains(&format!(
            "more than {} entries",
            hqc_proto::mcp::MAX_SITE_FILES
        )),
        "{e}"
    );
}

#[test]
fn a_record_with_a_large_extra_field_is_refused() {
    let e = inspected_bytes(&zip_with_directory(&["index.html".to_string()], 65_532)).unwrap_err();
    assert!(e.contains("an extra field or a comment"), "{e}");
}

/// The zip reader, failing on the last directory, reads the one of an earlier end record: here
/// the last one's record has the AES method without its extra field, and a directory nobody
/// checked lies before it.
#[test]
fn a_directory_the_reader_would_fail_on_is_refused() {
    let names = ["index.html".to_string()];
    let mut z = local_index();
    let hidden = z.len();
    z.extend(directory(&names, 0, 0, hidden));
    let last = z.len();
    z.extend(directory(&names, 0, 99, last));
    let e = inspected_bytes(&z).unwrap_err();
    assert!(e.contains("neither stored nor deflated"), "{e}");
}

/// An archive packed as the worker packs it (`write_zip` in the worker's `mcp/publish.rs`), at
/// the limits, passes.
#[test]
fn an_archive_at_the_limits_is_published() {
    let path = std::env::temp_dir().join(format!("hqc-inspect-{}.zip", Uuid::new_v4()));
    let mut z = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
    let o = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    let max = hqc_proto::mcp::MAX_SITE_FILES;
    for i in 0..max {
        let name = if i == 0 {
            "index.html".to_string()
        } else {
            format!("{i:05}{}", "a".repeat(hqc_proto::mcp::MAX_SITE_NAME - 5))
        };
        z.start_file(name, o).unwrap();
    }
    z.finish().unwrap();
    let site = hqc_hub::publish::inspect(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(site.files.len(), max);
}

/// SPEC §10.1: a publication whose call's id an open request of another kind holds is refused at
/// once, not left waiting for an answer that cannot reach it.
#[tokio::test]
async fn a_publication_whose_id_another_open_request_holds_is_refused_at_once() {
    let hub = hub().await;
    let (worker, mut w, sid, token) = agent(&hub).await;
    let a = upload_site(&hub, &worker, &sid, &page()).await;
    let aid = a["id"].as_str().unwrap();
    let rid = rid();
    assert!(hqc_hub::db::open_request(
        &hub.app.db,
        "approval",
        sid.parse().unwrap(),
        &rid,
        &json!({}),
        None,
        None,
    )
    .await
    .unwrap());
    let f = publish(
        &mut w,
        &token,
        &rid,
        json!({"artifact_id": aid, "path": "/w/hqc/report"}),
    )
    .await;
    let (code, message) = refused(&f);
    assert_eq!(code, "unavailable");
    assert_eq!(message, hqc_hub::db::REQUEST_ID_TAKEN);
    assert_eq!(open_publications(&hub, &sid).await, 0);
    hub.finish().await;
}
