//! Project icons (SPEC §3 Project, §6, §12, T49): an emoji or letters stored in
//! `projects.icon`, or an uploaded image in `project_icons`.
//!
//! - `PUT /api/projects/{id}/icon` (multipart `file`, like an attachment): PNG, JPEG or WebP
//!   by the content's magic bytes (the declared type and the name do not count), square,
//!   128–1024 px, at most [`ICON_MAX`] bytes; anything else `422 invalid_icon`. The image
//!   becomes the project's icon; the reply is the project.
//! - `GET /api/projects/{id}/icon`: the image with its checked type, `nosniff` and a long
//!   private cache: the project object's `url` carries `?v=<sha256 prefix>`, so a new image
//!   is a new URL. Owner only, like the project.
//!
//! The image is not an artifact: it belongs to no session, the artifact cleanup must not
//! remove it, and it goes with the project (`ON DELETE CASCADE`).

use axum::{
    body::Body,
    extract::{Multipart, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use hqc_proto::api::{Project, ProjectIcon};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    api::project_for,
    app::AppState,
    auth::CurrentUser,
    db,
    error::{ApiError, ApiResult},
};

/// Largest icon image.
pub const ICON_MAX: usize = 256 * 1024;
/// Smallest and largest side, in pixels.
pub const ICON_MIN_SIDE: u32 = 128;
pub const ICON_MAX_SIDE: u32 = 1024;
/// The request body of `PUT .../icon`: the image plus the multipart framing. The upload is
/// refused with `422` as soon as the file passes [`ICON_MAX`], before this cuts it.
pub const BODY_LIMIT: usize = ICON_MAX + 64 * 1024;

fn invalid(msg: impl Into<String>) -> ApiError {
    ApiError::Unprocessable("invalid_icon", msg.into())
}

/// `{kind: emoji|letters, value}` as stored, from a `PATCH`: an emoji is 1–8 characters (a
/// ZWJ sequence or a flag is several), at most 32 bytes; letters are 1–2 characters, kept in
/// upper case; neither has spaces or control characters. An image only comes by `PUT`.
pub fn stored_icon(icon: ProjectIcon) -> ApiResult<Value> {
    let clean = |v: &str| {
        let v = v.trim();
        (!v.is_empty() && !v.chars().any(|c| c.is_control() || c.is_whitespace()))
            .then(|| v.to_string())
    };
    match icon {
        ProjectIcon::Emoji { value } => {
            let v = clean(&value)
                .filter(|v| v.len() <= 32 && v.chars().count() <= 8)
                .ok_or_else(|| invalid("icon: an emoji is 1 to 8 characters"))?;
            Ok(json!({"kind": "emoji", "value": v}))
        }
        ProjectIcon::Letters { value } => {
            let v = clean(&value)
                .map(|v| v.to_uppercase())
                .filter(|v| v.chars().count() <= 2)
                .ok_or_else(|| invalid("icon: letters are 1 or 2 characters"))?;
            Ok(json!({"kind": "letters", "value": v}))
        }
        ProjectIcon::Image { .. } => Err(invalid(
            "icon: an image is uploaded with PUT /api/projects/{id}/icon",
        )),
    }
}

/// The type of an icon image by its magic bytes, and its size in pixels from the header.
pub fn sniff(b: &[u8]) -> Option<(&'static str, u32, u32)> {
    let be16 = |i: usize| Some(u16::from_be_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32);
    let be32 = |i: usize| Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?));
    let le16 = |i: usize| Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32);
    let le24 = |i: usize| {
        let s = b.get(i..i + 3)?;
        Some(s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16)
    };
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        if b.get(12..16)? != b"IHDR" {
            return None;
        }
        return Some(("image/png", be32(16)?, be32(20)?));
    }
    if b.starts_with(&[0xFF, 0xD8]) {
        // Segments up to the first frame header (SOF0–SOF15 but DHT, JPG and DAC).
        let mut i = 2;
        loop {
            if *b.get(i)? != 0xFF {
                return None;
            }
            let m = *b.get(i + 1)?;
            match m {
                0xFF => i += 1,
                0x01 | 0xD0..=0xD7 => i += 2,
                0xD9 | 0xDA => return None,
                0xC0..=0xCF if !matches!(m, 0xC4 | 0xC8 | 0xCC) => {
                    return Some(("image/jpeg", be16(i + 7)?, be16(i + 5)?));
                }
                _ => i += 2 + be16(i + 2)? as usize,
            }
        }
    }
    if b.get(0..4)? == b"RIFF" && b.get(8..12)? == b"WEBP" {
        return match b.get(12..16)? {
            b"VP8 " if b.get(23..26)? == [0x9D, 0x01, 0x2A] => {
                Some(("image/webp", le16(26)? & 0x3FFF, le16(28)? & 0x3FFF))
            }
            b"VP8L" if *b.get(20)? == 0x2F => {
                let bits = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                Some((
                    "image/webp",
                    (bits & 0x3FFF) + 1,
                    ((bits >> 14) & 0x3FFF) + 1,
                ))
            }
            b"VP8X" => Some(("image/webp", le24(24)? + 1, le24(27)? + 1)),
            _ => None,
        };
    }
    None
}

/// The checks of an icon image; its type.
pub fn check_image(b: &[u8]) -> ApiResult<&'static str> {
    if b.len() > ICON_MAX {
        return Err(invalid(format!("the image is over {} KB", ICON_MAX / 1024)));
    }
    let (mime, w, h) = sniff(b).ok_or_else(|| invalid("the image is not a PNG, JPEG or WebP"))?;
    if w != h {
        return Err(invalid(format!("the image is {w}×{h} px, not square")));
    }
    if !(ICON_MIN_SIDE..=ICON_MAX_SIDE).contains(&w) {
        return Err(invalid(format!(
            "the image is {w} px; {ICON_MIN_SIDE} to {ICON_MAX_SIDE} px please"
        )));
    }
    Ok(mime)
}

/// `PUT /api/projects/{id}/icon`.
pub async fn put(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    mut mp: Multipart,
) -> ApiResult<Json<Project>> {
    project_for(&app, &user, id).await?;
    let mut data: Option<Vec<u8>> = None;
    let multipart = |e: axum::extract::multipart::MultipartError| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            invalid(format!("the image is over {} KB", ICON_MAX / 1024))
        } else {
            ApiError::BadRequest(format!("multipart: {e}"))
        }
    };
    while let Some(mut field) = mp.next_field().await.map_err(multipart)? {
        if field.name() != Some("file") || data.is_some() {
            continue;
        }
        let mut buf = Vec::new();
        while let Some(c) = field.chunk().await.map_err(multipart)? {
            buf.extend_from_slice(&c);
            if buf.len() > ICON_MAX {
                return Err(invalid(format!("the image is over {} KB", ICON_MAX / 1024)));
            }
        }
        data = Some(buf);
    }
    let data =
        data.ok_or_else(|| ApiError::BadRequest("the multipart field `file` is required".into()))?;
    let mime = check_image(&data)?;
    let sha: String = Sha256::digest(&data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let p = db::set_project_icon_image(&app.db, id, mime, &data, &sha).await?;
    tracing::info!(project_id = %id, mime, size = data.len(), "project icon image set");
    Ok(Json(p.to_api()))
}

/// `GET /api/projects/{id}/icon`.
pub async fn get(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    project_for(&app, &user, id).await?;
    let img = db::project_icon_image(&app.db, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    let etag = format!("\"{}\"", img.sha256);
    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag));
    let mut resp = if fresh {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        Body::from(img.data).into_response()
    };
    let h = resp.headers_mut();
    let mut set = |k, v: &str| {
        if let Ok(v) = HeaderValue::from_str(v) {
            h.insert(k, v);
        }
    };
    set(header::CONTENT_TYPE, &img.mime);
    set(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(
        header::CACHE_CONTROL,
        "private, max-age=31536000, immutable",
    );
    set(header::ETAG, &etag);
    set(header::CONTENT_SECURITY_POLICY, crate::artifacts::CSP);
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend(w.to_be_bytes());
        b.extend(h.to_be_bytes());
        b.extend([8, 6, 0, 0, 0]);
        b
    }

    #[test]
    fn sniffs_png_jpeg_and_webp_headers() {
        assert_eq!(sniff(&png(256, 128)), Some(("image/png", 256, 128)));
        // JPEG: APP0, then SOF2 (progressive) with height 300, width 200.
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xC2, 0, 11, 8];
        j.extend([1, 44, 0, 200]);
        assert_eq!(sniff(&j), Some(("image/jpeg", 200, 300)));
        let mut w = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0".to_vec();
        w.extend([255, 0, 0, 127, 0, 0]);
        assert_eq!(sniff(&w), Some(("image/webp", 256, 128)));
        let mut l = b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0\x2f".to_vec();
        let bits: u32 = 199 | (99 << 14);
        l.extend(bits.to_le_bytes());
        assert_eq!(sniff(&l), Some(("image/webp", 200, 100)));
        let mut v = b"RIFF\0\0\0\0WEBPVP8 \0\0\0\0\0\0\0\x9d\x01\x2a".to_vec();
        v.extend([0x40, 0x01, 0x40, 0x01]);
        assert_eq!(sniff(&v), Some(("image/webp", 320, 320)));
        assert_eq!(sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"), None);
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2]), None);
        assert_eq!(sniff(&png(1, 1)[..20]), None, "a cut header");
    }

    #[test]
    fn checks_square_and_size() {
        assert_eq!(check_image(&png(512, 512)).unwrap(), "image/png");
        assert!(check_image(&png(512, 511)).is_err());
        assert!(check_image(&png(64, 64)).is_err());
        assert!(check_image(&png(2048, 2048)).is_err());
        let mut big = png(512, 512);
        big.resize(ICON_MAX + 1, 0);
        assert!(check_image(&big).is_err());
    }

    #[test]
    fn emoji_and_letters() {
        let e = |v: &str| stored_icon(ProjectIcon::Emoji { value: v.into() });
        let l = |v: &str| stored_icon(ProjectIcon::Letters { value: v.into() });
        assert_eq!(e("🔌").unwrap(), json!({"kind": "emoji", "value": "🔌"}));
        assert!(e("👨‍👩‍👧‍👦").is_ok(), "a ZWJ family");
        assert!(e("").is_err() && e("a b").is_err() && e("abcdefghi").is_err());
        assert_eq!(
            l(" pm ").unwrap(),
            json!({"kind": "letters", "value": "PM"})
        );
        assert_eq!(l("ж").unwrap()["value"], "Ж");
        assert!(l("abc").is_err() && l("").is_err() && l("a\u{7}").is_err());
        assert!(stored_icon(ProjectIcon::Image { url: String::new() }).is_err());
    }
}
