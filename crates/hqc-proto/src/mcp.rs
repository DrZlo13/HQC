//! The hqc MCP tools (SPEC §7): the list `hqc-worker mcp` gives in `tools/list` and the hub
//! implements behind `mcp.call` (SPEC §5.2). A tool appears here only once its task lands
//! (docs/ITERATION-3.md): no stubs.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

/// One tool as `tools/list` shows it.
#[derive(Debug, Clone, Copy)]
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema of the arguments (`inputSchema`).
    pub input_schema: fn() -> Value,
}

impl Tool {
    /// The `tools/list` entry.
    pub fn listing(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": (self.input_schema)(),
        })
    }
}

pub const LIST_SESSIONS: &str = "hqc_list_sessions";
pub const ATTACH: &str = "hqc_attach";
pub const SEND: &str = "hqc_send";
pub const ASK: &str = "hqc_ask";
pub const SEARCH_HISTORY: &str = "hqc_search_history";
pub const SPAWN: &str = "hqc_spawn";
pub const SCHEDULE: &str = "hqc_schedule";
pub const MOVE: &str = "hqc_move";
pub const RENAME: &str = "hqc_rename";
/// `instructions` of `initialize` (SPEC §7.2): the runtime keeps them in the system prompt while
/// the tools themselves wait behind its tool search, so each tool is named here by its use.
pub const INSTRUCTIONS: &str = "hqc tools act with the rights of this session's owner; the user \
    reads this session in the hqc chat. Required in every session, also for a one-line question \
    and a task from another session: the moment you understand what the work in this session \
    is, usually on the first message and before you answer, call hqc_rename without `session` \
    with a short title for that work; call it again whenever the work turns to something \
    substantially different. Rename this session always without `session`, also when you think \
    you know its address: a copied session carries the history of its source, and an address of \
    this session seen there (an hqc_rename result, `current: true` in hqc_list_sessions) is the \
    source's. Required as well: every file you make for the user to look at (an \
    image, a chart, a report, a diff) goes into the chat with hqc_attach in the same answer, also \
    when the user did not ask to see it; a path alone is not enough. hqc_attach takes only a file \
    inside the session's workspace, so copy a file you made elsewhere (a scratchpad in /tmp) into \
    .hqc/out/ of the workspace first, not elsewhere in it: hqc keeps .hqc/ out of git. \
    Other sessions: hqc_list_sessions, hqc_send, hqc_ask, hqc_spawn; history: \
    hqc_search_history; later or recurring messages: hqc_schedule; moving this session to \
    another folder or project, only when the user asks (a folder that is not one of the user's \
    workspaces in the same environment waits for the user to allow it): hqc_move; renaming a \
    session: hqc_rename.";

pub const TOOLS: &[Tool] = &[
    Tool {
        name: LIST_SESSIONS,
        description: "List the hqc sessions of your owner: each with its address \
        (<project-slug>/<session-slug>), title and status; `current: true` marks this \
        session. Give `project` (a project slug or id) to list one project only.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "A project slug or id; all projects when absent."
                    }
                },
                "additionalProperties": false
            })
        },
    },
    Tool {
        name: ATTACH,
        description: "Show a file from your workspace to the user in the hqc chat: an image \
            inline, a diff as a diff, anything else as a file to open or download. At most \
            100 MB. `path` is absolute or relative to your working directory and must lie \
            inside the session's workspace. The result's `artifact_id` can go in `files` of \
            hqc_send and hqc_ask; a file attached during a turn that answers an hqc_ask goes \
            back to the asker with the reply (so do the files of an hqc_send to the asker \
            from that turn).",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file, absolute or relative to the working directory."
                    },
                    "name": {
                        "type": "string",
                        "description": "The name shown in the chat; the file's name when absent."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["file", "image", "diff", "html", "text"],
                        "description": "How the chat shows it; guessed from the name when absent."
                    },
                    "caption": {
                        "type": "string",
                        "description": "A line shown with the file."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            })
        },
    },
    Tool {
        name: SEND,
        description: "Send a message to another hqc session of your owner without waiting for a \
            reply: it starts a turn there (after the turn running now, if any), and the other \
            agent may answer you later with hqc_send, which starts a turn here. When you need \
            its answer or a file back to go on, use hqc_ask instead. Do not send only to thank \
            or acknowledge: every send costs the other session a turn. `session` is an address \
            from hqc_list_sessions (<project-slug>/<session-slug>) or a session id. To hand \
            over files, list them in `files` (paths in your workspace, at most 100 MB each, or \
            artifact ids from hqc_attach); never paste a file's content into `text`. The other \
            agent gets each file as a local path on its own machine. While you answer an \
            hqc_ask, a send to the asker goes back with your reply (its text after your final \
            message, its files with the reply's files; `delivery: with_reply`).",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "The message."},
                    "session": {
                        "type": "string",
                        "description": "The target session: <project-slug>/<session-slug> or its id."
                    },
                    "files": files_schema()
                },
                "required": ["text", "session"],
                "additionalProperties": false
            })
        },
    },
    Tool {
        name: ASK,
        description: "Ask another hqc session of your owner and wait for its answer: the \
            message starts a turn there, and that turn's final message comes back as this \
            tool's result (`reply`). Waits up to `timeout_s` (at most and by default 600 s). \
            Not to your own session. `session` is an address from hqc_list_sessions \
            (<project-slug>/<session-slug>) or a session id. To hand over files, list them in \
            `files` (paths in your workspace, at most 100 MB each, or artifact ids from \
            hqc_attach); never paste a file's content into `text`. The other agent gets each \
            file as a local path; the files it attaches with hqc_attach or sends to you with \
            hqc_send while answering come back in the result's `files`, each with a local \
            `path` on this machine (or an `error`), and the text of such a send follows its \
            final message in `reply`.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "The question or task."},
                    "session": {
                        "type": "string",
                        "description": "The target session: <project-slug>/<session-slug> or its id."
                    },
                    "timeout_s": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "How long to wait for the reply, in seconds."
                    },
                    "files": files_schema()
                },
                "required": ["text", "session"],
                "additionalProperties": false
            })
        },
    },
    Tool {
        name: SEARCH_HISTORY,
        description: "Search the history of your owner's hqc sessions (archived ones too): \
            the messages of users and agents, messages between sessions, hqc's notices, \
            answers to questions, tool calls with their arguments, tool output and session \
            titles. A word finds its other forms (\"deploy\" finds \"deployed\", \"переезд\" \
            finds \"переезда\"); all words must be there; \"quoted phrase\", `or`, `-word` \
            (a dash before four letters or more, or before a quoted phrase, excludes it), \
            `word*` for a prefix; `fuzzy: true` also finds typos. A command pasted with its \
            flags finds its call: `-rf`, `-lah`, `--force` are words. Best match first, \
            20 per call; pass `next_cursor` as `cursor` for more. Each hit has the session's \
            address, the event's `seq` and a snippet.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to look for."},
                    "project": {
                        "type": "string",
                        "description": "A project slug or id; all projects when absent."
                    },
                    "fuzzy": {
                        "type": "boolean",
                        "description": "Similar words too (at least 3 characters)."
                    },
                    "cursor": {
                        "type": "string",
                        "description": "`next_cursor` of the previous call."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            })
        },
    },
    Tool {
        name: SPAWN,
        description: "Start a new hqc session of your owner and give it its first task: \
            `prompt` reaches the new agent as a message from this session and starts its first \
            turn; it can answer you with hqc_send. Returns the new session's address at once \
            and does not wait for its work. The session runs in a workspace of `project` (the \
            project's default workspace when `workspace` is absent) on that workspace's \
            machine, with your model unless `model` is given. With `isolation: \"worktree\"` \
            the new session works in a git worktree of that workspace of its own, on a new \
            branch (hqc/<session-slug> unless `branch` is given), so it does not edit the \
            checkout you work in; the call then waits until the worktree is made and also \
            returns its `worktree` path and branch. The new agent's commits stay on that \
            branch for your owner to merge; the worktree is not deleted automatically.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": "A project slug or id."},
                    "workspace": {
                        "type": "string",
                        "description": "A workspace of the project: its id, label or path."
                    },
                    "runtime": {
                        "type": "string",
                        "enum": ["claude_code"],
                        "description": "The agent's runtime; claude_code is the only one so far."
                    },
                    "model": {"type": "string", "description": "The new agent's model."},
                    "prompt": {"type": "string", "description": "The new agent's first task."},
                    "isolation": {
                        "type": "string",
                        "enum": ["worktree"],
                        "description": "worktree: a new git worktree of the workspace for the new session."
                    },
                    "branch": {
                        "type": "string",
                        "description": "With isolation: the worktree's new branch; hqc/<session-slug> when absent."
                    }
                },
                "required": ["project", "prompt"],
                "additionalProperties": false
            })
        },
    },
    Tool {
        name: SCHEDULE,
        description: "Schedule a message into an hqc session of your owner (this one too): the \
            hub sends `message` there as your owner's input, once at `at` or repeatedly by \
            `cron`, and the session's agent gets it as a new turn. Give exactly one of `at` (an \
            RFC 3339 time with an offset, e.g. 2026-10-26T09:00:00+01:00) and `cron` (5 fields: \
            minute hour day-of-month month day-of-week, at most every 5 minutes) in `timezone` \
            (IANA, e.g. Europe/Berlin; your owner's zone when absent). A run missed while the \
            hub was down is skipped unless `catch_up` is true. The owner sees, pauses and \
            deletes schedules in the hqc client.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "session": {
                        "type": "string",
                        "description": "The target session: <project-slug>/<session-slug> or its id."
                    },
                    "message": {"type": "string", "description": "The text to send."},
                    "at": {
                        "type": "string",
                        "description": "When to send it once: an RFC 3339 time with an offset."
                    },
                    "cron": {
                        "type": "string",
                        "description": "When to send it repeatedly: a 5-field cron pattern."
                    },
                    "timezone": {
                        "type": "string",
                        "description": "The IANA time zone of `cron`; your owner's when absent."
                    },
                    "catch_up": {
                        "type": "boolean",
                        "description": "Send a run missed while the hub was down once, late."
                    }
                },
                "required": ["session", "message"],
                "additionalProperties": false
            })
        },
    },
    Tool {
        name: MOVE,
        description: "Move this session to another folder on the same machine. Call it only \
            when the user asks you to (\"move this into project foo\"). `path` becomes this \
            session's working folder, in `project` when given, otherwise in a new project named \
            after the folder; a folder that is a workspace already comes with its project. Into \
            one of the user's workspaces in the environment of your folder now the session moves \
            at once; any other folder (a new one, one that is no workspace yet, one in another \
            environment) waits until the user allows it in the chat, and the call fails if the \
            user denies it or does not answer before the tool call times out. A missing folder is created once allowed; it must lie inside the \
            machine's allowed roots and neither inside nor around another workspace. Returns \
            `{address, project, path}`: \
            your new address, the project's slug and the folder's canonical path. After the \
            call, end your turn: from the next turn you work in `path`, the conversation goes \
            on, and your address is now `address`.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The folder's absolute path on this machine; created when missing, once the user allows it."
                    },
                    "project": {
                        "type": "string",
                        "description": "A project slug or id; a new project named after the folder when absent."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            })
        },
    },
    Tool {
        name: RENAME,
        description: "Rename an hqc session of your owner: this one when `session` is absent, \
            otherwise the session at that address (<project-slug>/<session-slug>) or id. Leave \
            `session` out to rename this one, also when you think you know its address: a \
            copied session carries the history of its source, and an address of this session \
            seen there (an hqc_rename result, `current: true` in hqc_list_sessions) is the \
            source's. The \
            title is set as the user sets it in the client: the session list and the chat show \
            it, and the session's automatic title no longer replaces it. The address does not \
            change. One line, at most 200 characters.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "description": "The new title."},
                    "session": {
                        "type": "string",
                        "description": "Another session to rename: <project-slug>/<session-slug> or its id. Leave it out to rename this session."
                    }
                },
                "required": ["title"],
                "additionalProperties": false
            })
        },
    },
];

/// The `files` argument of [`SEND`] and [`ASK`] (since 0.46, T41).
fn files_schema() -> Value {
    json!({
        "type": "array",
        "items": {"type": "string"},
        "maxItems": MAX_FILES,
        "description": "Files to hand over: paths (absolute or relative to your working \
            directory, inside the session's workspace) or artifact ids from hqc_attach."
    })
}

/// Most files one message carries (the hub's cap on an input's artifacts, SPEC §20).
pub const MAX_FILES: usize = 32;

/// One file of an [`ASK`] reply as the hub sends it in `result.files` (since 0.46, T41): an
/// artifact the target attached during its answering turn. The asking worker downloads it
/// into its own inbox and gives the agent a [`ReplyFile`] instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplyArtifact {
    pub artifact_id: Uuid,
    pub name: String,
    pub kind: crate::event::ArtifactKind,
}

/// One file of an [`ASK`] reply as the agent gets it: the local `path` of the download, or
/// the `error` that stopped it. `artifact_id` (T51): the hub's copy, which the asker's client
/// opens and which the agent may pass on in `files`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplyFile {
    pub artifact_id: Uuid,
    pub name: String,
    pub kind: crate::event::ArtifactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `args` of the `mcp.call` for [`ATTACH`]: the worker reads and uploads `path` itself
/// (`PUT /worker/artifacts`, SPEC §12) and sends the artifact instead of the path; `name`
/// and `kind` went with the upload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachCall {
    pub artifact_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

pub fn tool(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}

/// Largest `mcp.call.args`, serialized, in bytes (SPEC §20). The worker refuses a bigger
/// call before sending it; the hub checks again.
pub const MAX_ARGS: usize = 64 * 1024;

/// `McpError.code` values (SPEC §7); the list is open, an unknown code reads as `internal`.
pub mod error_code {
    /// The session token is unknown, or belongs to a session another worker runs.
    pub const UNAUTHORIZED: &str = "unauthorized";
    /// The tool is not in [`super::TOOLS`] of the hub.
    pub const UNKNOWN_TOOL: &str = "unknown_tool";
    /// The arguments do not fit the tool's schema or [`super::MAX_ARGS`].
    pub const INVALID_ARGS: &str = "invalid_args";
    /// A project, session or other target the owner does not have.
    pub const NOT_FOUND: &str = "not_found";
    /// The worker lost the call: the session's agent stopped, or the hub refused the frame.
    pub const UNAVAILABLE: &str = "unavailable";
    pub const INTERNAL: &str = "internal";
    /// `hqc_ask` got no reply within its timeout (SPEC §7.1).
    pub const TIMEOUT: &str = "timeout";
    /// A chain limit of SPEC §7.1 (`depth`, `hops`): only a human's turn goes on.
    pub const LIMIT_EXCEEDED: &str = "limit_exceeded";
    /// The worker restarted while `hqc_ask` waited: the blocking call died with it.
    pub const WORKER_RESTART: &str = "worker_restart";
}

/// `mcp.cancel.reason` values (SPEC §7.2).
pub mod cancel_reason {
    /// The runtime sent `notifications/cancelled` (e.g. its tool timeout).
    pub const CANCELLED: &str = "cancelled";
    /// The MCP process exited (EOF on its stdio) with the call open.
    pub const EOF: &str = "eof";
    pub const ALL: &[&str] = &[CANCELLED, EOF];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_are_listed_with_object_schemas() {
        let mut names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
        names.dedup();
        assert_eq!(names.len(), TOOLS.len(), "unique names");
        for t in TOOLS {
            assert!(t.name.starts_with("hqc_"), "{}", t.name);
            let l = t.listing();
            assert_eq!(l["inputSchema"]["type"], "object", "{}", t.name);
            assert!(!t.description.is_empty());
        }
        assert!(tool(LIST_SESSIONS).is_some());
        assert!(tool(SCHEDULE).is_some());
        assert!(tool(SPAWN).is_some());
        assert!(tool(ATTACH).is_some());
        assert!(tool(SEND).is_some() && tool(ASK).is_some());
        assert!(tool(SEARCH_HISTORY).is_some());
        assert!(tool(MOVE).is_some());
        assert!(tool(RENAME).is_some());
    }

    #[test]
    fn instructions_name_every_tool() {
        for t in TOOLS {
            assert!(INSTRUCTIONS.contains(t.name), "{}", t.name);
        }
    }
}
