//! The native iOS client's hub side (SPEC 0.258–0.261): its sign-in through the system browser
//! and Apple push next to Web Push.

mod common;

use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use common::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const WS_PATH: &str = "/Users/home-mac/work/hqc";

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A GET that does not follow the redirect: the `Location` and the status.
async fn location(hub: &TestHub, path: &str) -> (u16, Option<String>) {
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let r = http
        .get(format!("{}{path}", hub.base))
        .send()
        .await
        .unwrap();
    let loc = r
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    (r.status().as_u16(), loc)
}

/// SPEC §6: `/api/native/login` redirects to the app with a one-time code, never the token;
/// the code is exchanged once, only with its verifier, outside Access at `/worker/native/token`.
#[tokio::test]
async fn native_sign_in_exchanges_a_code_once_with_its_verifier() {
    let hub = TestHub::start().await;
    let verifier = "v".repeat(64);
    let path = format!(
        "/api/native/login?challenge={}&state=state-1234",
        challenge(&verifier)
    );
    let (s, loc) = location(&hub, &path).await;
    assert_eq!(s, 302);
    let loc = loc.unwrap();
    assert!(loc.starts_with("hqc-app://signed-in?code="), "{loc}");
    assert!(loc.ends_with("&state=state-1234"), "{loc}");
    let code = loc
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();

    // A wrong verifier burns the code.
    let (s, v) = hub
        .post(
            "/worker/native/token",
            json!({"code": code, "verifier": "w".repeat(64)}),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("bad_code")));
    let (s, _) = hub
        .post(
            "/worker/native/token",
            json!({"code": code, "verifier": verifier}),
        )
        .await;
    assert_eq!(s, 400);

    // A fresh code with the right verifier gives the token (empty in dev mode), once.
    let (_, loc) = location(&hub, &path).await;
    let code = loc
        .unwrap()
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let (s, v) = hub
        .post(
            "/worker/native/token",
            json!({"code": code, "verifier": verifier}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["token"], "");
    let (s, _) = hub
        .post(
            "/worker/native/token",
            json!({"code": code, "verifier": verifier}),
        )
        .await;
    assert_eq!(s, 400);

    // Only a challenge and a state the redirect can carry as they are.
    let (s, _) = location(&hub, "/api/native/login?challenge=x&state=state-1234").await;
    assert_eq!(s, 400);
    let (s, _) = location(
        &hub,
        &format!(
            "/api/native/login?challenge={}&state=a%26evil%3Dyes",
            challenge(&verifier)
        ),
    )
    .await;
    assert_eq!(s, 400);
    hub.finish().await;
}

/// SPEC §16: a registered iOS device gets the notice a Web Push gets, over APNs, with what the
/// app needs to answer an approval from the notification.
#[tokio::test]
async fn apns_device_gets_the_approval() {
    let hub = TestHub::start().await;
    let (s, v) = hub.get("/api/push/apns").await;
    assert_eq!((s, v["enabled"].as_bool()), (200, Some(true)));
    let device = "ab".repeat(32);
    let (s, _) = hub
        .post(
            "/api/push/apns",
            json!({"token": "nothex", "sandbox": true}),
        )
        .await;
    assert_eq!(s, 400);
    let (s, _) = hub
        .post(
            "/api/push/apns",
            json!({"token": device.to_uppercase(), "sandbox": true}),
        )
        .await;
    assert_eq!(s, 201);

    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("apns").await;
    ack_start(&mut w, &sid).await;
    w.event(agent_event(
        &sid,
        "approval.request",
        json!({
            "request_id": "req-1",
            "tool": "Bash",
            "description": "cargo test",
            "options": [
                {"id": "allow", "label": "Allow once", "canonical": "once"},
                {"id": "deny", "label": "Deny", "canonical": "deny"}
            ],
            "expires_at": "2099-01-01T00:00:00Z",
        }),
    ))
    .await;
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if !hub.apns.sent.lock().unwrap().is_empty() {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no APNs push");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (tok, sandbox, collapse, p): (String, bool, String, Value) =
        hub.apns.sent.lock().unwrap()[0].clone();
    assert_eq!(tok, device);
    assert!(sandbox);
    assert_eq!(collapse, format!("{sid}:req-1"));
    assert_eq!(p["aps"]["category"], "APPROVAL");
    assert_eq!(p["session_id"], sid.as_str());
    assert_eq!(p["request_id"], "req-1");
    assert_eq!(p["options"][0]["id"], "allow");
    assert!(p["aps"]["alert"]["body"]
        .as_str()
        .unwrap()
        .contains("cargo test"));

    // Unregistered: no more APNs pushes.
    let r = hub
        .http
        .delete(format!("{}/api/push/apns", hub.base))
        .json(&json!({ "token": device }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 204);
    let (s, v) = hub.get("/api/push/apns").await;
    assert_eq!((s, v["enabled"].as_bool()), (200, Some(true)));
    hub.finish().await;
}

/// SPEC 0.260: APNs goes out whoever else shows the session (a web tab left open must not
/// keep the phone quiet), and is held back only while the iOS app itself shows it.
#[tokio::test]
async fn apns_skips_only_while_the_ios_app_watches() {
    let hub = TestHub::start().await;
    let (s, _) = hub
        .post(
            "/api/push/apns",
            json!({"token": "ef".repeat(32), "sandbox": false}),
        )
        .await;
    assert_eq!(s, 201);
    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("watched").await;
    ack_start(&mut w, &sid).await;
    let ask = |n: u32| {
        agent_event(
            &sid,
            "approval.request",
            json!({
                "request_id": format!("req-{n}"),
                "tool": "Bash",
                "description": "cargo test",
                "options": [{"id": "allow", "label": "Allow once", "canonical": "once"}],
                "expires_at": "2099-01-01T00:00:00Z",
            }),
        )
    };
    let sent = |hub: &TestHub| hub.apns.sent.lock().unwrap().len();

    // A web client shows the session: the phone is still told.
    let mut web = FakeClient::connect(&hub).await;
    web.subscribe(&sid, 0).await;
    w.event(ask(1)).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while sent(&hub) < 1 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no APNs push while only the web watches"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The iOS app shows it too: nothing more over APNs.
    let mut ios = FakeClient::connect_ios(&hub).await;
    ios.subscribe(&sid, 0).await;
    w.event(ask(2)).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        sent(&hub),
        1,
        "APNs went out while the iOS app showed the session"
    );

    // The app leaves it (backgrounded): the phone is told again.
    ios.send(json!({"type": "unsubscribe", "session_id": sid}))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    w.event(ask(3)).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while sent(&hub) < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no APNs push after the app left"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    hub.finish().await;
}

/// SPEC 0.259: a device that registered a key gets its notices sealed: Apple and a relay see
/// only a generic alert; the key opens the content, request and options included.
#[tokio::test]
async fn apns_notice_is_sealed_with_the_device_key() {
    use aes_gcm::aead::{Aead, KeyInit};
    use base64::engine::general_purpose::STANDARD;
    let hub = TestHub::start().await;
    let device = "cd".repeat(32);
    let key = [42u8; 32];
    let (s, _) = hub
        .post(
            "/api/push/apns",
            json!({"token": device, "sandbox": false, "key": STANDARD.encode([1u8; 16])}),
        )
        .await;
    assert_eq!(s, 400, "a key must be 32 bytes");
    let (s, _) = hub
        .post(
            "/api/push/apns",
            json!({"token": device, "sandbox": false, "key": STANDARD.encode(key)}),
        )
        .await;
    assert_eq!(s, 201);

    let (_, token) = hub.enroll("home-mac").await;
    let (mut w, _) = FakeWorker::online(&hub, &token, WS_PATH).await;
    let sid = hub.create_session("sealed").await;
    ack_start(&mut w, &sid).await;
    w.event(agent_event(
        &sid,
        "approval.request",
        json!({
            "request_id": "req-9",
            "tool": "Bash",
            "description": "deploy to prod",
            "options": [{"id": "allow", "label": "Allow once", "canonical": "once"}],
            "expires_at": "2099-01-01T00:00:00Z",
        }),
    ))
    .await;
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        if !hub.apns.sent.lock().unwrap().is_empty() {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no APNs push");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (_, _, _, p): (String, bool, String, Value) = hub.apns.sent.lock().unwrap()[0].clone();
    let text = p.to_string();
    for leak in [
        "deploy to prod",
        "req-9",
        sid.as_str(),
        "Allow once",
        "APPROVAL",
    ] {
        assert!(!text.contains(leak), "{leak} is readable on the way");
    }
    assert_eq!(p["aps"]["mutable-content"], 1);
    let boxed = STANDARD.decode(p["enc"].as_str().unwrap()).unwrap();
    let (nonce, ct) = boxed.split_at(12);
    let inner = aes_gcm::Aes256Gcm::new_from_slice(&key)
        .unwrap()
        .decrypt(nonce.into(), ct)
        .unwrap();
    let inner: Value = serde_json::from_slice(&inner).unwrap();
    assert_eq!(inner["category"], "APPROVAL");
    assert_eq!(inner["session_id"], sid.as_str());
    assert_eq!(inner["request_id"], "req-9");
    assert_eq!(inner["options"][0]["id"], "allow");
    assert!(inner["body"].as_str().unwrap().contains("deploy to prod"));
    hub.finish().await;
}

/// SPEC §16: a hub with the key and `HQC_APNS_RELAY=1` sends other hubs' notices; a hub with
/// `HQC_APNS_RELAY_URL` sends through it, and hears when a device is gone.
#[tokio::test]
async fn apns_relay_sends_for_a_hub_without_a_key() {
    let relay = TestHub::start_config(|c| c.apns_relay_serve = true).await;
    let device = "cd".repeat(32);
    let notice = json!({"aps": {"alert": {"title": "t", "body": "b"}}, "session_id": "s"});

    // What a hub without a key sends, through `RelayClient`.
    let client = hqc_hub::apns::RelayClient::new(&relay.base).unwrap();
    use hqc_hub::apns::ApnsSender;
    client
        .send(
            &device,
            true,
            "tag-1",
            &serde_json::to_vec(&notice).unwrap(),
        )
        .await
        .unwrap();
    let sent = relay.apns.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, device);
    assert!(sent[0].1);
    assert_eq!(sent[0].2, "tag-1");
    assert_eq!(sent[0].3["aps"]["alert"]["body"], "b");

    // Only an alert notice, to a real token.
    let (s, _) = relay
        .post(
            "/worker/push/relay",
            json!({"token": device, "payload": {"x": 1}}),
        )
        .await;
    assert_eq!(s, 400);
    let (s, _) = relay
        .post(
            "/worker/push/relay",
            json!({"token": "zz", "payload": notice}),
        )
        .await;
    assert_eq!(s, 400);
    relay.finish().await;

    // A hub that does not relay says so.
    let plain = TestHub::start().await;
    let (s, v) = plain
        .post(
            "/worker/push/relay",
            json!({"token": device, "payload": notice}),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("no_relay")));
    plain.finish().await;
}

const TEST_KID: &str = "test-key-1";
const TEST_KEY_PEM: &[u8] = include_bytes!("fixtures/test-access-key.pem");
const TEST_MODULUS_HEX: &str = "94E0DC3E10FFA7CD4E0A1084A6BE2C63A9E65C492A50BB083D2F012946EAE3671FF27B328F89DABD24B0692064BB97515010967EAA54A58764C3150380F47590A5395E44E83FAE4DE06C8431744CA5B9595E4381862FF1FB23186BF9396363099CB7FFEF39D486A5778DAFA977C9C1D3A4D1298C2B5ADBEA1AF134451E7C3B9477D2423FF9C46C235D63AE3FA0E4D4F6BDB41226021EE5A4507ED6B036545A07EADF8BD77163DAFEE87D84A63AB349396B52E0BA2486F2575DCD03AAB9BB531DEC1285CBA622C3F6CE5F6BCC3C5FD14640C7E1443FAB21ECD2FE342602C0F554CCE175B669C2458DD7442B799DDFF653B53D35393E21971F4C1EE56682157C25";

async fn jwks_server() -> String {
    let n: Vec<u8> = (0..TEST_MODULUS_HEX.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&TEST_MODULUS_HEX[i..i + 2], 16).unwrap())
        .collect();
    let n = URL_SAFE_NO_PAD.encode(n);
    let body = json!({"keys": [{"kty": "RSA", "kid": TEST_KID, "alg": "RS256", "use": "sig", "n": n, "e": "AQAB"}]});
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = axum::Router::new().route(
        "/certs",
        axum::routing::get(move || {
            let body = body.clone();
            async move { axum::Json(body) }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/certs")
}

fn sign_rs256(claims: Value) -> String {
    let mut h = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    h.kid = Some(TEST_KID.into());
    jsonwebtoken::encode(
        &h,
        &claims,
        &jsonwebtoken::EncodingKey::from_rsa_pem(TEST_KEY_PEM).unwrap(),
    )
    .unwrap()
}

fn extract_code(loc: &str) -> String {
    loc.split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string()
}

/// SPEC 0.261: native sign-in works behind an OIDC reverse proxy (Keycloak + oauth2-proxy,
/// RUNBOOK §1.9), returning the proxy's cookies (or bearer token when no cookies were sent).
#[tokio::test]
async fn native_sign_in_works_in_oidc_and_access_modes() {
    let jwks = jwks_server().await;
    let issuer = "https://auth.example.com/realms/home";
    let aud = "hqc";
    let oidc_hub = TestHub::start_with(
        hqc_hub::config::AuthConfig::Oidc {
            issuer: issuer.into(),
            aud: aud.into(),
            jwks_url: jwks.clone(),
            allowed_emails: vec!["bob@example.com".into()],
        },
        String::new(),
    )
    .await;

    let now = chrono::Utc::now().timestamp();
    let id_token = sign_rs256(json!({
        "iss": issuer,
        "aud": aud,
        "email": "bob@example.com",
        "email_verified": true,
        "sub": "sub-bob",
        "typ": "ID",
        "iat": now,
        "exp": now + 600,
    }));
    let verifier = "o".repeat(64);
    let path = format!(
        "/api/native/login?challenge={}&state=oidc-state-1",
        challenge(&verifier)
    );
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // Unauthenticated login attempt is rejected.
    let r = http
        .get(format!("{}{path}", oidc_hub.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);

    // Authenticated via OIDC proxy with split cookies: returns the joined Cookie header.
    let r = http
        .get(format!("{}{path}", oidc_hub.base))
        .header("Authorization", format!("Bearer {id_token}"))
        .header("Cookie", "_oauth2_proxy_0=part0==; _oauth2_proxy_1=part1==")
        .header("Cookie", "extra=1")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 302);
    let loc = r
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(loc.starts_with("hqc-app://signed-in?code="), "{loc}");
    assert!(loc.ends_with("&state=oidc-state-1"), "{loc}");
    let code = extract_code(&loc);
    let (s, v) = oidc_hub
        .post(
            "/worker/native/token",
            json!({"code": code, "verifier": verifier}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        v["cookie"].as_str(),
        Some("_oauth2_proxy_0=part0==; _oauth2_proxy_1=part1==; extra=1")
    );
    assert!(v.get("token").is_none());
    // Single-use.
    let (s, _) = oidc_hub
        .post(
            "/worker/native/token",
            json!({"code": code, "verifier": verifier}),
        )
        .await;
    assert_eq!(s, 400);

    // OIDC without cookies falls back to returning the verified bearer token.
    let r = http
        .get(format!("{}{path}", oidc_hub.base))
        .header("Authorization", format!("Bearer {id_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 302);
    let code = extract_code(r.headers().get("location").unwrap().to_str().unwrap());
    let (s, v) = oidc_hub
        .post(
            "/worker/native/token",
            json!({"code": code, "verifier": verifier}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["bearer"].as_str(), Some(id_token.as_str()));

    // Over-limit Cookie header is rejected.
    let huge = format!("_oauth2_proxy={}", "x".repeat(65 * 1024));
    let r = http
        .get(format!("{}{path}", oidc_hub.base))
        .header("Authorization", format!("Bearer {id_token}"))
        .header("Cookie", huge)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
    oidc_hub.finish().await;

    // Access mode still returns both `token` (for older builds) and `cookie`.
    let team = "test-team.cloudflareaccess.com";
    let access_hub = TestHub::start_with(
        hqc_hub::config::AuthConfig::Access {
            team_domain: team.into(),
            aud: aud.into(),
            jwks_url: Some(jwks),
        },
        String::new(),
    )
    .await;
    let cf_jwt = sign_rs256(json!({
        "iss": format!("https://{team}"),
        "aud": [aud],
        "email": "alice@example.com",
        "sub": "u1",
        "iat": now,
        "exp": now + 600,
    }));
    let r = http
        .get(format!("{}{path}", access_hub.base))
        .header("Cf-Access-Jwt-Assertion", &cf_jwt)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 302);
    let code = extract_code(r.headers().get("location").unwrap().to_str().unwrap());
    let (s, v) = access_hub
        .post(
            "/worker/native/token",
            json!({"code": code, "verifier": verifier}),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["token"].as_str(), Some(cf_jwt.as_str()));
    assert_eq!(
        v["cookie"].as_str(),
        Some(format!("CF_Authorization={cf_jwt}").as_str())
    );
    access_hub.finish().await;
}
