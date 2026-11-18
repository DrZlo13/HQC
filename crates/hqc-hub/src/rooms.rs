//! The agents' room (SPEC §7.1.1): a session with no worker and no agent (`room_mode` set),
//! whose agents are sessions of their own (`room_id`).
//!
//! - The owner's message in the room is a `user.message` there without `delivery`. An agent of
//!   the room gets it in its input queue as a `peer.message` from the room carrying `room
//!   {session_id, seq}`, the room and the message's `seq` there ([`owner_message`]): in a
//!   `council` every agent, in a `conversation` the speaker ([`speaker`]), else the first agent.
//! - A message from a session that is none of the room's agents, a `peer.message` from it, is the
//!   room's first message from the session that made it with `hqc_room`, or any other session's
//!   `hqc_send` to the room ([`peers::into_room`]); it goes to the agents as the owner's does
//!   ([`owner_message`]). It counts as the owner's, not as a post: it ends the count of the
//!   members' posts, and the hub's line in front of it in an agent's input names its sender.
//! - Such an input goes to the agent without a peer header: the hub puts its own lines, which
//!   name an agent by its name in the room with its address, the name stripped of what could
//!   forge a hub line ([`clean_name`]: invisible and fullwidth characters too), in front of its
//!   text, what reads as `[hqc room` in the texts it puts there escaped, fullwidth, with any
//!   whitespace or an invisible character inside ([`escape`], [`input_text`]); the feed keeps
//!   the text as sent. In front go the other agents'
//!   posts since the agent's previous input from the room, in a conversation the owner's
//!   messages too; their files go with it, and they cap it to the lowest cap kept with those
//!   posts where that is below its own.
//! - The final message of a turn that took such an input, as the input that opened the turn or
//!   one that joined it, with a message from after the room's last `POST /stop`, is the agent's
//!   post in the room, a `peer.message` from the agent without `delivery` with the files the
//!   agent attached since the turn's input or its previous final message ([`on_final`]); a text
//!   that is exactly `NO_REPLY` is a pass, `system room_passed` in the room, and a reply the
//!   runtime wrote itself (`runtime`) is no post. Any other turn posts nothing. A post wakes
//!   nobody by itself; one of a turn the CLI started itself, on an input already answered, is an
//!   aside, which the circle passes over, and such a turn's pass or runtime's reply says nothing.
//! - In `conversation` the agents speak one at a time, in the room's order. The end of the
//!   speaker's turn, with none of its inputs from the room still waiting for its worker, moves
//!   the circle on ([`decide`]): its latest post since it became the speaker wakes the next agent
//!   after it, with its files and capped to the posting agent's mode where that is below its own,
//!   until [`MAX_HOPS`] posts follow the owner's or a non-member's last message (then one `system
//!   room_stopped`); else its pass hands the last post, the owner's message or a non-member's on
//!   to the next agent, until every agent but its author passed; else the room stops
//!   (`room_stopped`). Its runtime's own reply, an input from the room that fails delivery and
//!   `POST /stop` of the room write `room_stopped` too. In `council` nobody is woken but by the
//!   owner's or a non-member's message, and while the room is archived by none but the owner's.
//! - A round of the room ends when, in a conversation, every agent but the last message's author
//!   passed ([`quiet_end`]), the posts limit or any other `room_stopped` was written, or the
//!   owner stopped the room; in a council, when no agent has a turn open or an input waiting
//!   ([`council_ended`]). Every end goes through [`round_ended`]: each session that is none of the
//!   room's agents and wrote in it gets a `peer.message` from the room with how it ended and
//!   the last two entries that came in the room since its previous notice or its first message,
//!   the others left to `hqc_read`.
//! - All of it is decided and stored under the room's [`lock`].

use hqc_proto::{api::RoomMode, event::Chain, frame::PermissionMode, mcp::MAX_ARGS};
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
/// `system.code` in a room whose agent answered `NO_REPLY` (SPEC §4).
pub const ROOM_PASSED: &str = "room_passed";
/// The text of the `room_stopped` line `POST /stop` of a room writes (SPEC §7.1.1).
pub const STOPPED: &str = "Stopped";
/// The final message that posts nothing.
const NO_REPLY: &str = "NO_REPLY";
/// The characters of a runtime's own reply a `room_stopped` line keeps.
const MAX_RUNTIME_TEXT: usize = 200;
/// The words of the `room_stopped` line for the speaker's turn that ended without a post or a
/// pass ([`decide`]).
const ENDED: &str = "ended without a post";
/// How many entries of the room a notice carries, the newest ones; the rest is read with
/// `hqc_read`.
const NOTICE_ENTRIES: usize = 2;
/// What the hub's own lines in an input from a room start with.
const MARK: &str = "[hqc room";
/// What [`MARK`] becomes in a text the hub puts into such an input ([`escape`]).
const ESCAPED: &str = "[hqc-room-text";

/// Whether `room` is a `conversation`.
fn is_conversation(room: &SessionRow) -> bool {
    room.room_mode.as_deref() == Some(RoomMode::Conversation.as_str())
}

/// The lock of room `room` (SPEC §7.1.1): who speaks, what is posted, which line is written and
/// who is woken next are decided and stored under it, so the owner's message (its caller stores
/// it under the lock, then calls [`owner_message`]), an agent's final message ([`on_final`]), an
/// input acked without opening a turn ([`acked`]), a turn's end ([`turn_ended`],
/// [`turn_lost`]), a failed input ([`input_failed`]) and `POST /stop` of the room take turns,
/// each seeing what the one before it stored. In memory: the hub is one process (SPEC §2), and
/// the hub's stores go through the pool, not one transaction a `pg_advisory_xact_lock` would hold
/// for.
pub async fn lock(app: &AppState, room: Uuid) -> tokio::sync::OwnedMutexGuard<()> {
    app.owner_lock("room", room).await
}

/// The owner's message `msg`, or a non-member's, stored in room `room` under its [`lock`], to the
/// room's agents: in a council to each of them; in a conversation to the speaker as of the
/// message, so that a repeat of it goes to the same agent, else to the first agent.
pub async fn owner_message(
    app: &AppState,
    room: &SessionRow,
    msg: &EventRow,
) -> anyhow::Result<()> {
    let mut members = db::room_members(&app.db, room.id).await?;
    if is_conversation(room) {
        let speaker = speaker(app, room.id, msg.seq).await?;
        let i = members
            .iter()
            .position(|m| Some(m.id) == speaker)
            .unwrap_or(0);
        members = members.into_iter().skip(i).take(1).collect();
    }
    wake(app, room, &members, msg).await
}

/// The chain of a message of a session that goes on in the inputs made from it: a non-member's
/// message and an agent's post carry one; the owner's message has none, and each input made from
/// it starts its own.
fn chain_of(source: &EventRow) -> Option<Chain> {
    (source.kind == "peer.message")
        .then(|| source.chain.clone())
        .flatten()
        .and_then(|c| serde_json::from_value(c).ok())
}

/// The files of a room's event, its `artifact_ids`.
fn files_of(payload: Option<&Value>) -> Vec<Uuid> {
    payload
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

/// Event `source` of room `room`, the owner's message, a non-member's or an agent's post, into the
/// input queue of each of `targets` but its author: a `peer.message` from its author (the room
/// for the owner's message) with the event's text, `room {session_id, seq}` and as files the
/// event's, then those of the messages that go in front of it ([`room_block`]), each once. The
/// input is capped by the cap kept with a post for an agent whose own mode is above it, and by
/// the posts that go in front of it ([`entries_cap`]); it carries the chain of a non-member's message
/// or a post on ([`chain_of`]), the owner's message starts one. Its id is derived from the agent
/// and the event, so a repeat stores nothing twice.
async fn wake(
    app: &AppState,
    room: &SessionRow,
    targets: &[SessionRow],
    source: &EventRow,
) -> anyhow::Result<()> {
    let text = source.payload["text"].as_str().unwrap_or_default();
    let source_files = files_of(source.payload.get("artifact_ids"));
    let (from_id, from, cap) = if source.kind == "peer.message" {
        let Some(from_id) = source.payload["from_session_id"]
            .as_str()
            .and_then(|v| v.parse::<Uuid>().ok())
        else {
            return Ok(());
        };
        let from = source.payload["from"].as_str().unwrap_or("?").to_string();
        // The cap of the posting agent's mode, kept in the post's chain (T39).
        let cap = source
            .chain
            .clone()
            .and_then(|c| serde_json::from_value::<Chain>(c).ok())
            .and_then(|c| {
                c.permission_cap
                    .map(|m| (m, c.capped_by.unwrap_or_else(|| from.clone())))
            });
        (from_id, from, cap)
    } else {
        (room.id, peers::address(app, room).await?, None)
    };
    for m in targets {
        if m.id == from_id {
            continue;
        }
        // A cap at or above the agent's own mode holds nothing down, and a capped input would
        // not join a running turn (SPEC §17.2).
        let mut cap = cap.clone().filter(|(c, _)| *c < own_mode(m));
        let block = room_block(
            app,
            room.id,
            m.id,
            source.seq,
            i64::MAX,
            is_conversation(room),
        )
        .await?;
        // Kept in the input's chain: the turn on it, and what the turn sends and posts, carry it.
        if let Some(low) =
            entries_cap(m, &block).filter(|(c, _)| cap.as_ref().is_none_or(|(k, _)| c < k))
        {
            cap = Some(low);
        }
        let mut files = source_files.clone();
        for f in block.iter().flat_map(|e| files_of(e.files.as_ref())) {
            if !files.contains(&f) {
                files.push(f);
            }
        }
        // Before the message exists: its worker downloads the files once it has the input.
        peers::link_files(app, m.id, &files).await?;
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
        // An input from a message that came from a session carries its chain on, hops and all: a
        // loop through the room runs into the chain limit. The owner's message starts one.
        let chain = made_chain(m, id, chain_of(source), cap);
        store(app, m, id, from_id, payload, Some("pending"), chain).await?;
        delivery::kick(app, m.id, false).await;
    }
    Ok(())
}

/// The speaker of conversation `room` as of its `seq` `before` (SPEC §7.1.1): the agent the room
/// woke last before it (its latest input from the room, by `room.seq`; of two woken by one event
/// the later), while the room has, after that `seq` and before `before`, no `system
/// room_stopped`, and not every agent passed on the room's last message ([`all_passed`]). Its
/// posts and its pass end nothing by themselves: the end of its turn moves the circle on
/// ([`decide`]). Only stored events decide it.
async fn speaker(app: &AppState, room: Uuid, before: i64) -> anyhow::Result<Option<Uuid>> {
    let w: Option<Uuid> = sqlx::query_scalar(
        "SELECT w.session_id FROM (
             SELECT i.session_id, (i.payload -> 'room' ->> 'seq')::bigint AS woken
             FROM events i JOIN sessions m ON m.id = i.session_id
             WHERE m.room_id = $1 AND NOT m.archived AND i.type = 'peer.message'
               AND i.payload -> 'room' ->> 'session_id' = $1::text
               AND (i.payload -> 'room' ->> 'seq')::bigint < $2
             ORDER BY woken DESC, i.ts DESC LIMIT 1) w
         WHERE NOT EXISTS (
             SELECT 1 FROM events e
             WHERE e.session_id = $1 AND e.seq > w.woken AND e.seq < $2 AND e.type = 'system'
               AND e.payload ->> 'code' = $3)",
    )
    .bind(room)
    .bind(before)
    .bind(ROOM_STOPPED)
    .fetch_optional(&app.db)
    .await?;
    let Some(w) = w else {
        return Ok(None);
    };
    if let Some(last) = last_message(app, room, before).await? {
        if all_passed(&db::room_members(&app.db, room).await?, &last) {
            return Ok(None);
        }
    }
    Ok(Some(w))
}

/// A room's last message: a post, the owner's message or a non-member's, its author (`None` for
/// the owner's, else the posting agent or the sender) and the agents that passed after it
/// ([`last_message`]).
struct LastMessage {
    event: EventRow,
    author: Option<Uuid>,
    passed: Vec<Uuid>,
}

/// The last message of room `room` before its `seq` `before`, an aside left out ([`post`]), with
/// the agents that passed after it and before `before`.
async fn last_message(
    app: &AppState,
    room: Uuid,
    before: i64,
) -> anyhow::Result<Option<LastMessage>> {
    let last: Option<Uuid> = sqlx::query_scalar(
        "SELECT event_id FROM events
         WHERE session_id = $1 AND type IN ('peer.message', 'user.message') AND seq < $2
           AND NOT (payload ? 'aside')
         ORDER BY seq DESC LIMIT 1",
    )
    .bind(room)
    .bind(before)
    .fetch_optional(&app.db)
    .await?;
    let Some(last) = last else {
        return Ok(None);
    };
    let Some(event) = db::event_by_id(&app.db, last).await? else {
        return Ok(None);
    };
    let author = if event.kind == "peer.message" {
        event.payload["from_session_id"]
            .as_str()
            .and_then(|v| v.parse::<Uuid>().ok())
    } else {
        None
    };
    let passed: Vec<Uuid> = sqlx::query_scalar(
        "SELECT (payload ->> 'from_session_id')::uuid FROM events
         WHERE session_id = $1 AND type = 'system' AND payload ->> 'code' = $2
           AND payload ? 'from_session_id' AND seq > $3 AND seq < $4",
    )
    .bind(room)
    .bind(ROOM_PASSED)
    .bind(event.seq)
    .bind(before)
    .fetch_all(&app.db)
    .await?;
    Ok(Some(LastMessage {
        event,
        author,
        passed,
    }))
}

/// Whether every one of `members` but the author of `last` passed on it: nobody is left to
/// speak.
fn all_passed(members: &[SessionRow], last: &LastMessage) -> bool {
    members
        .iter()
        .all(|m| Some(m.id) == last.author || last.passed.contains(&m.id))
}

/// The first of `members` after `after` in their order, wrapping, that is neither `after` nor
/// `skip`.
fn next_after(members: &[SessionRow], after: Uuid, skip: Option<Uuid>) -> Option<&SessionRow> {
    let i = members.iter().position(|m| m.id == after)?;
    members
        .iter()
        .cycle()
        .skip(i + 1)
        .take(members.len() - 1)
        .find(|m| Some(m.id) != skip)
}

/// The cap of session `m`'s input, not stored yet, with `block` in front of it ([`room_block`]),
/// or of its notice with the entries it carries ([`notify`], which leaves out the owner's
/// messages and the reader's own that its own mode capped): the lowest cap kept with the posts and
/// the messages there when it is below the session's own mode, named by the first with it (its
/// chain's `capped_by`, else its author). Their text never runs above the mode of the session that
/// wrote it (T40).
fn entries_cap<'a>(
    m: &SessionRow,
    block: impl IntoIterator<Item = &'a BlockEntry>,
) -> Option<(PermissionMode, String)> {
    let mut low: Option<(PermissionMode, String)> = None;
    for post in block {
        let Some(c) = post
            .chain
            .clone()
            .and_then(|c| serde_json::from_value::<Chain>(c).ok())
        else {
            continue;
        };
        let Some(mode) = c.permission_cap.filter(|x| *x < own_mode(m)) else {
            continue;
        };
        if low.as_ref().is_none_or(|(l, _)| mode < *l) {
            low = Some((mode, c.capped_by.unwrap_or_else(|| post.address.clone())));
        }
    }
    low
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

/// The chain of message `id` of room session `s`: `chain` gone on, or a person's, as an input of
/// a person has (SPEC §7.1), capped by `cap` when given (T39). Only `cap` lowers it: what the
/// chain capped is in `cap` already.
fn made_chain(
    s: &SessionRow,
    id: Uuid,
    chain: Option<Chain>,
    cap: Option<(PermissionMode, String)>,
) -> Chain {
    let chain = chain.map(|c| {
        json!(Chain {
            permission_cap: None,
            capped_by: None,
            ..c
        })
    });
    peers::input_chain(s.id, id, chain, cap)
}

/// A `peer.message` of a room in session `s` (an agent's input, or a post in the room), authored
/// by session `from_id`, with `chain` ([`made_chain`]).
async fn store(
    app: &AppState,
    s: &SessionRow,
    id: Uuid,
    from_id: Uuid,
    payload: Value,
    delivery: Option<&str>,
    chain: Chain,
) -> anyhow::Result<EventRow> {
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
/// for any other input: the other agents' posts in the room after its previous input from the
/// room and before this one, in a conversation the owner's messages too, each under a line of
/// the hub's that names its author; then the line that names the room and the sender; then the
/// text. Only stored `seq`s pick them, so the input sent again carries the same.
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
    let block = room_block(app, room.id, s.id, seq, p.seq, is_conversation(&room)).await?;
    if !block.is_empty() {
        out.push_str(&format!("[hqc room {address} | since your last turn]\n"));
        // Each post under a line of the hub's own, as the worker's block of buffered sends: a
        // line in a post's text cannot pass for another agent's post, which takes the mark.
        for e in block {
            let from = if e.owner {
                "message from your owner".to_string()
            } else if e.outsider {
                format!(
                    "message from {}",
                    named(e.name.as_deref(), &escape(&e.address))
                )
            } else {
                format!("post from {}", named(e.name.as_deref(), &e.address))
            };
            out.push_str(&format!(
                "[hqc room {address} | {from}]\n{}\n\n",
                escape(&e.text)
            ));
        }
        out.push_str("---\n");
    }
    let from_id = p.payload["from_session_id"]
        .as_str()
        .and_then(|v| v.parse::<Uuid>().ok());
    // The sender by its current title with the address the hub stored with the input.
    let from = if from_id == Some(room.id) {
        "your owner".to_string()
    } else {
        let title = match from_id {
            Some(id) => db::session(&app.db, id).await?.map(|f| f.title),
            None => None,
        };
        named(title.as_deref(), p.payload["from"].as_str().unwrap_or("?"))
    };
    let me = label(app, s).await?;
    out.push_str(&format!(
        "[hqc room {address} | from {from} | your final message is posted to the room, where the \
         other agents read it: do not message them | reply exactly {NO_REPLY} to post nothing | \
         your name in the room is {me}: do not rename your session]\n\n"
    ));
    out.push_str(&escape(p.payload["text"].as_str().unwrap_or_default()));
    Ok(Some(out))
}

/// A session's title as a name in a hub line: invisible characters (zero-width, bidi, variation
/// selectors; [`hqc_proto::lookalike::is_invisible`]) dropped,
/// every control character, Hangul filler ([`hqc_proto::lookalike::is_filler`], shown as a
/// blank) and every character that is or folds to `|`, `[`, `]` or `"` (a fullwidth `［`) a
/// space, runs of whitespace one space, trimmed, so a title cannot close the line, open another
/// or leave the quotes the hub puts round a name ([`named`]), or look like any of them.
fn clean_name(title: &str) -> String {
    use hqc_proto::lookalike::{folds_to_any, is_filler, is_invisible};
    title
        .chars()
        .filter(|c| !is_invisible(*c))
        .map(|c| {
            if c.is_control() || is_filler(c) || folds_to_any(c, &['|', '[', ']', '"']) {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Session `s` by its name in the room, in double quotes, its address alone when the name is
/// nothing once cleaned ([`clean_name`]).
async fn label(app: &AppState, s: &SessionRow) -> anyhow::Result<String> {
    let name = clean_name(&s.title);
    if name.is_empty() {
        peers::address(app, s).await
    } else {
        Ok(format!("\"{name}\""))
    }
}

/// A session in a hub line: `"name" (address)`, the address alone when the name is none or empty
/// once cleaned ([`clean_name`]). The quotes keep a name that reads "your owner" from passing for
/// the hub's own `your owner`.
fn named(title: Option<&str>, address: &str) -> String {
    match title.map(clean_name).filter(|n| !n.is_empty()) {
        Some(n) => format!("\"{n}\" ({address})"),
        None => address.to_string(),
    }
}

/// `text` with every place that reads as `[hqc room` replaced by `[hqc-room-text`: every line of
/// the hub's own that opens the block, a post or the input from a room starts with the mark, so a
/// text the hub puts there cannot make one. Any case, fullwidth or other compatibility forms,
/// any run of Unicode whitespace between the words and invisible characters inside count
/// ([`hqc_proto::lookalike::escape`]).
fn escape(text: &str) -> String {
    hqc_proto::lookalike::escape(text, MARK, ESCAPED)
}

/// A `peer.message` of a room (`e.session_id = $1`) from a session that is not one of its
/// members: a non-member's message, which counts as the owner's.
const NON_MEMBER: &str = "e.type = 'peer.message' AND e.payload ->> 'from_session_id' NOT IN
    (SELECT id::text FROM sessions WHERE room_id = $1)";

/// A post, or the owner's or a non-member's message, that goes to an agent in front of its input
/// from the room.
#[derive(sqlx::FromRow)]
struct BlockEntry {
    /// The owner's message, not a post.
    owner: bool,
    /// A non-member's message, not a post either ([`NON_MEMBER`]).
    outsider: bool,
    /// The message of the session the entry is read by, in a notice ([`notify`]); never in an
    /// agent's block.
    mine: bool,
    text: String,
    /// A post's author's address, as the hub stored it with the post.
    address: String,
    /// The author's current title, its name in the hub's lines ([`named`]); none for the owner.
    name: Option<String>,
    chain: Option<Value>,
    /// Its `artifact_ids`.
    files: Option<Value>,
}

/// The posts in room `room` of agents other than `agent`, and with `owner_messages` the owner's
/// messages and the non-members', after the agent's last input from the room before its own
/// `seq` `before` and before the room's `seq` `seq`, oldest first: what goes in front of its
/// input from the room at `seq` (SPEC §7.1.1). Only stored `seq`s pick them.
async fn room_block(
    app: &AppState,
    room: Uuid,
    agent: Uuid,
    seq: i64,
    before: i64,
    owner_messages: bool,
) -> anyhow::Result<Vec<BlockEntry>> {
    Ok(sqlx::query_as(&format!(
        "SELECT e.type = 'user.message' AS owner, ({NON_MEMBER}) AS outsider, false AS mine,
                COALESCE(e.payload ->> 'text', '') AS text,
                COALESCE(e.payload ->> 'from', '?') AS address, a.title AS name, e.chain,
                e.payload -> 'artifact_ids' AS files
         FROM events e
         LEFT JOIN sessions a ON a.id::text = e.payload ->> 'from_session_id'
         WHERE e.session_id = $1 AND e.seq < $3
           AND ((e.type = 'peer.message' AND e.payload ->> 'from_session_id' <> $2::text
                 AND ($5 OR NOT ({NON_MEMBER})))
                OR ($5 AND e.type = 'user.message'))
           AND e.seq > COALESCE((SELECT (i.payload -> 'room' ->> 'seq')::bigint FROM events i
                                 WHERE i.session_id = $2 AND i.type = 'peer.message'
                                   AND i.payload -> 'room' ->> 'session_id' = $1::text
                                   AND i.seq < $4
                                 ORDER BY i.seq DESC LIMIT 1), 0)
         ORDER BY e.seq"
    ))
    .bind(room)
    .bind(agent)
    .bind(seq)
    .bind(before)
    .bind(owner_messages)
    .fetch_all(&app.db)
    .await?)
}

/// The final message `row` of agent `s` (in room `room_id`) with text `text`, ending its turn on
/// input `input`, under the room's [`lock`] (SPEC §7.1.1). It speaks in the room when that turn
/// took an input from the room carrying a message from after the room's last "Stopped" (`POST
/// /stop`): the input that opened the turn, or one acknowledged as `joined` and stored between it
/// and `row` ([`took_room_input`]). In a turn on an input that already had a final message
/// ([`answered_before`]) only a post counts, as an aside. Otherwise a text that is exactly
/// `NO_REPLY` once trimmed is a pass, `system room_passed` ([`passed`]); a reply the runtime wrote
/// itself (`runtime`) is no post, and in a council, or from the speaker of a conversation, a
/// `room_stopped` line ([`runtime_line`]); any other is the agent's post there ([`post`]). An
/// answer an interruption cut off (`interrupted`: Stop in the agent's own chat, "No" to a
/// permission request) posts nothing: its turn is one without a post. It wakes nobody: in a
/// conversation the end of the speaker's turn moves the circle on ([`decide`]).
#[allow(clippy::too_many_arguments)]
pub async fn on_final(
    app: &AppState,
    s: &SessionRow,
    room_id: Uuid,
    input: Uuid,
    text: &str,
    runtime: bool,
    interrupted: bool,
    row: &EventRow,
) -> anyhow::Result<()> {
    if interrupted {
        return Ok(());
    }
    let _room = lock(app, room_id).await;
    let Some(room) = db::session(&app.db, room_id).await? else {
        return Ok(());
    };
    let Some(opening) = db::event_by_id(&app.db, input)
        .await?
        .filter(|i| i.session_id == s.id)
    else {
        return Ok(());
    };
    // A turn on the agent's own messages, or on a message from before the Stop (the part of the
    // turn the Stop cut included) without a later one joined: its answer stays in its session.
    if !took_room_input(app, s, &room, &opening, row).await? {
        return Ok(());
    }
    // A turn the CLI started itself on an input already answered: its post is an aside, and its
    // pass or its runtime's reply says nothing (a pass would count on the room's message, a
    // runtime's reply would stop the room).
    let aside = answered_before(app, s, &opening, row).await?;
    if aside && (runtime || text.trim() == NO_REPLY) {
        return Ok(());
    }
    if text.trim() == NO_REPLY {
        return passed(app, &room, s).await;
    }
    if runtime {
        // Another agent's in a conversation would stop the speaker's turn.
        if !is_conversation(&room) || speaker(app, room.id, i64::MAX).await? == Some(s.id) {
            runtime_line(app, &room, s, text).await?;
        }
        return Ok(());
    }
    post(app, &room, s, &opening, text, row, aside).await?;
    Ok(())
}

/// Whether input `opening` of agent `s` had a final message before `row`: the turn `row` ends is
/// one the CLI started itself after the turn that answered it (Claude Code after a background
/// task), which carries that turn's `in_reply_to` (SPEC §7.1.1).
async fn answered_before(
    app: &AppState,
    s: &SessionRow,
    opening: &EventRow,
    row: &EventRow,
) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM events
                        WHERE session_id = $1 AND type = 'agent.message'
                          AND payload ->> 'final' = 'true' AND payload ->> 'in_reply_to' = $2::text
                          AND seq < $3)",
    )
    .bind(s.id)
    .bind(opening.event_id)
    .bind(row.seq)
    .fetch_one(&app.db)
    .await?)
}

/// Whether the turn of agent `s` that `opening` opened and its final message `row` ends took an
/// input from room `room` whose message is from after the room's last "Stopped": `opening`
/// itself, or one acknowledged as `joined` (SPEC §17.2) and stored between `opening` and `row`.
async fn took_room_input(
    app: &AppState,
    s: &SessionRow,
    room: &SessionRow,
    opening: &EventRow,
    row: &EventRow,
) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM events i
             WHERE i.session_id = $1 AND i.type = 'peer.message'
               AND i.payload -> 'room' ->> 'session_id' = $2::text
               AND (i.event_id = $3
                    OR (i.payload ->> 'joined' = 'true' AND i.seq > $4 AND i.seq < $5))
               AND (i.payload -> 'room' ->> 'seq')::bigint >
                   COALESCE((SELECT max(seq) FROM events
                             WHERE session_id = $2 AND type = 'system'
                               AND payload ->> 'code' = $6 AND payload ->> 'text' = $7), 0))",
    )
    .bind(s.id)
    .bind(room.id)
    .bind(opening.event_id)
    .bind(opening.seq)
    .bind(row.seq)
    .bind(ROOM_STOPPED)
    .bind(STOPPED)
    .fetch_one(&app.db)
    .await?)
}

/// Agent `s`'s post in room `room` of its final message `row` with text `text`, ending the turn
/// `opening` opened ([`on_final`]): the files the agent attached between `opening` (or its
/// previous final message, when later) and `row`, as a reply to an ask has them
/// ([`peers::reply_files`], linked to the room), and the cap of the posting agent's mode its
/// `hqc_send` would carry (T39); with `aside`, `aside: true`: the circle of a conversation goes on
/// as if the post were not there ([`last_message`], [`decide`]). Its id is derived from `row`, so
/// a repeat stores nothing twice.
async fn post(
    app: &AppState,
    room: &SessionRow,
    s: &SessionRow,
    opening: &EventRow,
    text: &str,
    row: &EventRow,
    aside: bool,
) -> anyhow::Result<EventRow> {
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
    let files: Vec<Uuid> = peers::reply_files(
        app,
        s,
        room.id,
        opening.event_id,
        since.unwrap_or(0),
        row.seq,
        &[],
    )
    .await?
    .into_iter()
    .map(|f| f.artifact_id)
    .collect();
    // The lowest mode of the posting agent and up its turn's chain, as for its `hqc_send`;
    // `allow_all` holds nobody down. Kept with the post: it caps an input made from the post, and
    // one that carries the post in front ([`wake`]).
    let turn = turn_chain(app, s.id, opening).await?;
    let cap = match peers::chain_cap(Some(&turn), peers::sender_mode(s), &from) {
        (mode, Some(by)) => Some((mode, by)),
        (_, None) => None,
    };
    let mut payload = json!({"from_session_id": s.id, "from": from, "text": text, "mode": "send"});
    if !files.is_empty() {
        payload["artifact_ids"] = json!(files);
    }
    if aside {
        payload["aside"] = json!(true);
    }
    let id = once_id("room.post", room.id, &row.event_id.to_string());
    // The chain the agent's own `hqc_send` from this turn would carry (the limits aside: a post
    // is stored whatever it counts), so what the post wakes goes on in the turn's chain.
    // The owner's message that joined the turn is a person writing: the post goes on from the chain
    // of the input made from it, which that message started, not from the turn's older one.
    let turn = match owner_joined(app, s, room, opening, row).await? {
        Some(joined) => turn_chain(app, s.id, &joined).await?,
        None => turn,
    };
    let chain = peers::moved_on(Some(turn), false, peers::sender_mode(s), &from);
    let chain = made_chain(room, id, Some(chain), cap);
    store(app, room, id, s.id, payload, None, chain).await
}

/// The last input of agent `s` made from the owner's message in room `room` that joined the turn
/// `opening` opened, before its final message `row`.
async fn owner_joined(
    app: &AppState,
    s: &SessionRow,
    room: &SessionRow,
    opening: &EventRow,
    row: &EventRow,
) -> anyhow::Result<Option<EventRow>> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT event_id FROM events
         WHERE session_id = $1 AND type = 'peer.message' AND payload ->> 'joined' = 'true'
           AND payload -> 'room' ->> 'session_id' = $2::text
           AND payload ->> 'from_session_id' = $2::text AND seq > $3 AND seq < $4
         ORDER BY seq DESC LIMIT 1",
    )
    .bind(s.id)
    .bind(room.id)
    .bind(opening.seq)
    .bind(row.seq)
    .fetch_optional(&app.db)
    .await?;
    match id {
        Some(id) => Ok(db::event_by_id(&app.db, id).await?),
        None => Ok(None),
    }
}

/// `system room_passed` of agent `s` in room `room`.
async fn passed(app: &AppState, room: &SessionRow, s: &SessionRow) -> anyhow::Result<()> {
    events::system_with(
        app,
        room,
        json!({"text": format!("{} passed", s.title), "code": ROOM_PASSED,
               "from_session_id": s.id}),
    )
    .await?;
    Ok(())
}

/// A reply the runtime of agent `s` wrote itself instead of the model (an API error, "Credit
/// balance is too low"), `text`: no post, `system room_stopped` "<title>: <text>" in room `room`,
/// the text cut to [`MAX_RUNTIME_TEXT`] characters.
async fn runtime_line(
    app: &AppState,
    room: &SessionRow,
    s: &SessionRow,
    text: &str,
) -> anyhow::Result<()> {
    let text = crate::push::clip(text.trim(), MAX_RUNTIME_TEXT);
    let line = format!("{}: {text}", s.title);
    events::system(app, room, ROOM_STOPPED, &line).await?;
    // A council's round ends with the last turn ([`council_ended`]).
    if is_conversation(room) {
        round_ended(app, room, &line).await?;
    }
    Ok(())
}

/// The speaker `s`'s post `post` in conversation `room`, at the end of its turn ([`decide`]): it
/// wakes the next agent after `s`, with its files and cap, while at most [`MAX_HOPS`] posts follow
/// the owner's or a non-member's last message before it; past them the room gets `system
/// room_stopped` instead.
async fn after_post(
    app: &AppState,
    room: &SessionRow,
    s: &SessionRow,
    post: &EventRow,
) -> anyhow::Result<()> {
    // The owner's or a non-member's last message before this one, and the members' posts after it,
    // this one included.
    let (since, sender): (i64, Option<String>) = sqlx::query_as(&format!(
        "SELECT e.seq, e.payload ->> 'from' FROM events e
         WHERE e.session_id = $1 AND e.seq < $2 AND (e.type = 'user.message' OR ({NON_MEMBER}))
         ORDER BY e.seq DESC LIMIT 1"
    ))
    .bind(room.id)
    .bind(post.seq)
    .fetch_optional(&app.db)
    .await?
    .unwrap_or((0, None));
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM events
         WHERE session_id = $1 AND type = 'peer.message' AND seq <= $2 AND seq > $3
           AND payload ->> 'from_session_id' IN (SELECT id::text FROM sessions WHERE room_id = $1)",
    )
    .bind(room.id)
    .bind(post.seq)
    .bind(since)
    .fetch_one(&app.db)
    .await?;
    let max = i64::from(MAX_HOPS);
    if n > max {
        let whose = match sender {
            Some(from) => format!("{from}'s"),
            None => "your last".to_string(),
        };
        events::system(
            app,
            room,
            ROOM_STOPPED,
            &format!(
                "The agents posted {max} times since {whose} message and stopped waking each \
                 other; they go on when you write"
            ),
        )
        .await?;
        return round_ended(app, room, &format!("the agents posted {max} times")).await;
    }
    let members = db::room_members(&app.db, room.id).await?;
    match next_after(&members, s.id, None) {
        Some(next) => wake(app, room, std::slice::from_ref(next), post).await,
        None => Ok(()),
    }
}

/// After the speaker `passer` of conversation `room` passed, at the end of its turn
/// ([`decide`]): the last post, owner's message or non-member's of the room goes to the next agent
/// after the passer that is not its author (a non-member's has its sender for one, who is no
/// agent). The speaker is gone once every agent but that author passed ([`all_passed`]), so
/// somebody is left.
async fn pass_on(app: &AppState, room: &SessionRow, passer: Uuid) -> anyhow::Result<()> {
    let Some(last) = last_message(app, room.id, i64::MAX).await? else {
        return Ok(());
    };
    let members = db::room_members(&app.db, room.id).await?;
    match next_after(&members, passer, last.author) {
        Some(next) => wake(app, room, std::slice::from_ref(next), &last.event).await,
        None => Ok(()),
    }
}

/// The end of a turn of agent `s` of room `room` (SPEC §7.1.1), under the room's [`lock`]: in a
/// conversation that is not archived, when `s` is the speaker and none of its inputs from the
/// room still waits for its worker (`pending`), what it said since it became the speaker moves
/// the circle on: its latest post that is no aside wakes the next agent ([`after_post`]), else its
/// pass hands the room's last message on ([`pass_on`]), else the room stops, `system
/// room_stopped` "<title>'s turn <ended>; …". With an input still waiting, the turn that takes it
/// decides.
async fn decide(
    app: &AppState,
    room: &SessionRow,
    s: &SessionRow,
    ended: &str,
) -> anyhow::Result<()> {
    if room.archived {
        return Ok(());
    }
    if !is_conversation(room) {
        return council_ended(app, room).await;
    }
    let speaker = speaker(app, room.id, i64::MAX).await?;
    if speaker != Some(s.id) {
        // Nobody is left to speak: every agent but the last message's author passed.
        if speaker.is_none() {
            return quiet_end(app, room).await;
        }
        return Ok(());
    }
    let waiting: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM events
                        WHERE session_id = $1 AND type = 'peer.message' AND delivery = 'pending'
                          AND payload -> 'room' ->> 'session_id' = $2::text)",
    )
    .bind(s.id)
    .bind(room.id)
    .fetch_one(&app.db)
    .await?;
    if waiting {
        return Ok(());
    }
    // It became the speaker with its first input from the room since another agent was woken,
    // the room stopped or the owner wrote last: what it said before that is no answer to it.
    let since: Option<i64> = sqlx::query_scalar(
        "SELECT min((i.payload -> 'room' ->> 'seq')::bigint) FROM events i
         WHERE i.session_id = $2 AND i.type = 'peer.message'
           AND i.payload -> 'room' ->> 'session_id' = $1::text
           AND (i.payload -> 'room' ->> 'seq')::bigint >= GREATEST(
               (SELECT COALESCE(max((o.payload -> 'room' ->> 'seq')::bigint), 0)
                FROM events o JOIN sessions m ON m.id = o.session_id
                WHERE m.room_id = $1 AND o.session_id <> $2 AND o.type = 'peer.message'
                  AND o.payload -> 'room' ->> 'session_id' = $1::text),
               (SELECT COALESCE(max(seq), 0) FROM events
                WHERE session_id = $1
                  AND (type = 'user.message'
                       OR (type = 'peer.message'
                           AND payload ->> 'from_session_id' NOT IN
                               (SELECT id::text FROM sessions WHERE room_id = $1))
                       OR (type = 'system' AND payload ->> 'code' = $3))))",
    )
    .bind(room.id)
    .bind(s.id)
    .bind(ROOM_STOPPED)
    .fetch_one(&app.db)
    .await?;
    let Some(since) = since else {
        return Ok(());
    };
    let post: Option<Uuid> = sqlx::query_scalar(
        "SELECT event_id FROM events
         WHERE session_id = $1 AND type = 'peer.message'
           AND payload ->> 'from_session_id' = $2::text AND seq > $3 AND NOT (payload ? 'aside')
         ORDER BY seq DESC LIMIT 1",
    )
    .bind(room.id)
    .bind(s.id)
    .bind(since)
    .fetch_optional(&app.db)
    .await?;
    if let Some(post) = post {
        return match db::event_by_id(&app.db, post).await? {
            Some(post) => after_post(app, room, s, &post).await,
            None => Ok(()),
        };
    }
    let passed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM events
                        WHERE session_id = $1 AND type = 'system' AND payload ->> 'code' = $3
                          AND payload ->> 'from_session_id' = $2::text AND seq > $4)",
    )
    .bind(room.id)
    .bind(s.id)
    .bind(ROOM_PASSED)
    .bind(since)
    .fetch_one(&app.db)
    .await?;
    if passed {
        return pass_on(app, room, s.id).await;
    }
    let line = format!(
        "{}'s turn {ended}; the agents go on when you write",
        s.title
    );
    events::system(app, room, ROOM_STOPPED, &line).await?;
    round_ended(app, room, &line).await
}

/// Input `row` of agent `session_id`, acknowledged without opening a turn (SPEC §17.2): joined to
/// the turn running then, or acked after the turn that answers it ended (an ack lost with the
/// link and sent again after the worker's outbox, §5.2). That turn's end decided nothing while
/// the input waited; under the room's [`lock`], when the agent has no turn open now, it is decided
/// as at the end of a turn ([`decide`]). Any other input is no room's.
pub async fn acked(app: &AppState, session_id: Uuid, row: &EventRow) -> anyhow::Result<()> {
    let Some(room_id) = row.payload["room"]["session_id"]
        .as_str()
        .and_then(|v| v.parse::<Uuid>().ok())
    else {
        return Ok(());
    };
    let _room = lock(app, room_id).await;
    let (Some(s), Some(room)) = (
        db::session(&app.db, session_id).await?,
        db::session(&app.db, room_id).await?,
    ) else {
        return Ok(());
    };
    if s.turn_open {
        return Ok(());
    }
    decide(app, &room, &s, ENDED).await
}

/// Status `idle` or `error` of agent `s` of room `room_id`, the end of its turn, under the room's
/// [`lock`]: [`decide`].
pub async fn turn_ended(app: &AppState, s: &SessionRow, room_id: Uuid) -> anyhow::Result<()> {
    let _room = lock(app, room_id).await;
    let Some(room) = db::session(&app.db, room_id).await? else {
        return Ok(());
    };
    decide(app, &room, s, ENDED).await
}

/// The open turn of agent `s` was lost for `why` (its process stopped, the worker restarted, the
/// watchdog; SPEC §17.3), under its room's [`lock`]: [`decide`], the line naming the loss.
pub async fn turn_lost(app: &AppState, s: &SessionRow, why: &str) -> anyhow::Result<()> {
    let Some(room_id) = s.room_id else {
        return Ok(());
    };
    let _room = lock(app, room_id).await;
    let Some(room) = db::session(&app.db, room_id).await? else {
        return Ok(());
    };
    decide(app, &room, s, &format!("was lost ({why})")).await
}

/// Input `row` of agent `s`, failed for `reason` (SPEC §5.2), under the room's [`lock`]: when it
/// came from a room, the room says so with `system room_stopped`.
pub async fn input_failed(
    app: &AppState,
    s: &SessionRow,
    row: &EventRow,
    reason: &str,
) -> anyhow::Result<()> {
    if row.kind != "peer.message" {
        return Ok(());
    }
    let Some(room_id) = row.payload["room"]["session_id"]
        .as_str()
        .and_then(|v| v.parse::<Uuid>().ok())
    else {
        return Ok(());
    };
    let _room = lock(app, room_id).await;
    let Some(room) = db::session(&app.db, room_id).await? else {
        return Ok(());
    };
    let line = format!("{} did not get the message: {reason}", s.title);
    events::system(app, &room, ROOM_STOPPED, &line).await?;
    if is_conversation(&room) {
        round_ended(app, &room, &line).await
    } else {
        council_ended(app, &room).await
    }
}

/// The end of a conversation's round for want of a speaker: every agent but the author of the
/// room's last message passed ([`all_passed`]), and that message, the owner's or a non-member's
/// too, ends with them ([`round_ended`]). Under the room's [`lock`].
async fn quiet_end(app: &AppState, room: &SessionRow) -> anyhow::Result<()> {
    let Some(last) = last_message(app, room.id, i64::MAX).await? else {
        return Ok(());
    };
    let members = db::room_members(&app.db, room.id).await?;
    if members.is_empty() || !all_passed(&members, &last) {
        return Ok(());
    }
    let reason = match members.iter().find(|m| Some(m.id) == last.author) {
        Some(author) => {
            // The name as the reason ends up ([`round_ended`] cleans it), the address when that
            // is nothing.
            let name = clean_name(&author.title);
            let name = if name.is_empty() {
                peers::address(app, author).await?
            } else {
                name
            };
            format!("every agent but {name} passed")
        }
        None => "every agent passed".to_string(),
    };
    round_ended(app, room, &reason).await
}

/// The end of a council's round: no agent has a turn open or an input from the room waiting for
/// its worker, so every agent's turn on the message has ended, each having posted, passed,
/// stopped or been lost ([`round_ended`]). Under the room's [`lock`].
async fn council_ended(app: &AppState, room: &SessionRow) -> anyhow::Result<()> {
    let busy: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM sessions m
             WHERE m.room_id = $1 AND NOT m.archived
               AND (m.turn_open OR EXISTS (
                    SELECT 1 FROM events i
                    WHERE i.session_id = m.id AND i.type = 'peer.message'
                      AND i.delivery = 'pending'
                      AND i.payload -> 'room' ->> 'session_id' = $1::text)))",
    )
    .bind(room.id)
    .fetch_one(&app.db)
    .await?;
    if busy {
        return Ok(());
    }
    round_ended(app, room, "every agent answered").await
}

/// A round of room `room` ended for `reason`: every session that is none of its agents and wrote
/// in it, the first message of the session that made it with `hqc_room` or an `hqc_send` into it,
/// gets a `peer.message` from the room with how it ended and the last two entries that came in
/// the room since its previous notice or its first message (SPEC §7.1.1), as a send of the room
/// wakes it (`delivery: pending`; [`notify`]). The owner, a person who reads the feed, gets none,
/// nor a session that is archived or gone. The notice is stored under the room's [`lock`], which
/// the caller holds, with the end that caused it, so one end tells a session once. Its chain is
/// the last post's moved on ([`peers::moved_on`]), else that of the session's message, and holds
/// whatever the limits; it is capped by every entry since the boundary, the left-out ones too, as
/// an agent's input is by the posts in front of it ([`entries_cap`]). The reason is cleaned like
/// a name ([`clean_name`]).
pub async fn round_ended(app: &AppState, room: &SessionRow, reason: &str) -> anyhow::Result<()> {
    let senders: Vec<Uuid> = sqlx::query_scalar(&format!(
        "SELECT sender FROM (
             SELECT (e.payload ->> 'from_session_id')::uuid AS sender, min(e.seq) AS first
             FROM events e WHERE e.session_id = $1 AND {NON_MEMBER} GROUP BY 1) w
         ORDER BY first"
    ))
    .bind(room.id)
    .fetch_all(&app.db)
    .await?;
    if senders.is_empty() {
        return Ok(());
    }
    let address = peers::address(app, room).await?;
    let notice_seq: i64 =
        sqlx::query_scalar("SELECT COALESCE(max(seq), 0) FROM events WHERE session_id = $1")
            .bind(room.id)
            .fetch_one(&app.db)
            .await?;
    // One line with no `|`, `[`, `]` or `"` and no hub mark in it: the reason carries agents'
    // titles.
    let reason = escape(&clean_name(reason));
    for id in senders {
        let Some(sender) = db::session(&app.db, id).await?.filter(|s| !s.archived) else {
            continue;
        };
        notify(app, room, &sender, &address, &reason, notice_seq).await?;
    }
    Ok(())
}

/// The notice of [`round_ended`] to `sender`, when it is due: everything in the room after its
/// previous notice (`notice_seq`), or, never told, after its first message there (that message
/// itself left out), of which the notice shows the last two, oldest first, as an agent's block
/// has it ([`room_block`]), its own messages under "message from you" ([`notice_text`]). Due
/// when anything came since; a session never told gets the reason alone when nothing did.
async fn notify(
    app: &AppState,
    room: &SessionRow,
    sender: &SessionRow,
    address: &str,
    reason: &str,
    notice_seq: i64,
) -> anyhow::Result<()> {
    // Its first message in the room, and its latest with the chain it carried.
    let first: i64 = sqlx::query_scalar(
        "SELECT min(seq) FROM events
         WHERE session_id = $1 AND type = 'peer.message' AND payload ->> 'from_session_id' = $2::text",
    )
    .bind(room.id)
    .bind(sender.id)
    .fetch_one(&app.db)
    .await?;
    let latest: Option<Value> = sqlx::query_scalar(
        "SELECT chain FROM events
         WHERE session_id = $1 AND type = 'peer.message' AND payload ->> 'from_session_id' = $2::text
         ORDER BY seq DESC LIMIT 1",
    )
    .bind(room.id)
    .bind(sender.id)
    .fetch_one(&app.db)
    .await?;
    let previous: i64 = sqlx::query_scalar(
        "SELECT COALESCE(max((payload ->> 'notice_seq')::bigint), 0) FROM events
         WHERE session_id = $1 AND type = 'peer.message' AND payload ->> 'from_session_id' = $2::text
           AND payload ? 'notice_seq'",
    )
    .bind(sender.id)
    .bind(room.id)
    .fetch_one(&app.db)
    .await?;
    let never_told = previous == 0;
    let entries: Vec<BlockEntry> = sqlx::query_as(&format!(
        "SELECT e.type = 'user.message' AS owner, ({NON_MEMBER}) AS outsider,
                COALESCE(e.payload ->> 'from_session_id' = $2::text, false) AS mine,
                COALESCE(e.payload ->> 'text', '') AS text,
                COALESCE(e.payload ->> 'from', '?') AS address, a.title AS name, e.chain,
                e.payload -> 'artifact_ids' AS files
         FROM events e
         LEFT JOIN sessions a ON a.id::text = e.payload ->> 'from_session_id'
         WHERE e.session_id = $1 AND e.type IN ('peer.message', 'user.message') AND e.seq > $3
         ORDER BY e.seq"
    ))
    .bind(room.id)
    .bind(sender.id)
    .bind(if never_told { first } else { previous })
    .fetch_all(&app.db)
    .await?;
    if entries.is_empty() && !never_told {
        return Ok(());
    }
    let text = notice_text(address, reason, &entries);
    // The last post's chain moved on, else its message's; the limits aside, as a post is stored.
    let posts = || entries.iter().filter(|e| !e.owner && !e.outsider);
    let last = posts().next_back().and_then(|p| p.chain.clone()).or(latest);
    let chain = last
        .and_then(|c| serde_json::from_value(c).ok())
        .map(|c| peers::moved_on(Some(c), false, PermissionMode::AllowAll, address));
    let id = once_id(
        "room.notice",
        sender.id,
        &format!("{}:{notice_seq}", room.id),
    );
    let payload = json!({
        "from_session_id": room.id,
        "from": address,
        "text": text,
        "mode": "send",
        "notice_seq": notice_seq,
    });
    // Everything the notice carries but the owner's messages can bring a cap: another session's
    // message too, which the reader would run as its own mode. The reader's own message only
    // when another session capped it (it was written in a turn that session started); its cap by
    // the reader's own mode is none to the reader.
    let me = peers::address(app, sender).await?;
    let capped = entries.iter().filter(|e| {
        !e.owner
            && (!e.mine
                || e.chain
                    .as_ref()
                    .and_then(|c| c.get("capped_by"))
                    .and_then(Value::as_str)
                    .is_some_and(|by| by != me))
    });
    let chain = made_chain(sender, id, chain, entries_cap(sender, capped));
    store(app, sender, id, room.id, payload, Some("pending"), chain).await?;
    delivery::kick(app, sender.id, false).await;
    Ok(())
}

/// The text of a notice of room `address`: the line with `reason`, then the last [`NOTICE_ENTRIES`]
/// of `entries` under the lines of an agent's block, from the newest back while they fit the limit
/// of a text of `hqc_send` ([`MAX_ARGS`]); the others are left out, their count in the first line
/// with a pointer to `hqc_read`. The newest alone over the limit is cut instead.
fn notice_text(address: &str, reason: &str, entries: &[BlockEntry]) -> String {
    let part = |e: &BlockEntry, text: &str| {
        let from = if e.owner {
            "message from your owner".to_string()
        } else if e.mine {
            "message from you".to_string()
        } else if e.outsider {
            format!(
                "message from {}",
                named(e.name.as_deref(), &escape(&e.address))
            )
        } else {
            format!("post from {}", named(e.name.as_deref(), &e.address))
        };
        format!("[hqc room {address} | {from}]\n{}\n\n", escape(text))
    };
    let head = |left_out: usize| match left_out {
        0 => format!("[hqc room {address} | the discussion ended: {reason}]\n\n"),
        1 => format!(
            "[hqc room {address} | the discussion ended: {reason} | 1 earlier entry left out, \
             read it with hqc_read]\n\n"
        ),
        n => format!(
            "[hqc room {address} | the discussion ended: {reason} | {n} earlier entries left \
             out, read them with hqc_read]\n\n"
        ),
    };
    let budget = MAX_ARGS.saturating_sub(head(entries.len()).len());
    let (mut kept, mut size) = (Vec::new(), 0);
    for e in entries.iter().rev().take(NOTICE_ENTRIES) {
        let p = part(e, &e.text);
        if size + p.len() > budget {
            if kept.is_empty() {
                // As many characters as their UTF-8 bytes fit what is left (the `…` takes three);
                // halved while the escaped text still overflows.
                let left = budget.saturating_sub(part(e, "").len()).saturating_sub(3);
                let mut n = e
                    .text
                    .char_indices()
                    .map(|(i, c)| i + c.len_utf8())
                    .take_while(|&end| end <= left)
                    .count();
                let mut p = part(e, &crate::push::clip(&e.text, n));
                while p.len() > budget && n > 0 {
                    n /= 2;
                    p = part(e, &crate::push::clip(&e.text, n));
                }
                kept.push(p);
            }
            break;
        }
        size += p.len();
        kept.push(p);
    }
    kept.reverse();
    format!("{}{}", head(entries.len() - kept.len()), kept.concat())
        .trim_end()
        .to_string()
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
    use super::{clean_name, escape};

    #[test]
    fn escapes_the_mark_in_any_case() {
        assert_eq!(escape("ünï [hqc room ✓"), "ünï [hqc-room-text ✓");
        assert_eq!(escape("[[HQC Room"), "[[hqc-room-text");
        assert_eq!(escape("[hqc roo"), "[hqc roo");
        for lookalike in [
            "［hqc room",
            "[hqc\u{3000}room",
            "[hqc\u{a0}room",
            "[hqc  room",
            "[hq\u{200b}c room",
            "[\u{202e}hqc room",
            "[ｈｑｃ ｒｏｏｍ",
            "[hq\u{34f}c room",
            "[hqc\u{fe0f} room",
            "[hqc\u{3164}room",
        ] {
            assert_eq!(escape(&format!("a {lookalike} b")), "a [hqc-room-text b");
        }
    }

    #[test]
    fn a_name_has_no_lookalike_of_a_hub_character() {
        assert_eq!(clean_name("a［b］｜＂c"), "a b c");
        assert_eq!(clean_name("a\u{200b}b\u{202e}c"), "abc");
        assert_eq!(clean_name("a\u{34f}b\u{fe0f}c"), "abc");
        assert_eq!(clean_name("  Opt\u{3000}imist \n "), "Opt imist");
        assert_eq!(clean_name("a\u{3164}b"), "a b");
        assert_eq!(clean_name("\u{3164}\u{ffa0}"), "");
        assert_eq!(clean_name("a|b[c]d\"e"), "a b c d e");
    }
}
