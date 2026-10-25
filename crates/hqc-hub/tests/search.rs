//! T33 (SPEC §15, §7): `GET /api/search` and `hqc_search_history` over the index of
//! migration 0016: exact and fuzzy hits, Russian and English, only the owner's sessions
//! (service sessions left out, archived ones marked), paging, the backfill of old rows.

mod common;

use std::time::Instant;

use chrono::{DateTime, TimeDelta, Utc};
use common::*;
use hqc_hub::db::{self, Inserted, NewEvent};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

const WS_PATH: &str = "/Users/home-mac/work/hqc";

async fn me(hub: &TestHub) -> (Uuid, Uuid) {
    let (_, me) = hub.get("/api/me").await;
    (
        me["id"].as_str().unwrap().parse().unwrap(),
        me["personal_project_id"].as_str().unwrap().parse().unwrap(),
    )
}

/// A session row straight in the database (no worker needed to search it).
async fn session(db: &PgPool, owner: Uuid, project: Uuid, slug: &str, title: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO sessions (id, project_id, owner_id, title, slug, created_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(project)
    .bind(owner)
    .bind(title)
    .bind(slug)
    .bind(t(0))
    .execute(db)
    .await
    .unwrap();
    id
}

/// An event through the hub's own insert path.
async fn event(db: &PgPool, session: Uuid, ty: &str, payload: Value, ts: DateTime<Utc>) -> i64 {
    let e = NewEvent {
        event_id: Uuid::now_v7(),
        session_id: session,
        ts,
        author: json!({"kind": "system"}),
        kind: ty.into(),
        payload,
        chain: None,
        client_op_id: None,
        delivery: None,
        delivered_at: None,
    };
    match db::insert_event(db, &e).await.unwrap() {
        Inserted::New(r) => r.seq,
        other => panic!("{other:?}"),
    }
}

fn t(minutes: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-20T10:00:00Z")
        .unwrap()
        .to_utc()
        + TimeDelta::minutes(minutes)
}

async fn search(hub: &TestHub, query: &str) -> Value {
    let (s, v) = hub.get(&format!("/api/search?{query}")).await;
    assert_eq!(s, 200, "{query}: {v}");
    v
}

/// `(address, seq)` of each hit.
fn hits(v: &Value) -> Vec<(String, Option<i64>)> {
    v["results"]
        .as_array()
        .unwrap_or_else(|| panic!("{v}"))
        .iter()
        .map(|h| {
            (
                h["address"].as_str().unwrap().to_string(),
                h["seq"].as_i64(),
            )
        })
        .collect()
}

/// Percent-encoded for a query string.
fn q(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

async fn tool(w: &mut FakeWorker, token: &str, args: Value) -> Value {
    let rid = Uuid::now_v7().to_string();
    let id = w
        .send(
            "mcp.call",
            json!({"request_id": rid, "session_token": token,
                   "tool": "hqc_search_history", "args": args}),
        )
        .await;
    let r = w.expect("mcp.result").await;
    assert_eq!(r["id"], id.as_str(), "{r}");
    r["payload"].clone()
}

#[tokio::test]
async fn exact_and_fuzzy_hits_of_the_owner_only() {
    let hub = TestHub::start().await;
    let db = hub.app.db.clone();
    let (owner, personal) = me(&hub).await;
    let work = Uuid::now_v7();
    sqlx::query("INSERT INTO projects (id, owner_id, name, slug) VALUES ($1, $2, 'Work', 'work')")
        .bind(work)
        .bind(owner)
        .execute(&db)
        .await
        .unwrap();
    let a = session(&db, owner, personal, "deploy", "Deploy the hub").await;
    let b = session(&db, owner, work, "notes", "Заметки по миграции").await;
    let user = |text: &str| json!({"text": text, "artifact_ids": []});
    let agent = |text: &str| json!({"text": text, "final": true});
    let a1 = event(
        &db,
        a,
        "user.message",
        user("Please deploy the hub to production"),
        t(1),
    )
    .await;
    let a2 = event(
        &db,
        a,
        "agent.message",
        agent("Deployment finished, the tunnel is up"),
        t(2),
    )
    .await;
    // Tool output is not indexed.
    event(
        &db,
        a,
        "tool.call",
        json!({"call_id": "c1", "tool": "Bash", "args_preview": "deploy --tunnel"}),
        t(3),
    )
    .await;
    event(
        &db,
        a,
        "tool.result",
        json!({"call_id": "c1", "result_preview": "tunnel deploy ok", "error": false}),
        t(4),
    )
    .await;
    let b1 = event(
        &db,
        b,
        "user.message",
        user("Проверь миграцию базы данных"),
        t(5),
    )
    .await;
    let b2 = event(
        &db,
        b,
        "peer.message",
        json!({"from_session_id": a, "text": "Миграции готовы", "mode": "send"}),
        t(6),
    )
    .await;
    let b3 = event(
        &db,
        b,
        "system",
        json!({"text": "The tunnel was restarted", "code": "turn_lost"}),
        t(7),
    )
    .await;

    // Another user's session with the same words, and the owner's service session.
    let other = Uuid::now_v7();
    let other_project = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, email, display_name) VALUES ($1, $2, 'other')")
        .bind(other)
        .bind(format!("other-{other}@example.com"))
        .execute(&db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO projects (id, owner_id, name, slug) VALUES ($1, $2, 'Personal', 'personal')",
    )
    .bind(other_project)
    .bind(other)
    .execute(&db)
    .await
    .unwrap();
    let foreign = session(&db, other, other_project, "deploy", "Deploy the hub").await;
    event(
        &db,
        foreign,
        "user.message",
        user("deploy the hub, tunnel and миграцию"),
        t(8),
    )
    .await;
    let sys = session(&db, owner, personal, "home-mac", "home-mac").await;
    sqlx::query("UPDATE sessions SET system = true WHERE id = $1")
        .bind(sys)
        .execute(&db)
        .await
        .unwrap();
    event(
        &db,
        sys,
        "system",
        json!({"text": "deploy tunnel миграцию", "code": "worker_log", "level": "warn"}),
        t(9),
    )
    .await;

    let pa = |seq| ("personal/deploy".to_string(), Some(seq));
    let wb = |seq| ("work/notes".to_string(), Some(seq));

    // Exact words, English: newest first; tool output and others' sessions never appear.
    let v = search(&hub, "q=tunnel").await;
    assert_eq!(hits(&v), vec![wb(b3), pa(a2)], "{v}");
    assert!(v.get("next_cursor").is_none());
    let h = &v["results"][1];
    assert_eq!(h["session_id"], a.to_string());
    assert_eq!(h["title"], "Deploy the hub");
    assert_eq!(h["type"], "agent.message");
    assert_eq!(h["archived"], false);
    assert_eq!(h["snippet"], "Deployment finished, the tunnel is up");
    // The title is searched too: a hit without `seq`, at the session's creation.
    let v = search(&hub, "q=deploy").await;
    assert_eq!(
        hits(&v),
        vec![pa(a1), ("personal/deploy".to_string(), None)],
        "{v}"
    );
    assert_eq!(v["results"][1]["snippet"], "Deploy the hub");
    // Exact means the word: "deployment" is another word in `simple`.
    assert!(!hits(&search(&hub, "q=deployment").await).contains(&pa(a1)));
    // A phrase, and `-word`.
    assert_eq!(
        hits(&search(&hub, &format!("q={}", q("\"the tunnel is up\""))).await),
        vec![pa(a2)]
    );
    assert_eq!(
        hits(&search(&hub, &format!("q={}", q("tunnel -restarted"))).await),
        vec![pa(a2)]
    );

    // Russian: exact needs the same form, fuzzy finds the others.
    let v = search(&hub, &format!("q={}", q("миграцию"))).await;
    assert_eq!(hits(&v), vec![wb(b1)], "{v}");
    let v = search(&hub, &format!("q={}&fuzzy=1", q("миграция"))).await;
    let got = hits(&v);
    for want in [wb(b1), wb(b2), ("work/notes".to_string(), None)] {
        assert!(got.contains(&want), "{want:?} in {v}");
    }
    // Fuzzy English: another form and a typo.
    let got = hits(&search(&hub, "q=deploying&fuzzy=true").await);
    assert!(got.contains(&pa(a1)) && got.contains(&pa(a2)), "{got:?}");
    let got = hits(&search(&hub, "q=tunnle&fuzzy=1").await);
    assert!(got.contains(&pa(a2)), "{got:?}");
    // Case does not matter.
    assert_eq!(hits(&search(&hub, "q=TUNNEL").await).len(), 2);

    // One project, by id or slug; another user's project is a 404.
    let v = search(&hub, &format!("q=tunnel&project={work}")).await;
    assert_eq!(hits(&v), vec![wb(b3)]);
    assert_eq!(
        hits(&search(&hub, "q=tunnel&project=personal").await),
        vec![pa(a2)]
    );
    let (s, _) = hub
        .get(&format!("/api/search?q=tunnel&project={other_project}"))
        .await;
    assert_eq!(s, 404);

    // Nothing of the other user or the service session, whatever the query.
    for query in [
        "q=deploy",
        "q=hub&fuzzy=1",
        "q=tunnel%20or%20deploy",
        "q=home-mac",
    ] {
        for h in search(&hub, query).await["results"].as_array().unwrap() {
            let sid = h["session_id"].as_str().unwrap();
            assert!(
                sid != foreign.to_string() && sid != sys.to_string(),
                "{query}: {h}"
            );
        }
    }

    // Archived sessions are found and marked.
    sqlx::query("UPDATE sessions SET archived = true WHERE id = $1")
        .bind(b)
        .execute(&db)
        .await
        .unwrap();
    let v = search(&hub, "q=tunnel").await;
    assert_eq!(v["results"][0]["archived"], true);
    assert_eq!(v["results"][1]["archived"], false);

    // Refused queries.
    for bad in ["q=", "q=%20%20", "q=ab&fuzzy=1", "q=tunnel&cursor=nonsense"] {
        let (s, v) = hub.get(&format!("/api/search?{bad}")).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (400, Some("bad_request")),
            "{bad}: {v}"
        );
    }
    let (s, _) = hub.get(&format!("/api/search?q={}", "x".repeat(201))).await;
    assert_eq!(s, 400);
    hub.finish().await;
}

#[tokio::test]
async fn pages_follow_the_cursor() {
    let hub = TestHub::start().await;
    let db = hub.app.db.clone();
    let (owner, personal) = me(&hub).await;
    let s = session(&db, owner, personal, "long", "Long").await;
    let mut seqs = Vec::new();
    for n in 0..25 {
        // Two events at the same time each: the id breaks the tie.
        let text = format!("needle number {n}");
        seqs.push(
            event(
                &db,
                s,
                "agent.message",
                json!({"text": text, "final": false}),
                t(n / 2),
            )
            .await,
        );
    }
    let mut got = Vec::new();
    let mut cursor = String::new();
    for page in 0.. {
        let v = search(&hub, &format!("q=needle&limit=10&cursor={}", q(&cursor))).await;
        let n = v["results"].as_array().unwrap().len();
        got.extend(hits(&v).into_iter().map(|(_, s)| s.unwrap()));
        match v["next_cursor"].as_str() {
            Some(c) => {
                assert_eq!(n, 10, "page {page}");
                cursor = c.to_string();
            }
            None => {
                assert_eq!(n, 5);
                break;
            }
        }
    }
    seqs.reverse();
    assert_eq!(got, seqs, "every hit once, newest first");
    // The limit is capped.
    let v = search(&hub, "q=needle&limit=1000").await;
    assert_eq!(v["results"].as_array().unwrap().len(), 25);
    hub.finish().await;
}

/// Migration 0016 backfills the events stored before it.
#[tokio::test]
async fn the_migration_indexes_old_events() {
    const SEED: &str = r#"
        INSERT INTO users (id, email, display_name) VALUES
            ('0199f3a2-7c10-7000-8000-0000000000aa', '{email}', 'me');
        INSERT INTO projects (id, owner_id, name, slug, is_personal) VALUES
            ('0199f3a2-7c10-7000-8000-0000000000d1', '0199f3a2-7c10-7000-8000-0000000000aa',
             'Personal', 'personal', true);
        UPDATE users SET personal_project_id = '0199f3a2-7c10-7000-8000-0000000000d1';
        INSERT INTO sessions (id, project_id, owner_id, title, slug, last_seq) VALUES
            ('0199f3a2-7c10-7000-8000-000000000001', '0199f3a2-7c10-7000-8000-0000000000d1',
             '0199f3a2-7c10-7000-8000-0000000000aa', 'Old one', 'old-one', 2);
        INSERT INTO events (event_id, session_id, seq, ts, author, type, payload) VALUES
            ('0199f3a2-7c10-7000-8000-000000000101', '0199f3a2-7c10-7000-8000-000000000001',
             1, '2026-10-01T10:00:00Z', '{"kind":"system"}', 'user.message',
             '{"text":"an old pineapple","artifact_ids":[]}'),
            ('0199f3a2-7c10-7000-8000-000000000102', '0199f3a2-7c10-7000-8000-000000000001',
             2, '2026-10-01T10:00:01Z', '{"kind":"system"}', 'tool.result',
             '{"call_id":"c","result_preview":"pineapple","error":false}');
    "#;
    let hub = TestHub::start_upgrading(16, SEED).await;
    let v = search(&hub, "q=pineapple").await;
    assert_eq!(
        hits(&v),
        vec![("personal/old-one".to_string(), Some(1))],
        "{v}"
    );
    hub.finish().await;
}

#[tokio::test]
async fn the_tool_searches_for_the_calling_sessions_owner() {
    let hub = TestHub::start().await;
    let (_, token) = hub.enroll("one").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("Searcher").await;
    let f = ack_start(&mut w, &sid).await;
    let session_token = f["payload"]["session_token"].as_str().unwrap().to_string();
    let sid_uuid: Uuid = sid.parse().unwrap();
    let seq = event(
        &hub.app.db,
        sid_uuid,
        "agent.message",
        json!({"text": "The answer is pineapple", "final": true}),
        Utc::now(),
    )
    .await;

    let r = tool(&mut w, &session_token, json!({"query": "pineapple"})).await;
    let res = &r["result"];
    assert_eq!(res["results"].as_array().unwrap().len(), 1, "{r}");
    let h = &res["results"][0];
    assert_eq!(h["address"], "personal/searcher");
    assert_eq!(h["seq"], seq);
    assert_eq!(h["type"], "agent.message");
    assert_eq!(h["snippet"], "The answer is pineapple");
    assert!(h.get("session_id").is_none() && res.get("next_cursor").is_none());
    let args = json!({"query": "pinapple", "fuzzy": true, "project": "personal"});
    let r = tool(&mut w, &session_token, args).await;
    assert_eq!(r["result"]["results"][0]["seq"], seq, "{r}");
    for (args, code) in [
        (json!({"query": ""}), "invalid_args"),
        (json!({"query": "x", "fuzzy": true}), "invalid_args"),
        (json!({"query": "x", "cursor": "bad"}), "invalid_args"),
        (json!({"qery": "x"}), "invalid_args"),
        (json!({"query": "x", "project": "nope"}), "not_found"),
    ] {
        let r = tool(&mut w, &session_token, args.clone()).await;
        assert_eq!(r["error"]["code"], code, "{args}: {r}");
    }
    hub.finish().await;
}

/// The text of a seeded event (`g.n` is its number, `s.k` its session's): 20 words in all.
const FEW_WORDS: &str = r#"(SELECT string_agg((ARRAY['deploy','tunnel','hub','worker','migration',
    'index','test','client','session','agent','миграция','база','данных','проверь','тесты',
    'индекс','запуск','сервер','ошибка','готово'])[1 + ((g.n * 7 + s.k * 13 + i * 31) % 20)], ' ')
    FROM generate_series(1, 12 + g.n % 30) i)"#;
/// Log-uniform over 5000 words per language (word k with a weight of about 1/k, as in
/// natural text). `g.n > 0` makes Postgres evaluate it per row.
const MANY_WORDS: &str = r#"(SELECT string_agg(CASE WHEN random() < 0.5 THEN 'word' ELSE 'слово' END
    || floor(exp(random() * ln(5000)))::int, ' ') FROM generate_series(1, 12 + g.n % 30) i
    WHERE g.n > 0)"#;

/// 100 sessions × 1000 events; 60 % carry text (user / agent messages), 40 % are tool calls
/// and results; every 10 000th text has the rare word `quokka`.
async fn seed(db: &PgPool, owner: Uuid, project: Uuid, words: &str) {
    sqlx::raw_sql(&format!(
        r#"
        SELECT setseed(0.33);
        INSERT INTO sessions (id, project_id, owner_id, title, slug, last_seq)
        SELECT gen_random_uuid(), '{project}', '{owner}', 'Session ' || n, 'session-' || n, 1000
        FROM generate_series(1, 100) n;
        INSERT INTO events (event_id, session_id, seq, ts, author, type, payload)
        SELECT gen_random_uuid(), s.id, g.n,
               timestamptz '2026-01-01' + (g.n * 100 + s.k) * interval '1 second',
               '{{"kind":"system"}}',
               CASE g.n % 5 WHEN 0 THEN 'tool.call' WHEN 1 THEN 'tool.result'
                            WHEN 2 THEN 'user.message' ELSE 'agent.message' END,
               CASE WHEN g.n % 5 < 2
                    THEN jsonb_build_object('call_id', 'c', 'result_preview', 'tool output deploy')
                    ELSE jsonb_build_object('text', {words}
                        || CASE WHEN (g.n * 100 + s.k) % 10000 = 0 THEN ' quokka' ELSE '' END)
               END
        FROM (SELECT id, row_number() OVER (ORDER BY id) AS k FROM sessions
              WHERE owner_id = '{owner}' AND slug LIKE 'session-%') s,
             generate_series(1, 1000) g(n);
        ANALYZE events;
        ANALYZE sessions;
        "#
    ))
    .execute(db)
    .await
    .unwrap();
}

/// SPEC §15, T33: queries over 100k events, two corpora: 20 words in all (every word in
/// most events, the worst case for common words) and 10 000 words with natural frequencies.
/// Prints the timings; run with `cargo test --release -p hqc-hub --test search -- --ignored
/// --nocapture` (the numbers are in docs/notes/t33-questions.md).
#[tokio::test]
#[ignore]
async fn timing_over_100k_events() {
    let few: &[String] = &[
        "q=quokka".into(),
        "q=quokka&fuzzy=1".into(),
        "q=deploy".into(),
        format!("q={}", q("миграция")),
        format!("q={}", q("\"tunnel hub\"")),
        format!("q={}&fuzzy=1", q("миграцию")),
        "q=tunel&fuzzy=1".into(),
        "q=nothinglikethis".into(),
        "q=nothinglikethis&fuzzy=1".into(),
    ];
    let many: &[String] = &[
        "q=quokka".into(),
        "q=word1".into(),
        "q=word2000".into(),
        format!("q={}", q("слово30")),
        format!("q={}", q("\"word1 word2\"")),
        "q=wrd2000&fuzzy=1".into(),
        format!("q={}&fuzzy=1", q("слово")),
        "q=nothinglikethis&fuzzy=1".into(),
    ];
    for (name, words, queries) in [
        ("20 words", FEW_WORDS, few),
        ("10 000 words", MANY_WORDS, many),
    ] {
        let hub = TestHub::start().await;
        let db = hub.app.db.clone();
        let (owner, personal) = me(&hub).await;
        let started = Instant::now();
        seed(&db, owner, personal, words).await;
        let n: i64 =
            sqlx::query_scalar("SELECT count(*) FROM events WHERE search_text IS NOT NULL")
                .fetch_one(&db)
                .await
                .unwrap();
        println!(
            "{name}: seeded 100000 events ({n} with text) in {:?}",
            started.elapsed()
        );
        let sizes: (String, String, String, String) = sqlx::query_as(
            "SELECT pg_size_pretty(sum(pg_column_size(search_text))),
                    pg_size_pretty(sum(pg_column_size(search_words))),
                    pg_size_pretty(pg_relation_size('events_search_words')),
                    pg_size_pretty(pg_relation_size('events_search_trgm'))
             FROM events",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        println!("  search_text, search_words, words index, trigram index: {sizes:?}");
        for query in queries {
            let mut best = std::time::Duration::MAX;
            let mut v = Value::Null;
            for _ in 0..5 {
                let s = Instant::now();
                let (status, body) = hub.get(&format!("/api/search?{query}")).await;
                best = best.min(s.elapsed());
                v = if status == 200 {
                    body
                } else {
                    json!({"results": [], "status": status})
                };
            }
            println!(
                "  {query:50} {:>3} hits on the page{}, best of 5: {best:?}",
                v["results"].as_array().unwrap().len(),
                v.get("status")
                    .map_or(String::new(), |s| format!(" (status {s})")),
            );
        }
        hub.finish().await;
    }
}
