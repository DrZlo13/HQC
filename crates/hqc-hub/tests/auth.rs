//! Cloudflare Access JWT, OIDC ID token, worker enrollment and the worker token (SPEC §5.1,
//! §6, §20).

mod common;

use axum::{routing::get, Json, Router};
use base64::Engine;
use common::*;
use futures_util::{SinkExt, StreamExt};
use hqc_hub::config::AuthConfig;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

const TEAM: &str = "test-team.cloudflareaccess.com";
const AUD: &str = "test-aud-tag";
const KID: &str = "test-key-1";
/// Test-only key, not used anywhere else.
const KEY_PEM: &[u8] = include_bytes!("fixtures/test-access-key.pem");
/// `openssl rsa -in fixtures/test-access-key.pem -noout -modulus`.
const MODULUS_HEX: &str = "94E0DC3E10FFA7CD4E0A1084A6BE2C63A9E65C492A50BB083D2F012946EAE3671FF27B328F89DABD24B0692064BB97515010967EAA54A58764C3150380F47590A5395E44E83FAE4DE06C8431744CA5B9595E4381862FF1FB23186BF9396363099CB7FFEF39D486A5778DAFA977C9C1D3A4D1298C2B5ADBEA1AF134451E7C3B9477D2423FF9C46C235D63AE3FA0E4D4F6BDB41226021EE5A4507ED6B036545A07EADF8BD77163DAFEE87D84A63AB349396B52E0BA2486F2575DCD03AAB9BB531DEC1285CBA622C3F6CE5F6BCC3C5FD14640C7E1443FAB21ECD2FE342602C0F554CCE175B669C2458DD7442B799DDFF653B53D35393E21971F4C1EE56682157C25";

fn jwks() -> Value {
    let n: Vec<u8> = (0..MODULUS_HEX.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&MODULUS_HEX[i..i + 2], 16).unwrap())
        .collect();
    let n = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(n);
    json!({"keys": [{"kty": "RSA", "kid": KID, "alg": "RS256", "use": "sig", "n": n, "e": "AQAB"}]})
}

fn token(claims: Value) -> String {
    let mut h = Header::new(Algorithm::RS256);
    h.kid = Some(KID.into());
    jsonwebtoken::encode(&h, &claims, &EncodingKey::from_rsa_pem(KEY_PEM).unwrap()).unwrap()
}

fn claims(email: &str, aud: &str, exp_in: i64) -> Value {
    let now = chrono::Utc::now().timestamp();
    json!({"iss": format!("https://{TEAM}"), "aud": [aud], "email": email, "sub": "u1",
           "iat": now, "exp": now + exp_in})
}

async fn jwks_server() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().route("/certs", get(|| async { Json(jwks()) }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/certs")
}

async fn me(hub: &TestHub, jwt: Option<&str>, extra: Option<(&str, &str)>) -> (u16, Value) {
    let mut r = hub.http.get(format!("{}/api/me", hub.base));
    if let Some(t) = jwt {
        r = r.header("Cf-Access-Jwt-Assertion", t);
    }
    if let Some((k, v)) = extra {
        r = r.header(k, v);
    }
    let r = r.send().await.unwrap();
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn access_jwt_is_verified() {
    let auth = AuthConfig::Access {
        team_domain: TEAM.into(),
        aud: AUD.into(),
        jwks_url: Some(jwks_server().await),
    };
    let hub = TestHub::start_with(auth, String::new()).await;

    let good = token(claims("Alice@Example.com", AUD, 600));
    let (s, v) = me(&hub, Some(&good), None).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["email"], "alice@example.com");
    assert!(v["personal_project_id"].is_string());
    // Same user on the next login, same personal project.
    let (_, v2) = me(&hub, Some(&good), None).await;
    assert_eq!(v2["id"], v["id"]);
    assert_eq!(v2["personal_project_id"], v["personal_project_id"]);

    assert_eq!(me(&hub, None, None).await.0, 401);
    // Nor the same token as a bearer token: in Access mode only the Access header counts.
    let bearer = format!("Bearer {good}");
    assert_eq!(
        me(&hub, None, Some(("Authorization", &bearer))).await.0,
        401
    );
    // An email header without a JWT is not accepted (SPEC §6).
    assert_eq!(
        me(
            &hub,
            None,
            Some(("Cf-Access-Authenticated-User-Email", "alice@example.com"))
        )
        .await
        .0,
        401
    );
    assert_eq!(me(&hub, Some("not.a.jwt"), None).await.0, 401);
    assert_eq!(
        me(&hub, Some(&token(claims("a@b.c", "other-aud", 600))), None)
            .await
            .0,
        401
    );
    assert_eq!(
        me(&hub, Some(&token(claims("a@b.c", AUD, -600))), None)
            .await
            .0,
        401
    );
    let mut wrong_iss = claims("a@b.c", AUD, 600);
    wrong_iss["iss"] = json!("https://evil.cloudflareaccess.com");
    assert_eq!(me(&hub, Some(&token(wrong_iss)), None).await.0, 401);
    let mut tampered = good.clone();
    tampered.insert(good.rfind('.').unwrap() + 2, 'x');
    assert_eq!(me(&hub, Some(&tampered), None).await.0, 401);

    assert!(FakeClient::connect_with(&hub, None).await.is_err());
    let mut c = FakeClient::connect_with(&hub, Some(&good)).await.unwrap();
    c.send(json!({"type": "ping"})).await;
    assert_eq!(c.recv().await["type"], "pong");

    // /worker/* does not accept Access tokens.
    assert!(FakeWorker::connect(&hub, &good).await.is_err());
    hub.finish().await;
}

const ISSUER: &str = "https://auth.example.com/realms/home";

/// Keycloak's shape of an ID token: `aud` is the client id as a string.
fn oidc_claims(iss: &str, aud: &str) -> Value {
    let mut c = claims("bob@example.com", aud, 600);
    c["iss"] = json!(iss);
    c["aud"] = json!(aud);
    c["email_verified"] = json!(true);
    c
}

async fn oidc_hub() -> TestHub {
    let auth = AuthConfig::Oidc {
        issuer: ISSUER.into(),
        aud: AUD.into(),
        jwks_url: jwks_server().await,
        allowed_emails: vec!["bob@example.com".into(), "alice@example.com".into()],
    };
    TestHub::start_with(auth, String::new()).await
}

/// `GET /api/me` with the ID token of `claims` as the bearer token.
async fn me_oidc(hub: &TestHub, claims: Value) -> (u16, Value) {
    let h = format!("Bearer {}", token(claims));
    me(hub, None, Some(("Authorization", &h))).await
}

/// `oidc_claims` for `email` and `sub`.
fn oidc_claims_of(email: &str, sub: &str) -> Value {
    let mut c = oidc_claims(ISSUER, AUD);
    c["email"] = json!(email);
    c["sub"] = json!(sub);
    c
}

async fn oidc_sub_of(hub: &TestHub, email: &str) -> Option<String> {
    sqlx::query_scalar("SELECT oidc_sub FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(&hub.app.db)
        .await
        .unwrap()
}

/// `HQC_OIDC_*` behind oauth2-proxy: the ID token comes in `Authorization: Bearer`, the
/// Access header is not read.
#[tokio::test]
async fn oidc_bearer_token_is_verified() {
    let hub = oidc_hub().await;
    let good = token(oidc_claims(ISSUER, AUD));
    let bearer = format!("Bearer {good}");
    let (s, v) = me(&hub, None, Some(("Authorization", &bearer))).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["email"], "bob@example.com");

    assert_eq!(me(&hub, None, None).await.0, 401);
    assert_eq!(me(&hub, Some(&good), None).await.0, 401);
    assert_eq!(me(&hub, None, Some(("Authorization", &good))).await.0, 401);
    let mut expired = oidc_claims(ISSUER, AUD);
    expired["exp"] = json!(chrono::Utc::now().timestamp() - 600);
    let mut hs256 = Header::new(Algorithm::HS256);
    hs256.kid = Some(KID.into());
    let b64 = |v: Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
    let rsa = EncodingKey::from_rsa_pem(KEY_PEM).unwrap();
    let mut accepted = Vec::new();
    for (why, bad) in [
        (
            "another issuer",
            token(oidc_claims("https://auth.example.com/realms/other", AUD)),
        ),
        ("another client", token(oidc_claims(ISSUER, "other-client"))),
        ("expired", token(expired)),
        (
            "HS256",
            jsonwebtoken::encode(
                &hs256,
                &oidc_claims(ISSUER, AUD),
                &EncodingKey::from_secret(b"secret"),
            )
            .unwrap(),
        ),
        (
            "alg none",
            format!(
                "{}.{}.",
                b64(json!({"alg": "none", "typ": "JWT", "kid": KID})),
                b64(oidc_claims(ISSUER, AUD))
            ),
        ),
        (
            "no kid",
            jsonwebtoken::encode(
                &Header::new(Algorithm::RS256),
                &oidc_claims(ISSUER, AUD),
                &rsa,
            )
            .unwrap(),
        ),
    ] {
        let h = format!("Bearer {bad}");
        let (s, v) = me(&hub, None, Some(("Authorization", &h))).await;
        if s != 401 {
            accepted.push(format!("{why}: {s}: {v}"));
        }
    }
    assert!(accepted.is_empty(), "{accepted:#?}");

    let ws = |authorization: Option<String>| {
        let mut req = format!("{}/api/ws", hub.ws_base)
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("Origin", hub.base.parse().unwrap());
        if let Some(a) = authorization {
            req.headers_mut()
                .insert("Authorization", a.parse().unwrap());
        }
        tokio_tungstenite::connect_async(req)
    };
    assert!(ws(None).await.is_err());
    assert!(FakeClient::connect_with(&hub, Some(&good)).await.is_err());
    let (mut c, _) = ws(Some(bearer)).await.unwrap();
    c.send(Message::Text(json!({"type": "ping"}).to_string().into()))
        .await
        .unwrap();
    let pong = loop {
        if let Message::Text(t) = c.next().await.unwrap().unwrap() {
            break serde_json::from_str::<Value>(t.as_str()).unwrap();
        }
    };
    assert_eq!(pong["type"], "pong");

    // /worker/* reads the same header but does not accept OIDC tokens.
    assert!(FakeWorker::connect(&hub, &good).await.is_err());
    hub.finish().await;
}

/// The email is the hub user, and through it their workers: in OIDC mode only an email the
/// provider verified (Keycloak lets every account edit its own), and only from an ID token.
#[tokio::test]
async fn oidc_token_needs_a_verified_email_and_the_id_type() {
    let hub = oidc_hub().await;
    let mut unverified = oidc_claims(ISSUER, AUD);
    unverified["email_verified"] = json!(false);
    let mut no_claim = oidc_claims(ISSUER, AUD);
    no_claim.as_object_mut().unwrap().remove("email_verified");
    let mut id = oidc_claims(ISSUER, AUD);
    id["typ"] = json!("ID");
    // Keycloak's access token for the same client.
    let mut access = oidc_claims(ISSUER, AUD);
    access["typ"] = json!("Bearer");
    let mut wrong = Vec::new();
    for (why, claims, want) in [
        ("email_verified false", unverified, 403),
        ("email_verified missing", no_claim, 403),
        ("typ ID", id, 200),
        ("typ Bearer", access, 401),
    ] {
        let h = format!("Bearer {}", token(claims));
        let (s, v) = me(&hub, None, Some(("Authorization", &h))).await;
        if s != want {
            wrong.push(format!("{why}: {s}, not {want}: {v}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    hub.finish().await;
}

/// Any account of the realm may have an ID token for the client: only the emails of
/// `HQC_OIDC_ALLOWED_EMAILS` (any case) get in, the others get the answer of a bad token and no
/// hub user.
#[tokio::test]
async fn oidc_email_outside_the_allow_list_is_refused() {
    let hub = oidc_hub().await;
    let bad_token = me_oidc(
        &hub,
        oidc_claims("https://auth.example.com/realms/other", AUD),
    )
    .await;
    assert_eq!(bad_token.0, 401, "{}", bad_token.1);
    let eve = me_oidc(&hub, oidc_claims_of("eve@example.com", "u9")).await;
    assert_eq!(eve, bad_token);
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE email = $1")
        .bind("eve@example.com")
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(users, 0);
    let (s, v) = me_oidc(&hub, oidc_claims_of("Alice@Example.COM", "u2")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["email"], "alice@example.com");
    hub.finish().await;
}

/// An OIDC token without a `sub`, or with an empty one, gets the answer of a bad token and no
/// hub user: there is no subject to bind the user to.
#[tokio::test]
async fn oidc_token_without_a_subject_is_refused() {
    let hub = oidc_hub().await;
    let bad_token = me_oidc(
        &hub,
        oidc_claims("https://auth.example.com/realms/other", AUD),
    )
    .await;
    assert_eq!(bad_token.0, 401, "{}", bad_token.1);
    let mut none = oidc_claims_of("alice@example.com", "u2");
    none.as_object_mut().unwrap().remove("sub");
    assert_eq!(me_oidc(&hub, none).await, bad_token);
    let empty = oidc_claims_of("alice@example.com", "");
    assert_eq!(me_oidc(&hub, empty).await, bad_token);
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE email = $1")
        .bind("alice@example.com")
        .fetch_one(&hub.app.db)
        .await
        .unwrap();
    assert_eq!(users, 0);
    hub.finish().await;
}

/// The first OIDC sign-in binds the hub user to the token's `sub`, a user of an earlier
/// sign-in without one (Access) included; the user of that `sub` stays the same when the email
/// changes at the provider.
#[tokio::test]
async fn oidc_sign_in_binds_the_subject() {
    let hub = oidc_hub().await;
    let bob = hqc_hub::db::ensure_user(&hub.app.db, "bob@example.com")
        .await
        .unwrap();
    assert_eq!(oidc_sub_of(&hub, "bob@example.com").await, None);
    let (s, v) = me_oidc(&hub, oidc_claims_of("bob@example.com", "u1")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["id"], bob.id.to_string());
    assert_eq!(
        oidc_sub_of(&hub, "bob@example.com").await.as_deref(),
        Some("u1")
    );

    let (s, v) = me_oidc(&hub, oidc_claims_of("alice@example.com", "u2")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        oidc_sub_of(&hub, "alice@example.com").await.as_deref(),
        Some("u2")
    );

    // Bob's account at the provider now has another allowed email: still Bob's hub user.
    let (s, v) = me_oidc(&hub, oidc_claims_of("alice@example.com", "u1")).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["id"], bob.id.to_string());
    hub.finish().await;
}

/// The hub user of a verified email is not taken over by another account of the provider
/// with that email once the user is bound to a `sub`.
#[tokio::test]
async fn oidc_email_of_another_subject_is_refused() {
    let hub = oidc_hub().await;
    let (s, v) = me_oidc(&hub, oidc_claims_of("bob@example.com", "u1")).await;
    assert_eq!(s, 200, "{v}");
    let bad_token = me_oidc(
        &hub,
        oidc_claims("https://auth.example.com/realms/other", AUD),
    )
    .await;
    let other = me_oidc(&hub, oidc_claims_of("bob@example.com", "u2")).await;
    assert_eq!(other, bad_token);
    assert_eq!(
        oidc_sub_of(&hub, "bob@example.com").await.as_deref(),
        Some("u1")
    );
    hub.finish().await;
}

/// `nbf` is checked (with the 60 s leeway of `exp`).
#[tokio::test]
async fn oidc_token_before_its_nbf_is_refused() {
    let hub = oidc_hub().await;
    let now = chrono::Utc::now().timestamp();
    let mut early = oidc_claims(ISSUER, AUD);
    early["nbf"] = json!(now + 600);
    assert_eq!(me_oidc(&hub, early).await.0, 401);
    let mut valid = oidc_claims(ISSUER, AUD);
    valid["nbf"] = json!(now - 10);
    let (s, v) = me_oidc(&hub, valid).await;
    assert_eq!(s, 200, "{v}");
    hub.finish().await;
}

/// An `aud` array counts only with `azp` equal to the client id (OIDC Core 3.1.3.7).
#[tokio::test]
async fn oidc_array_aud_needs_azp_of_the_client() {
    let hub = oidc_hub().await;
    let with_azp = |azp: Option<&str>| {
        let mut c = oidc_claims(ISSUER, AUD);
        c["aud"] = json!([AUD, "other-client"]);
        if let Some(azp) = azp {
            c["azp"] = json!(azp);
        }
        c
    };
    let mut wrong = Vec::new();
    for (why, claims, want) in [
        ("no azp", with_azp(None), 401),
        ("azp of another client", with_azp(Some("other-client")), 401),
        ("azp of the client", with_azp(Some(AUD)), 200),
    ] {
        let (s, v) = me_oidc(&hub, claims).await;
        if s != want {
            wrong.push(format!("{why}: {s}, not {want}: {v}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
    hub.finish().await;
}

#[tokio::test]
async fn enrollment_and_worker_token() {
    let hub = TestHub::start().await;
    let (s, e) = hub
        .post("/worker/enroll", json!({"name": "home-mac", "os": "macos"}))
        .await;
    assert_eq!(s, 200);
    let code = e["code"].as_str().unwrap().to_string();
    assert_eq!(code.len(), 9);

    let (s, _) = hub
        .post("/api/workers/enroll/confirm", json!({"code": "AAAA-AAAA"}))
        .await;
    assert_eq!(s, 404);
    // Codes are typed by hand: case and dash do not matter.
    let typed = code.to_lowercase().replace('-', "");
    let (s, c) = hub
        .post(
            "/api/workers/enroll/confirm",
            json!({"code": typed, "name": "Mac mini"}),
        )
        .await;
    assert_eq!(s, 200, "{c}");
    assert_eq!(c["name"], "Mac mini");
    let (s, _) = hub
        .post("/api/workers/enroll/confirm", json!({"code": code}))
        .await;
    assert_eq!(s, 404, "a code is confirmed once");

    let (_, p) = hub
        .post(
            "/worker/enroll/poll",
            json!({"poll_token": e["poll_token"]}),
        )
        .await;
    assert_eq!(p["status"], "confirmed");
    let token = p["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("hqcw_"));
    let (_, p) = hub
        .post(
            "/worker/enroll/poll",
            json!({"poll_token": e["poll_token"]}),
        )
        .await;
    assert_eq!(p["status"], "expired", "the token is handed out once");

    // Only the hash is stored.
    let stored: Option<String> =
        sqlx::query_scalar("SELECT token_hash FROM workers WHERE id = $1::uuid")
            .bind(c["worker_id"].as_str().unwrap())
            .fetch_one(&hub.app.db)
            .await
            .unwrap();
    assert_eq!(
        stored.as_deref(),
        Some(hqc_hub::auth::hash_secret(&token).as_str())
    );

    assert!(FakeWorker::connect(&hub, "hqcw_wrong").await.is_err());

    // Strict frames: hello first, protocol version checked.
    let mut w = FakeWorker::connect(&hub, &token).await.unwrap();
    let id = w.send("session.state", json!({})).await;
    let f = w.expect("error").await;
    assert_eq!(
        (f["id"].as_str(), f["payload"]["code"].as_str()),
        (Some(id.as_str()), Some("invalid_frame"))
    );
    w.send(
        "hello",
        json!({"protocol_version": 99, "capabilities": {}, "environments": [], "env_bindings": [],
               "allowed_roots": [], "allow_foreign_sessions": false, "share_local_models": false,
               "local_models": []}),
    )
    .await;
    let f = w.expect("error").await;
    assert_eq!(f["payload"]["code"], "protocol_version_unsupported");
    assert_eq!(
        f["payload"]["details"]["min_version"],
        hqc_proto::PROTOCOL_VERSION
    );
    w.expect_closed().await;

    let (mut w, _) = FakeWorker::online(&hub, &token, "/tmp/ws").await;
    w.send_raw(json!({"id": "x-1", "type": "approval.cancel", "payload": {}}))
        .await;
    let f = w.expect("error").await;
    assert_eq!(
        (f["id"].as_str(), f["payload"]["code"].as_str()),
        (Some("x-1"), Some("invalid_frame"))
    );

    let (s, list) = hub.get("/api/workers").await;
    assert_eq!(s, 200);
    let me = &list["workers"][0];
    assert_eq!(
        (me["status"].as_str(), me["name"].as_str()),
        (Some("online"), Some("Mac mini"))
    );
    assert_eq!(me["allowed_roots"], json!(["/tmp/ws"]));

    // Revocation drops the connection and the token.
    assert_eq!(
        hub.delete(&format!(
            "/api/workers/{}",
            c["worker_id"].as_str().unwrap()
        ))
        .await,
        204
    );
    w.expect_closed().await;
    assert!(FakeWorker::connect(&hub, &token).await.is_err());
    hub.finish().await;
}

#[tokio::test]
async fn enroll_is_rate_limited() {
    let hub = TestHub::start().await;
    let mut limited = false;
    for _ in 0..12 {
        let (s, _) = hub.post("/worker/enroll", json!({"name": "x"})).await;
        if s == 429 {
            limited = true;
            break;
        }
        assert_eq!(s, 200);
    }
    assert!(limited);
    hub.finish().await;
}

/// `/worker/*` is outside Cloudflare Access by design (SPEC §20). No path under that prefix
/// may reach `/api/*` data without a JWT, however it is spelled: dot segments, encoded
/// slashes, mixed case. The guarantee is the JWT check on `/api/*`, not router details, and
/// this test keeps it that way across axum versions.
#[tokio::test]
async fn worker_prefix_never_reaches_api_without_jwt() {
    let auth = AuthConfig::Access {
        team_domain: TEAM.into(),
        aud: AUD.into(),
        jwks_url: Some(jwks_server().await),
    };
    let hub = TestHub::start_with(auth, String::new()).await;
    for path in [
        "/worker/../api/me",
        "/worker/..%2fapi/me",
        "/worker/..%2Fapi/me",
        "/worker/%2e%2e/api/me",
        "/worker/%2E%2E%2Fapi/me",
        "/worker/./../api/me",
        "/worker//../api/me",
        "/worker/enroll/../../api/me",
        "/WORKER/../api/me",
        "/worker%2f..%2fapi%2fme",
        "/api/me%00",
        "/api//me",
        "/./api/me",
    ] {
        let r = hub
            .http
            .get(format!("{}{path}", hub.base))
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        let body = r.text().await.unwrap_or_default();
        assert!(
            matches!(status, 401 | 403 | 404 | 405 | 400),
            "{path}: HTTP {status} {body}"
        );
        assert!(
            !body.contains("\"email\"") && !body.contains("personal_project_id"),
            "{path}: leaked user data: {body}"
        );
    }
    hub.finish().await;
}
