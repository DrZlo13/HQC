//! T49 (SPEC §3 User, §3 Project, §6, §12): the app theme, the look and the project bar's
//! order on the user, a project's color and icon, and the icon image: PNG, JPEG or WebP by
//! the content, square, 128–1024 px, at most 256 KB, owner only, served with its checked type
//! and a cache key in its URL, gone with the project.

mod common;

use common::*;
use serde_json::{json, Value};

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend(w.to_be_bytes());
    b.extend(h.to_be_bytes());
    b.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
    b
}

/// `PUT /api/projects/{id}/icon` with the field `file`.
async fn put_icon(
    hub: &TestHub,
    pid: &str,
    name: &str,
    mime: &str,
    bytes: Vec<u8>,
) -> (u16, Value) {
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(name.to_string())
        .mime_str(mime)
        .unwrap();
    let r = hub
        .http
        .put(format!("{}/api/projects/{pid}/icon", hub.base))
        .multipart(reqwest::multipart::Form::new().part("file", part))
        .send()
        .await
        .unwrap();
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn the_theme_round_trips_on_the_user() {
    let hub = TestHub::start().await;
    let (s, me) = hub.get("/api/me").await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["theme"], "warm", "the default");

    let (s, me) = hub.patch("/api/me", json!({"theme": "graphite"})).await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["theme"], "graphite");
    let (_, me) = hub.get("/api/me").await;
    assert_eq!(me["theme"], "graphite", "stored, the same on every device");

    // The zone alone leaves the theme.
    let (s, me) = hub.patch("/api/me", json!({"timezone": "UTC"})).await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["theme"], "graphite");

    // A light theme (since 0.88) is stored like a dark one.
    let (s, me) = hub
        .patch("/api/me", json!({"theme": "contrast_light"}))
        .await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["theme"], "contrast_light");
    let (_, me) = hub.get("/api/me").await;
    assert_eq!(me["theme"], "contrast_light");

    let (s, _) = hub.patch("/api/me", json!({"theme": "light"})).await;
    assert_eq!(s, 422, "not a theme");
    let (_, me) = hub.get("/api/me").await;
    assert_eq!(me["theme"], "contrast_light");
    hub.finish().await;
}

/// The client's look (SPEC §3 User): one switch, kept on the user like the theme.
#[tokio::test]
async fn the_look_round_trips_on_the_user() {
    let hub = TestHub::start().await;
    let (s, me) = hub.get("/api/me").await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["look"], "spacious", "the default");

    let (s, me) = hub.patch("/api/me", json!({"look": "compact"})).await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["look"], "compact");
    let (_, me) = hub.get("/api/me").await;
    assert_eq!(me["look"], "compact", "stored, the same on every device");

    // The theme alone leaves the look, and the look the theme.
    let (s, me) = hub.patch("/api/me", json!({"theme": "graphite"})).await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["look"], "compact");
    let (s, me) = hub.patch("/api/me", json!({"look": "spacious"})).await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["look"], "spacious");
    assert_eq!(me["theme"], "graphite");

    for odd in ["dense", "Compact"] {
        let (s, _) = hub.patch("/api/me", json!({"look": odd})).await;
        assert_eq!(s, 422, "not a look: {odd}");
    }
    let (_, me) = hub.get("/api/me").await;
    assert_eq!(me["look"], "spacious", "an unknown look changes nothing");
    hub.finish().await;
}

/// The project bar's order (SPEC §3 User): kept on the user like the look.
#[tokio::test]
async fn the_project_sort_round_trips_on_the_user() {
    let hub = TestHub::start().await;
    let (s, me) = hub.get("/api/me").await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["project_sort"], "custom", "the default");

    let (s, me) = hub.patch("/api/me", json!({"project_sort": "name"})).await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["project_sort"], "name");
    let (_, me) = hub.get("/api/me").await;
    assert_eq!(
        me["project_sort"], "name",
        "stored, the same on every device"
    );

    // The theme and the look alone leave the order, and the order them.
    let (s, me) = hub
        .patch("/api/me", json!({"theme": "graphite", "look": "compact"}))
        .await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["project_sort"], "name");
    let (s, me) = hub
        .patch("/api/me", json!({"project_sort": "recent"}))
        .await;
    assert_eq!(s, 200, "{me}");
    assert_eq!(me["project_sort"], "recent");
    assert_eq!(me["theme"], "graphite");
    assert_eq!(me["look"], "compact");

    for odd in ["activity", "Name"] {
        let (s, _) = hub.patch("/api/me", json!({"project_sort": odd})).await;
        assert_eq!(s, 422, "not an order: {odd}");
    }
    let (_, me) = hub.get("/api/me").await;
    assert_eq!(
        me["project_sort"], "recent",
        "an unknown order changes nothing"
    );
    hub.finish().await;
}

#[tokio::test]
async fn color_and_icon_round_trip_and_the_personal_project_gets_them_too() {
    let hub = TestHub::start().await;
    let (s, p) = hub
        .post("/api/projects", json!({"name": "physics-mp"}))
        .await;
    assert_eq!(s, 201, "{p}");
    let pid = p["id"].as_str().unwrap().to_string();
    // No color stored: one by a hash of the id, the same every time.
    let first = p["color"].as_str().unwrap().to_string();
    let (_, again) = hub.get(&format!("/api/projects/{pid}")).await;
    assert_eq!(again["color"], first.as_str());
    assert!(p.get("icon").is_none(), "no icon: the first letters");

    let (s, p) = hub
        .patch(
            &format!("/api/projects/{pid}"),
            json!({"color": "coral", "icon": {"kind": "emoji", "value": "⚛️"}}),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    assert_eq!(p["color"], "coral");
    assert_eq!(p["icon"], json!({"kind": "emoji", "value": "⚛️"}));
    let (_, list) = hub.get("/api/projects").await;
    let listed = list["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == pid.as_str())
        .unwrap()
        .clone();
    assert_eq!(listed["color"], "coral");
    assert_eq!(listed["icon"]["value"], "⚛️");

    // Letters are kept in upper case; the name and the color stay.
    let (s, p) = hub
        .patch(
            &format!("/api/projects/{pid}"),
            json!({"icon": {"kind": "letters", "value": "pm"}}),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    assert_eq!(p["icon"], json!({"kind": "letters", "value": "PM"}));
    assert_eq!(
        (p["color"].as_str(), p["name"].as_str()),
        (Some("coral"), Some("physics-mp"))
    );

    for bad in [
        json!({"color": "yellow"}),
        json!({"icon": {"kind": "letters", "value": "abc"}}),
        json!({"icon": {"kind": "emoji", "value": ""}}),
        json!({"icon": {"kind": "image"}}),
        json!({"icon": {"kind": "svg", "value": "x"}}),
    ] {
        let (s, v) = hub
            .patch(&format!("/api/projects/{pid}"), bad.clone())
            .await;
        assert_eq!(s, 422, "{bad} → {v}");
    }

    // The personal project cannot be renamed, but its appearance changes.
    let personal = hub.personal_project().await;
    let (s, p) = hub
        .patch(
            &format!("/api/projects/{personal}"),
            json!({"color": "slate", "icon": {"kind": "emoji", "value": "🏡"}}),
        )
        .await;
    assert_eq!(s, 200, "{p}");
    assert_eq!(
        (p["color"].as_str(), p["is_personal"].as_bool()),
        (Some("slate"), Some(true))
    );
    assert_eq!(p["icon"]["value"], "🏡");
    let (s, v) = put_icon(&hub, &personal, "home.png", "image/png", png(256, 256)).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["icon"]["kind"], "image");
    let (s, v) = hub
        .patch(
            &format!("/api/projects/{personal}"),
            json!({"name": "Mine"}),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("personal_project")));
    hub.finish().await;
}

#[tokio::test]
async fn the_icon_image_is_checked_by_its_content() {
    let hub = TestHub::start().await;
    let (_, p) = hub.post("/api/projects", json!({"name": "ESP"})).await;
    let pid = p["id"].as_str().unwrap().to_string();

    // An SVG, also when it says it is a PNG.
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256"/>"#.to_vec();
    for (name, mime) in [("logo.svg", "image/svg+xml"), ("logo.png", "image/png")] {
        let (s, v) = put_icon(&hub, &pid, name, mime, svg.clone()).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (422, Some("invalid_icon")),
            "{name}: {v}"
        );
    }
    // Too large: 256 KB and a byte, with a valid header.
    let mut big = png(512, 512);
    big.resize(256 * 1024 + 1, 0);
    let (s, v) = put_icon(&hub, &pid, "big.png", "image/png", big).await;
    assert_eq!(s, 422, "{v}");
    // Far larger than the body limit: still the same answer, the upload stops early.
    let mut huge = png(512, 512);
    huge.resize(3 << 20, 0);
    let (s, v) = put_icon(&hub, &pid, "huge.png", "image/png", huge).await;
    assert_eq!(s, 422, "{v}");
    // Not square, too small, too large in pixels.
    for (w, h) in [(512, 256), (64, 64), (2048, 2048)] {
        let (s, v) = put_icon(&hub, &pid, "x.png", "image/png", png(w, h)).await;
        assert_eq!(s, 422, "{w}×{h}: {v}");
    }
    let (_, p) = hub.get(&format!("/api/projects/{pid}")).await;
    assert!(p.get("icon").is_none(), "nothing refused was kept: {p}");

    // A PNG named and declared as WebP is fine: the content decides, and is served as PNG.
    let img = png(256, 256);
    let (s, p) = put_icon(&hub, &pid, "logo.webp", "image/webp", img.clone()).await;
    assert_eq!(s, 200, "{p}");
    let url = p["icon"]["url"].as_str().unwrap().to_string();
    assert!(
        url.starts_with(&format!("/api/projects/{pid}/icon?v=")),
        "{url}"
    );
    let r = hub
        .http
        .get(format!("{}{url}", hub.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let h = r.headers().clone();
    assert_eq!(h["content-type"], "image/png");
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert!(h["cache-control"].to_str().unwrap().contains("immutable"));
    let etag = h["etag"].to_str().unwrap().to_string();
    assert_eq!(r.bytes().await.unwrap().to_vec(), img);
    let r = hub
        .http
        .get(format!("{}{url}", hub.base))
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 304);

    // Another image: another URL. Emoji instead: the image is dropped.
    let (_, p2) = put_icon(&hub, &pid, "b.png", "image/png", png(300, 300)).await;
    assert_ne!(p2["icon"]["url"], url.as_str());
    let (s, _) = hub
        .patch(
            &format!("/api/projects/{pid}"),
            json!({"icon": {"kind": "emoji", "value": "🔌"}}),
        )
        .await;
    assert_eq!(s, 200);
    let (s, _) = hub.get(&format!("/api/projects/{pid}/icon")).await;
    assert_eq!(s, 404);
    hub.finish().await;
}

#[tokio::test]
async fn the_icon_is_the_owners_and_goes_with_the_project() {
    let hub = TestHub::start().await;
    let (_, p) = hub.post("/api/projects", json!({"name": "Tests"})).await;
    let pid = p["id"].as_str().unwrap().to_string();
    let (s, _) = put_icon(&hub, &pid, "t.png", "image/png", png(128, 128)).await;
    assert_eq!(s, 200);
    let n = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM project_icons")
            .fetch_one(&hub.app.db)
            .await
            .unwrap()
    };
    assert_eq!(n().await, 1);

    // Another user's project: missing, for reading and for writing.
    let other = hqc_hub::db::ensure_user(&hub.app.db, "other@example.com")
        .await
        .unwrap();
    let theirs = hqc_hub::db::create_project(&hub.app.db, other.id, "theirs", "")
        .await
        .unwrap();
    let (s, _) = put_icon(
        &hub,
        &theirs.id.to_string(),
        "t.png",
        "image/png",
        png(128, 128),
    )
    .await;
    assert_eq!(s, 404);
    let (s, _) = hub.get(&format!("/api/projects/{}/icon", theirs.id)).await;
    assert_eq!(s, 404);
    let (s, _) = hub
        .patch(
            &format!("/api/projects/{}", theirs.id),
            json!({"color": "rose"}),
        )
        .await;
    assert_eq!(s, 404);

    assert_eq!(hub.delete(&format!("/api/projects/{pid}")).await, 204);
    assert_eq!(n().await, 0, "deleted with the project");
    hub.finish().await;
}
