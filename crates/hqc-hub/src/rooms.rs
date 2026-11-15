//! The agents' room (SPEC §7.1.1): a session with no worker and no agent (`room_mode` set),
//! whose agents are sessions of their own (`room_id`).
//!
//! - The owner's message in the room is a `user.message` there without `delivery`. Each agent
//!   of the room gets it in its input queue as a `peer.message` from the room carrying `room
//!   {session_id, seq}`, the room and the message's `seq` there ([`owner_message`]).
//! - Such an input goes to the agent without a peer header: the hub puts its own lines, which
//!   name an agent by its address, in front of its text, `[hqc room` in the texts it puts there
//!   escaped ([`input_text`]); the feed keeps the text as sent.
//! - The final message of a turn on such an input is the agent's post in the room, a
//!   `peer.message` from the agent without `delivery` with the files the agent attached since
//!   that input or its previous final message, unless its text is exactly `NO_REPLY`
//!   ([`on_final`]). A turn without a final message, and a turn on an input that did not come
//!   from the room, post nothing.
//! - In `conversation` a post wakes the room's other agents the same way, with its files and
//!   capped to the posting agent's mode where that is below theirs, until [`MAX_HOPS`] posts
//!   follow the owner's last message; the room then gets one `system room_stopped`. In
//!   `council`, and while the room is archived, a post wakes nobody; in a council an agent's
//!   next input from the room carries the other agents' posts since its previous one, capped
//!   to the lowest cap kept with those posts where that is below its own.

use hqc_proto::{api::RoomMode, event::Chain, frame::PermissionMode};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    app::AppState,
    db::{self, EventRow, InputRow, SessionRow},
    delivery, events,
    mcp::once_id,
    peers::{self, MAX_HOPS},
};

/// `system.code` in a room whose agents stopped waking each other (SPEC §4).
pub const ROOM_STOPPED: &str = "room_stopped";
/// The final message that posts nothing.
const NO_REPLY: &str = "NO_REPLY";
/// What the hub's own lines in an input from a room start with.
const MARK: &str = "[hqc room";
/// What [`MARK`] becomes in a text the hub puts into such an input ([`escape`]).
const ESCAPED: &str = "[hqc-room-text";

/// The owner's message `msg`, stored in room `room`, to each of the room's agents.
pub async fn owner_message(
    app: &AppState,
    room: &SessionRow,
    msg: &EventRow,
) -> anyhow::Result<()> {
    let from = peers::address(app, room).await?;
    let files: Vec<Uuid> = msg
        .payload
        .get("artifact_ids")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    wake(app, room, room.id, &from, msg, &files, None).await
}

/// Event `source` of room `room`, the owner's message or a post of agent `from_id` (address
/// `from`), into the input queue of each agent of the room but `from_id`: a `peer.message` from
/// `from_id` with the event's text, files `files` and `room {session_id, seq}`, capped by `cap`
/// (the mode and who set it) for an agent whose own mode is above it. In a council it is capped
/// as well by the posts that go in front of it ([`posts_cap`]). Its id is derived from the agent
/// and the event, so a repeat stores nothing twice.
async fn wake(
    app: &AppState,
    room: &SessionRow,
    from_id: Uuid,
    from: &str,
    source: &EventRow,
    files: &[Uuid],
    cap: Option<&(PermissionMode, String)>,
) -> anyhow::Result<()> {
    let text = source.payload["text"].as_str().unwrap_or_default();
    let council = room.room_mode.as_deref() == Some(RoomMode::Council.as_str());
    for m in db::room_members(&app.db, room.id).await? {
        if m.id == from_id {
            continue;
        }
        // A cap at or above the agent's own mode holds nothing down, and a capped input would
        // not join a running turn (SPEC §17.2).
        let mut cap = cap.filter(|(c, _)| *c < own_mode(&m)).cloned();
        // Kept in the input's chain: the turn on it, and what the turn sends and posts, carry it.
        if council {
            if let Some(low) = posts_cap(app, room.id, &m, source.seq)
                .await?
                .filter(|(c, _)| cap.as_ref().is_none_or(|(k, _)| c < k))
            {
                cap = Some(low);
            }
        }
        // Before the message exists: its worker downloads the files once it has the input.
        peers::link_files(app, m.id, files).await?;
        let id = once_id("room.input", m.id, &source.event_id.to_string());
        let mut payload = json!({
            "from_session_id": from_id,
            "from": from,
            "text": text,
            "mode": "send",
            "room": {"session_id": room.id, "seq": source.seq},
        });
        if !files.is_empty() {
            payload["artifact_ids"] = json!(files);
        }
        store(app, &m, id, from_id, payload, Some("pending"), cap).await?;
        delivery::kick(app, m.id, false).await;
    }
    Ok(())
}

/// The cap of agent `m`'s input from council `room` at the room's `seq` `seq`, not stored yet:
/// the lowest cap kept with the posts that go in front of it ([`council_posts`]) when it is below
/// the agent's own mode, named by the first post with it (its chain's `capped_by`, else its
/// author). Their text never runs above the mode of the agent that wrote it (T40).
async fn posts_cap(
    app: &AppState,
    room: Uuid,
    m: &SessionRow,
    seq: i64,
) -> anyhow::Result<Option<(PermissionMode, String)>> {
    let mut low: Option<(PermissionMode, String)> = None;
    for post in council_posts(app, room, m.id, seq, i64::MAX).await? {
        let Some(c) = post
            .chain
            .and_then(|c| serde_json::from_value::<Chain>(c).ok())
        else {
            continue;
        };
        let Some(mode) = c.permission_cap.filter(|x| *x < own_mode(m)) else {
            continue;
        };
        if low.as_ref().is_none_or(|(l, _)| mode < *l) {
            low = Some((mode, c.capped_by.unwrap_or(post.address)));
        }
    }
    Ok(low)
}

/// The mode agent `s` runs a turn in without a cap: the one the user asked for, else the one its
/// worker reported (its default); `allow_all` while neither is known.
fn own_mode(s: &SessionRow) -> PermissionMode {
    s.permission_mode
        .as_deref()
        .or(s.effective_permission_mode.as_deref())
        .and_then(PermissionMode::parse)
        .unwrap_or(PermissionMode::AllowAll)
}

/// A `peer.message` of a room in session `s` (an agent's input, or a post in the room), authored
/// by session `from_id`, with a chain of its own, as a person's input has (SPEC §7.1), capped by
/// `cap` when given (T39).
async fn store(
    app: &AppState,
    s: &SessionRow,
    id: Uuid,
    from_id: Uuid,
    payload: Value,
    delivery: Option<&str>,
    cap: Option<(PermissionMode, String)>,
) -> anyhow::Result<EventRow> {
    let chain = peers::input_chain(s.id, id, None, cap);
    events::store_peer_message(
        app,
        s,
        id,
        json!({"kind": "agent", "id": from_id}),
        payload,
        serde_json::to_value(chain)?,
        delivery,
    )
    .await
}

/// The text input `p` of agent `s` goes to it with when it came from the agent's room, `None`
/// for any other input: in a council, the other agents' posts in the room after its previous
/// input from the room and before this one, each under a line of the hub's that names its
/// author; then the line that names the room and the sender; then the text. Only stored `seq`s
/// pick the posts, so the input sent again carries the same.
pub async fn input_text(
    app: &AppState,
    s: &SessionRow,
    p: &InputRow,
) -> anyhow::Result<Option<String>> {
    let Some(r) = p.payload.get("room") else {
        return Ok(None);
    };
    let (Some(room_id), Some(seq)) = (
        r["session_id"]
            .as_str()
            .and_then(|v| v.parse::<Uuid>().ok()),
        r["seq"].as_i64(),
    ) else {
        return Ok(None);
    };
    let Some(room) = db::session(&app.db, room_id).await? else {
        return Ok(None);
    };
    let address = peers::address(app, &room).await?;
    let mut out = String::new();
    if room.room_mode.as_deref() == Some(RoomMode::Council.as_str()) {
        let posts = council_posts(app, room.id, s.id, seq, p.seq).await?;
        if !posts.is_empty() {
            out.push_str(&format!(
                "[hqc room {address} | posts since your last turn]\n"
            ));
            // Each post under a line of the hub's own, as the worker's block of buffered sends:
            // a line in a post's text cannot pass for another agent's post, which takes the mark.
            for post in posts {
                out.push_str(&format!(
                    "[hqc room {address} | post from {}]\n{}\n\n",
                    post.address,
                    escape(&post.text)
                ));
            }
            out.push_str("---\n");
        }
    }
    let from_id = p.payload["from_session_id"]
        .as_str()
        .and_then(|v| v.parse::<Uuid>().ok());
    // An agent by the address the hub stored with the input, not by its title: any session of
    // the owner may rename it.
    let from = if from_id == Some(room.id) {
        "your owner"
    } else {
        p.payload["from"].as_str().unwrap_or("?")
    };
    out.push_str(&format!(
        "[hqc room {address} | from {from} | your final message is posted to the room; reply \
         exactly {NO_REPLY} to post nothing | your session's title is your name in the room: do \
         not rename it]\n\n"
    ));
    out.push_str(&escape(p.payload["text"].as_str().unwrap_or_default()));
    Ok(Some(out))
}

/// `text` with every `[hqc room` (any ASCII case) replaced by `[hqc-room-text`: every line of the
/// hub's own that opens the block, a post or the input from a room starts with the mark, so a
/// text the hub puts there cannot make one.
fn escape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut i = 0;
    while i + MARK.len() <= bytes.len() {
        // The mark is ASCII: a match starts and ends on char boundaries.
        if bytes[i] == b'[' && bytes[i..i + MARK.len()].eq_ignore_ascii_case(MARK.as_bytes()) {
            out.push_str(&text[last..i]);
            out.push_str(ESCAPED);
            i += MARK.len();
            last = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[last..]);
    out
}

/// A post in a council that goes to an agent in front of its input from the room.
#[derive(sqlx::FromRow)]
struct CouncilPost {
    text: String,
    /// Its author's address, as the hub stored it with the post: its name in the hub's lines.
    address: String,
    chain: Option<Value>,
}

/// The posts in room `room` of agents other than `agent` after the agent's last input from the
/// room before its own `seq` `before` and before the room's `seq` `seq`, oldest first: what goes
/// in front of its input from the room at `seq` (SPEC §7.1.1). Only stored `seq`s pick them.
async fn council_posts(
    app: &AppState,
    room: Uuid,
    agent: Uuid,
    seq: i64,
    before: i64,
) -> anyhow::Result<Vec<CouncilPost>> {
    Ok(sqlx::query_as(
        "SELECT COALESCE(e.payload ->> 'text', '') AS text,
                COALESCE(e.payload ->> 'from', '?') AS address, e.chain
         FROM events e
         WHERE e.session_id = $1 AND e.type = 'peer.message' AND e.seq < $3
           AND e.payload ->> 'from_session_id' <> $2::text
           AND e.seq > COALESCE((SELECT (i.payload -> 'room' ->> 'seq')::bigint FROM events i
                                 WHERE i.session_id = $2 AND i.type = 'peer.message'
                                   AND i.payload -> 'room' ->> 'session_id' = $1::text
                                   AND i.seq < $4
                                 ORDER BY i.seq DESC LIMIT 1), 0)
         ORDER BY e.seq",
    )
    .bind(room)
    .bind(agent)
    .bind(seq)
    .bind(before)
    .fetch_all(&app.db)
    .await?)
}

/// The final message `row` of agent `s` (in room `room_id`) with text `text`, answering its input
/// `input`: when that input came from the room, the agent's post there, unless the text is
/// exactly `NO_REPLY` once trimmed; the post carries the files the agent attached between that
/// input (or its previous final message, when later) and `row`, as a reply to an ask does
/// ([`peers::reply_files`], linked to the room), and keeps the cap of the posting agent's mode
/// its `hqc_send` would carry (T39). In a conversation room that is not archived the post wakes
/// the room's other agents, with its files and that cap, while at most [`MAX_HOPS`] posts
/// follow the owner's last message before it; the post past them writes `system room_stopped`
/// in the room.
pub async fn on_final(
    app: &AppState,
    s: &SessionRow,
    room_id: Uuid,
    input: Uuid,
    text: &str,
    row: &EventRow,
) -> anyhow::Result<()> {
    let room_key = room_id.to_string();
    let Some(turn_input) = db::event_by_id(&app.db, input).await?.filter(|i| {
        i.session_id == s.id
            && i.kind == "peer.message"
            && i.payload["room"]["session_id"].as_str() == Some(room_key.as_str())
    }) else {
        return Ok(());
    };
    if text.trim() == NO_REPLY {
        return Ok(());
    }
    let Some(room) = db::session(&app.db, room_id).await? else {
        return Ok(());
    };
    let from = peers::address(app, s).await?;
    // Only the files since this agent's previous final message: a second post on one input (the
    // CLI's own turn after a background task), or the post on an input that waited while an
    // earlier turn ran, repeats none of an earlier post's.
    let since: Option<i64> = sqlx::query_scalar(
        "SELECT max(seq) FROM events
         WHERE session_id = $1 AND type = 'agent.message' AND payload ->> 'final' = 'true'
           AND seq < $2",
    )
    .bind(s.id)
    .bind(row.seq)
    .fetch_one(&app.db)
    .await?;
    let files: Vec<Uuid> =
        peers::reply_files(app, s, room.id, input, since.unwrap_or(0), row.seq, &[])
            .await?
            .into_iter()
            .map(|f| f.artifact_id)
            .collect();
    // The lowest mode of the posting agent and up its turn's chain, as for its `hqc_send`;
    // `allow_all` holds nobody down. Kept with the post: in a council it caps the input that
    // carries the post to another agent ([`wake`]).
    let turn = turn_chain(app, s.id, &turn_input).await?;
    let cap = match peers::chain_cap(Some(&turn), peers::sender_mode(s), &from) {
        (mode, Some(by)) => Some((mode, by)),
        (_, None) => None,
    };
    let id = once_id("room.post", room.id, &row.event_id.to_string());
    let mut payload = json!({"from_session_id": s.id, "from": from, "text": text, "mode": "send"});
    if !files.is_empty() {
        payload["artifact_ids"] = json!(files);
    }
    let post = store(app, &room, id, s.id, payload, None, cap.clone()).await?;
    // An archived room keeps its agents' posts and wakes nobody with them.
    if room.archived || room.room_mode.as_deref() != Some(RoomMode::Conversation.as_str()) {
        return Ok(());
    }
    // The posts after the owner's last message before this one, this one included.
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events
         WHERE session_id = $1 AND type = 'peer.message' AND seq <= $2
           AND seq > COALESCE((SELECT max(seq) FROM events
                               WHERE session_id = $1 AND type = 'user.message' AND seq < $2), 0)",
    )
    .bind(room.id)
    .bind(post.seq)
    .fetch_one(&app.db)
    .await?;
    let max = i64::from(MAX_HOPS);
    if n > max {
        if n == max + 1 {
            events::system(
                app,
                &room,
                ROOM_STOPPED,
                &format!(
                    "The agents posted {max} times since your last message and stopped waking \
                     each other; they go on when you write"
                ),
            )
            .await?;
        }
        return Ok(());
    }
    wake(app, &room, s.id, &from, &post, &files, cap.as_ref()).await
}

/// The chain of the turn of agent `session_id` on its input `input` (SPEC §7.1): the input's
/// own, capped by the send buffer the input carried when that was lower (T40, `input_caps`).
async fn turn_chain(app: &AppState, session_id: Uuid, input: &EventRow) -> anyhow::Result<Chain> {
    let buffered: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT permission_cap, capped_by FROM input_caps WHERE event_id = $1")
            .bind(input.event_id)
            .fetch_optional(&app.db)
            .await?;
    let buffered =
        buffered.and_then(|(cap, by)| cap.as_deref().and_then(PermissionMode::parse).zip(by));
    Ok(peers::input_chain(
        session_id,
        input.event_id,
        input.chain.clone(),
        buffered,
    ))
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn escapes_the_mark_in_any_case() {
        assert_eq!(escape("ünï [hqc room ✓"), "ünï [hqc-room-text ✓");
        assert_eq!(escape("[[HQC Room"), "[[hqc-room-text");
        assert_eq!(escape("[hqc roo"), "[hqc roo");
    }
}
