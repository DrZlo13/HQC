//! Skills the owner keeps on the hub (SPEC 0.245, iteration 8, `docs/ITERATION-8.md`).
//!
//! The owner uploads a skill as a folder (multipart `file` parts, the path in each file name)
//! or a `.zip`; the hub checks it ([`hqc_proto::skills`]: paths inside it, no links, limits, no
//! `hooks` in a `SKILL.md`) and keeps an archive it writes itself (paths sorted, fixed times),
//! whose sha256 sessions of the skill's projects get in `session.start.skills`. A worker
//! fetches the archive with `GET /worker/skills/{sha256}` and checks it again.

use std::collections::BTreeMap;
use std::io::{Read, Write};

use axum::{
    body::Body,
    extract::{Multipart, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use hqc_proto::{
    api::{Skill, SkillFile, SkillFiles, SkillList, SkillRequest},
    skills::{
        check_path, check_skill_md, valid_sha256, valid_skill_name, SkillRef, MAX_SKILL_BYTES,
        MAX_SKILL_FILES, SKILL_MD,
    },
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    app::AppState,
    auth::CurrentUser,
    db,
    error::{ApiError, ApiResult},
};

/// An upload (a `.zip`, or a folder's files with the multipart framing) is at most this.
pub const BODY_LIMIT: usize = 9 * 1024 * 1024;
/// A `.zip` uploaded: at most this many bytes.
const MAX_ZIP: usize = 8 * 1024 * 1024;
/// Skills per owner.
const MAX_SKILLS: i64 = 100;

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    name: String,
    description: Option<String>,
    allowed_tools: Vec<String>,
    sha256: String,
    size: i64,
    files: i32,
    all_projects: bool,
    updated_at: DateTime<Utc>,
}

const COLS: &str =
    "id, name, description, allowed_tools, sha256, size, files, all_projects, updated_at";

async fn to_api(db: &sqlx::PgPool, r: Row) -> sqlx::Result<Skill> {
    let project_ids = sqlx::query_scalar(
        "SELECT project_id FROM skill_projects WHERE skill_id = $1 ORDER BY project_id",
    )
    .bind(r.id)
    .fetch_all(db)
    .await?;
    Ok(Skill {
        id: r.id,
        name: r.name,
        description: r.description,
        allowed_tools: r.allowed_tools,
        sha256: r.sha256,
        size: r.size as u64,
        files: r.files as u32,
        all_projects: r.all_projects,
        project_ids,
        updated_at: r.updated_at,
    })
}

/// The skills sessions of `project` get (`session.start.skills`).
pub async fn refs_for(
    db: &sqlx::PgPool,
    owner: Uuid,
    project: Uuid,
) -> sqlx::Result<Vec<SkillRef>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, sha256 FROM skills s
         WHERE owner_id = $1
           AND (all_projects OR EXISTS (SELECT 1 FROM skill_projects p
                                        WHERE p.skill_id = s.id AND p.project_id = $2))
         ORDER BY name",
    )
    .bind(owner)
    .bind(project)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(name, sha256)| SkillRef { name, sha256 })
        .collect())
}

// ---------------------------------------------------------------- the archive

/// A file of a skill: its content and whether it runs.
type Files = BTreeMap<String, (Vec<u8>, bool)>;

/// A skill checked and packed.
#[derive(Debug)]
pub struct Packed {
    pub name: String,
    pub description: Option<String>,
    pub allowed_tools: Vec<String>,
    pub archive: Vec<u8>,
    pub sha256: String,
    pub size: u64,
    pub files: usize,
}

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::BadRequest(m.into())
}

/// Left out of an upload, as a file manager or git leaves them.
fn junk(path: &str) -> bool {
    path.split('/')
        .any(|p| p == "__MACOSX" || p == ".git" || p == ".DS_Store")
}

/// The files of an uploaded `.zip`: no links, at most [`MAX_SKILL_BYTES`] unpacked.
fn read_zip(data: &[u8]) -> ApiResult<Files> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(data))
        .map_err(|e| bad(format!("not a zip: {e}")))?;
    if z.len() > MAX_SKILL_FILES * 4 {
        return Err(bad(format!("a skill has at most {MAX_SKILL_FILES} files")));
    }
    let mut out = Files::new();
    let mut total = 0u64;
    for i in 0..z.len() {
        let mut f = z.by_index(i).map_err(|e| bad(format!("the zip: {e}")))?;
        let name = f
            .name()
            .map_err(|e| bad(format!("the zip: {e}")))?
            .to_string();
        if f.is_dir() || junk(&name) {
            continue;
        }
        if f.is_symlink() {
            return Err(bad(format!("{name}: a link is not taken")));
        }
        let executable = f.unix_mode().is_some_and(|m| m & 0o111 != 0);
        let mut buf = vec![];
        (&mut f)
            .take(MAX_SKILL_BYTES - total + 1)
            .read_to_end(&mut buf)
            .map_err(|e| bad(format!("{name}: {e}")))?;
        total += buf.len() as u64;
        if total > MAX_SKILL_BYTES {
            return Err(too_large());
        }
        if out.insert(name.clone(), (buf, executable)).is_some() {
            return Err(bad(format!("{name} is in the zip twice")));
        }
    }
    Ok(out)
}

fn too_large() -> ApiError {
    ApiError::TooLarge(format!(
        "a skill is at most {} MiB unpacked",
        MAX_SKILL_BYTES / 1024 / 1024
    ))
}

/// Checks the files of a skill and packs them. `SKILL.md` is at the top or under one top
/// folder, whose name (else `given`) names the skill unless its frontmatter does.
pub fn pack(mut files: Files, given: Option<&str>) -> ApiResult<Packed> {
    files.retain(|p, _| !junk(p));
    let mut fallback = given
        .map(|g| g.trim().trim_end_matches(".zip").to_string())
        .filter(|g| !g.is_empty());
    if !files.contains_key(SKILL_MD) {
        let tops: std::collections::BTreeSet<&str> = files
            .keys()
            .map(|p| p.split('/').next().unwrap_or(""))
            .collect();
        let top = match tops.into_iter().collect::<Vec<_>>()[..] {
            [t] if files.contains_key(&format!("{t}/{SKILL_MD}")) => t.to_string(),
            _ => return Err(bad("no SKILL.md at the top of the skill")),
        };
        let prefix = format!("{top}/");
        files = files
            .into_iter()
            .map(|(p, v)| match p.strip_prefix(&prefix) {
                Some(rest) => Ok((rest.to_string(), v)),
                None => Err(bad(format!("{p} is a file and a folder"))),
            })
            .collect::<ApiResult<_>>()?;
        fallback = Some(top);
    }
    if files.len() > MAX_SKILL_FILES {
        return Err(bad(format!("a skill has at most {MAX_SKILL_FILES} files")));
    }
    let size: u64 = files.values().map(|(d, _)| d.len() as u64).sum();
    if size > MAX_SKILL_BYTES {
        return Err(too_large());
    }
    let mut top = None;
    for (path, (data, _)) in &files {
        check_path(path).map_err(bad)?;
        if let Some((parent, _)) = path.rsplit_once('/') {
            let mut p = parent;
            loop {
                if files.contains_key(p) {
                    return Err(bad(format!("{p} is a file and a folder")));
                }
                match p.rsplit_once('/') {
                    Some((up, _)) => p = up,
                    None => break,
                }
            }
        }
        let file = path.rsplit('/').next().unwrap_or(path);
        if !file.eq_ignore_ascii_case(SKILL_MD) {
            continue;
        }
        let text =
            std::str::from_utf8(data).map_err(|_| bad(format!("{path} is not UTF-8 text")))?;
        let fm = check_skill_md(text).map_err(|m| {
            if m.contains("hooks") {
                ApiError::Unprocessable("skill_hooks", format!("{path}: {m}"))
            } else {
                bad(format!("{path}: {m}"))
            }
        })?;
        if path == SKILL_MD {
            top = Some(fm);
        }
    }
    let fm = top.ok_or_else(|| bad("no SKILL.md at the top of the skill"))?;
    let name = fm
        .name
        .clone()
        .or(fallback)
        .ok_or_else(|| bad("the skill has no name: give SKILL.md a name"))?;
    if !valid_skill_name(&name) {
        return Err(bad(format!(
            "{name:?}: a skill's name is 1 to 64 of a-z, 0-9, '-' and '_', starting with a \
             letter or a digit"
        )));
    }
    let archive = write_zip(&files).map_err(|e| ApiError::Internal(e.into()))?;
    let sha256 = hex(&Sha256::digest(&archive));
    Ok(Packed {
        name,
        description: fm.description.map(|d| d.chars().take(1024).collect()),
        allowed_tools: fm.allowed_tools,
        archive,
        sha256,
        size,
        files: files.len(),
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The archive the hub keeps: paths sorted, fixed times, `0755` or `0644`.
fn write_zip(files: &Files) -> zip::result::ZipResult<Vec<u8>> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (path, (data, executable)) in files {
        let o = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .last_modified_time(zip::DateTime::default())
            .unix_permissions(if *executable { 0o755 } else { 0o644 });
        z.start_file(path.as_str(), o)?;
        z.write_all(data)?;
    }
    Ok(z.finish()?.into_inner())
}

// ---------------------------------------------------------------- REST

async fn own(app: &AppState, user: &CurrentUser, id: Uuid) -> ApiResult<Row> {
    sqlx::query_as(&format!(
        "SELECT {COLS} FROM skills WHERE id = $1 AND owner_id = $2"
    ))
    .bind(id)
    .bind(user.id)
    .fetch_optional(&app.db)
    .await?
    .ok_or(ApiError::NotFound)
}

/// `GET /api/skills`.
pub async fn list(State(app): State<AppState>, user: CurrentUser) -> ApiResult<Json<SkillList>> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM skills WHERE owner_id = $1 ORDER BY name"
    ))
    .bind(user.id)
    .fetch_all(&app.db)
    .await?;
    let mut skills = vec![];
    for r in rows {
        skills.push(to_api(&app.db, r).await?);
    }
    Ok(Json(SkillList { skills }))
}

/// `POST /api/skills`, multipart: a `zip` part, or `file` parts whose file names are the
/// paths inside the skill; `name` names it when its `SKILL.md` does not. Creates the skill
/// (`201`, every project) or replaces the content of the owner's skill of that name (`200`,
/// its projects kept).
pub async fn upload(
    State(app): State<AppState>,
    user: CurrentUser,
    mut mp: Multipart,
) -> ApiResult<(StatusCode, Json<Skill>)> {
    let multipart = |e: axum::extract::multipart::MultipartError| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            too_large()
        } else {
            bad(format!("multipart: {}", e.body_text()))
        }
    };
    let mut zip: Option<Vec<u8>> = None;
    let mut files = Files::new();
    let mut total = 0usize;
    let mut name: Option<String> = None;
    while let Some(mut field) = mp.next_field().await.map_err(multipart)? {
        let kind = field.name().unwrap_or("").to_string();
        let file_name = field.file_name().map(String::from);
        let mut buf = Vec::new();
        while let Some(c) = field.chunk().await.map_err(multipart)? {
            buf.extend_from_slice(&c);
            total += c.len();
            if total > MAX_ZIP {
                return Err(too_large());
            }
        }
        match kind.as_str() {
            "zip" if zip.is_none() => zip = Some(buf),
            "file" => {
                let path = file_name
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| bad("a file part needs its path as its file name"))?;
                if files.insert(path.clone(), (buf, false)).is_some() {
                    return Err(bad(format!("{path} is given twice")));
                }
            }
            "name" => name = String::from_utf8(buf).ok(),
            _ => return Err(bad(format!("unexpected part {kind:?}"))),
        }
    }
    let files = match zip {
        Some(_) if !files.is_empty() => return Err(bad("a zip or files, not both")),
        Some(z) => read_zip(&z)?,
        None if files.is_empty() => return Err(bad("the skill's files or a zip are required")),
        None => files,
    };
    let p = pack(files, name.as_deref())?;
    let exists: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM skills WHERE owner_id = $1 AND name = $2")
            .bind(user.id)
            .bind(&p.name)
            .fetch_optional(&app.db)
            .await?;
    if exists.is_none() {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM skills WHERE owner_id = $1")
            .bind(user.id)
            .fetch_one(&app.db)
            .await?;
        if n >= MAX_SKILLS {
            return Err(ApiError::Conflict(
                "too_many_skills",
                format!("at most {MAX_SKILLS} skills"),
            ));
        }
    }
    let (id, inserted): (Uuid, bool) = sqlx::query_as(
        "INSERT INTO skills (id, owner_id, name, description, allowed_tools, archive, sha256,
                             size, files)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (owner_id, name) DO UPDATE
           SET description = EXCLUDED.description, allowed_tools = EXCLUDED.allowed_tools,
               archive = EXCLUDED.archive, sha256 = EXCLUDED.sha256, size = EXCLUDED.size,
               files = EXCLUDED.files, updated_at = now()
         RETURNING id, (xmax = 0)",
    )
    .bind(db::new_id())
    .bind(user.id)
    .bind(&p.name)
    .bind(&p.description)
    .bind(&p.allowed_tools)
    .bind(&p.archive)
    .bind(&p.sha256)
    .bind(p.size as i64)
    .bind(p.files as i32)
    .fetch_one(&app.db)
    .await?;
    tracing::info!(skill_id = %id, name = %p.name, sha256 = %p.sha256, size = p.size, inserted, "skill uploaded");
    let r = own(&app, &user, id).await?;
    let status = if inserted {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(to_api(&app.db, r).await?)))
}

/// `PATCH /api/skills/{id}`: which projects get it.
pub async fn update(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<SkillRequest>,
) -> ApiResult<Json<Skill>> {
    own(&app, &user, id).await?;
    if let Some(ids) = &b.project_ids {
        let distinct: std::collections::HashSet<_> = ids.iter().collect();
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM projects WHERE id = ANY($1) AND owner_id = $2",
        )
        .bind(ids)
        .bind(user.id)
        .fetch_one(&app.db)
        .await?;
        if n as usize != distinct.len() {
            return Err(bad("project_ids: a project that is not yours"));
        }
    }
    let mut tx = app.db.begin().await?;
    if let Some(all) = b.all_projects {
        sqlx::query("UPDATE skills SET all_projects = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(all)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(ids) = &b.project_ids {
        sqlx::query("DELETE FROM skill_projects WHERE skill_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO skill_projects (skill_id, project_id)
             SELECT $1, unnest($2::uuid[]) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(ids)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let r = own(&app, &user, id).await?;
    Ok(Json(to_api(&app.db, r).await?))
}

/// `DELETE /api/skills/{id}`.
pub async fn delete(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    own(&app, &user, id).await?;
    sqlx::query("DELETE FROM skills WHERE id = $1 AND owner_id = $2")
        .bind(id)
        .bind(user.id)
        .execute(&app.db)
        .await?;
    tracing::info!(skill_id = %id, "skill deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/skills/{id}/files`: the paths and sizes in its archive.
pub async fn files(
    State(app): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SkillFiles>> {
    own(&app, &user, id).await?;
    let archive: Vec<u8> = sqlx::query_scalar("SELECT archive FROM skills WHERE id = $1")
        .bind(id)
        .fetch_one(&app.db)
        .await?;
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .map_err(|e| ApiError::Internal(e.into()))?;
    let mut files = vec![];
    for i in 0..z.len() {
        let f = z.by_index(i).map_err(|e| ApiError::Internal(e.into()))?;
        files.push(SkillFile {
            path: f
                .name()
                .map_err(|e| ApiError::Internal(e.into()))?
                .to_string(),
            size: f.size(),
            executable: f.unix_mode().is_some_and(|m| m & 0o111 != 0),
        });
    }
    Ok(Json(SkillFiles { files }))
}

/// `GET /worker/skills/{sha256}` with the worker token (SPEC §5.3): the archive of a skill of
/// the worker's owner; anything else is `404`.
pub async fn for_worker(
    State(app): State<AppState>,
    crate::security::ClientIp(ip): crate::security::ClientIp,
    Path(sha256): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let w = crate::artifacts::worker_of(&app, &ip, &headers).await?;
    if !valid_sha256(&sha256) {
        return Err(ApiError::NotFound);
    }
    let archive: Vec<u8> = sqlx::query_scalar(
        "SELECT archive FROM skills WHERE owner_id = $1 AND sha256 = $2 LIMIT 1",
    )
    .bind(w.owner_id)
    .bind(&sha256)
    .fetch_optional(&app.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from(archive),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[(&str, &str)]) -> Files {
        list.iter()
            .map(|(p, c)| (p.to_string(), (c.as_bytes().to_vec(), false)))
            .collect()
    }

    #[test]
    fn a_folder_and_a_zip_of_it_pack_the_same() {
        let folder = files(&[
            (
                "SKILL.md",
                "---\ndescription: Bench it.\nallowed-tools: Read, Bash(ls:*)\n---\n",
            ),
            ("scripts/run.sh", "echo hi\n"),
            (".DS_Store", "junk"),
        ]);
        let a = pack(folder, Some("bench")).unwrap();
        assert_eq!(a.name, "bench");
        assert_eq!(a.files, 2);
        assert_eq!(a.description.as_deref(), Some("Bench it."));
        assert_eq!(a.allowed_tools, ["Read", "Bash(ls:*)"]);
        // The same files under a top folder, in a zip with junk of a Mac's.
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default().unix_permissions(0o644);
        for (p, c) in [
            ("bench/scripts/run.sh", "echo hi\n"),
            (
                "bench/SKILL.md",
                "---\ndescription: Bench it.\nallowed-tools: Read, Bash(ls:*)\n---\n",
            ),
            ("__MACOSX/bench/._SKILL.md", "x"),
        ] {
            z.start_file(p, o).unwrap();
            z.write_all(c.as_bytes()).unwrap();
        }
        let zipped = z.finish().unwrap().into_inner();
        let b = pack(read_zip(&zipped).unwrap(), Some("other.zip")).unwrap();
        assert_eq!(b.name, "bench", "the top folder names it");
        assert_eq!(a.sha256, b.sha256);
        assert_eq!(a.archive, b.archive);
    }

    #[test]
    fn the_frontmatter_names_it() {
        let p = pack(files(&[("SKILL.md", "---\nname: probe\n---\n")]), Some("x")).unwrap();
        assert_eq!(p.name, "probe");
    }

    #[test]
    fn bad_skills_are_refused() {
        let refused = |f: Files, given: Option<&str>| pack(f, given).unwrap_err();
        assert!(matches!(
            refused(
                files(&[("SKILL.md", "---\nhooks:\n  Stop: []\n---\n")]),
                Some("x")
            ),
            ApiError::Unprocessable("skill_hooks", _)
        ));
        assert!(matches!(
            refused(
                files(&[("SKILL.md", ""), ("a/SKILL.md", "---\n'hooks': 1\n---\n")]),
                Some("x")
            ),
            ApiError::Unprocessable("skill_hooks", _)
        ));
        for (f, why) in [
            (files(&[("README.md", "")]), "no SKILL.md"),
            (files(&[("a/SKILL.md", ""), ("b/x", "")]), "two tops"),
            (files(&[("SKILL.md", ""), ("../x", "")]), "out"),
            (files(&[("SKILL.md", ""), ("/x", "")]), "absolute"),
            (files(&[("SKILL.md", ""), ("a", ""), ("a/b", "")]), "clash"),
            (files(&[("SKILL.md", "---\nname: Bad Name\n---\n")]), "name"),
            (files(&[("SKILL.md", "---\nname: x\n")]), "unclosed"),
            (
                files(&[("t", ""), ("t/SKILL.md", "")]),
                "a file named as the top folder",
            ),
        ] {
            assert!(pack(f, Some("x")).is_err(), "{why}");
        }
        assert!(pack(files(&[("SKILL.md", "")]), None).is_err(), "no name");
        let big = "x".repeat(MAX_SKILL_BYTES as usize + 1);
        assert!(matches!(
            refused(files(&[("SKILL.md", ""), ("big", &big)]), Some("x")),
            ApiError::TooLarge(_)
        ));
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("SKILL.md", o).unwrap();
        z.add_symlink("link", "/etc/passwd", o).unwrap();
        assert!(
            read_zip(&z.finish().unwrap().into_inner()).is_err(),
            "a link"
        );
    }
}
