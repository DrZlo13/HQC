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
pub const READ: &str = "hqc_read";
pub const SPAWN: &str = "hqc_spawn";
pub const ROOM: &str = "hqc_room";
pub const SCHEDULE: &str = "hqc_schedule";
pub const MOVE: &str = "hqc_move";
pub const PUBLISH: &str = "hqc_publish";
pub const RENAME: &str = "hqc_rename";
/// Since SPEC 0.147, listed only to an agent whose worker has `session_secrets`
/// ([`SECRET_TOOLS`]).
pub const SECRET: &str = "hqc_secret";
pub const SSH_KEYGEN: &str = "hqc_ssh_keygen";
/// The tools the hqc MCP server lists only with `HQC_SECRET_TOOLS=1` in its environment
/// (SPEC §7, since 0.147).
pub const SECRET_TOOLS: &[&str] = &[SECRET, SSH_KEYGEN];
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
    Required as well: when the user's latest message asks for a task that is not part of the \
    work of this session (not a question, a quick check or one small edit), ask the user before \
    any work on it, with your tool for questions to the user (AskUserQuestion), \"Start this in \
    its own session?\" with the options \"New session\" and \"Do it here\"; on \"New session\" \
    start it with hqc_spawn as its description says and do not do it here, on \"Do it here\" do \
    it here. Do not ask on a message from another session or a room (an `[hqc peer` or \
    `[hqc room` header): that session or room gave the task and chose where it runs. \
    Required as well: when the user asks how a subagent you started is doing, what it has done \
    or what it is thinking, and when you report its progress yourself, read it with hqc_read, \
    `subagent` being the `agentId` from the result of your Agent call, with `thinking: true` for \
    its thinking (`later` of an answer reads what it did since); do not judge it by the files it \
    changed. The result of a background Agent call says to answer only that the agent is still \
    running, since its output file would overflow your context: hqc_read reads its steps page \
    by page instead, and the `agentId` that result calls internal is what hqc_read takes. \
    Other sessions: hqc_list_sessions, hqc_send, hqc_ask, hqc_spawn, hqc_room (a room of several \
    agents); history: hqc_search_history to find, hqc_read to read a session or a room in full, or \
    what a subagent is doing; later or recurring messages: hqc_schedule; moving this session to \
    another folder or project, only when the user asks (a folder that is not one of the user's \
    workspaces in the same environment waits for the user to allow it): hqc_move; publishing a \
    page or a file at a public link anyone can open, only when the user asks (the user allows it \
    in the chat first), then give the link in your answer: hqc_publish; renaming a session: \
    hqc_rename; where this machine allows it, a password or an ssh key from the user without it \
    entering the conversation: hqc_secret, and an ssh key of your own for a device you set up: \
    hqc_ssh_keygen.";

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
            message, its files with the reply's files; `delivery: with_reply`). `session` may \
            be a room too (a session with several agents): the message goes to its agents, \
            and their answers stay in the room; when the room's discussion ends, the room \
            sends you how it ended and its last two entries (read the rest with hqc_read), \
            which starts a turn here, so do not poll the room.",
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
        name: READ,
        description: "Read one hqc session of your owner from the hub, in full, page by page: \
            any session of your owner, archived ones, a room and this session itself. You get \
            the events as the hqc chat shows them: the messages of users and agents, messages \
            between sessions, a room's posts, tool calls with their output, and hqc's own \
            lines. Without `before_seq` and `after_seq` you get the last events, oldest first. \
            A page is at most 100 events and 64 KiB (one event is returned even when it alone \
            is larger); when the page leaves out older events the \
            answer has `earlier`, pass it as `before_seq` for the page before; when it leaves \
            out newer ones it has `later`, pass it as `after_seq` for the page after. Each \
            event has its `seq`. To find where to read, use hqc_search_history, whose hits \
            give the session's address and the event's `seq`. With `subagent` (the `agentId` in \
            the result of your Agent call; without `session`, one of this session's own) you read \
            what that subagent has done so far, while it works and after: its text, tool calls and \
            their results, oldest first, without a cursor its last steps, in pages of at most 100 \
            events and 64 KiB. Page with `earlier` and `later` of the answer, passed as \
            `before_event` and `after_event` (not `before_seq` and `after_seq`): `earlier` reads \
            the steps before the page; `later` reads what the subagent did after it, also later on \
            while it works (a page read with `before_event` has no `later`). An id that is not a \
            subagent's `agentId` has no transcript and gives no events. Its thinking is left out \
            unless you pass `thinking: true`. Only the last 4 MiB of the subagent's transcript are \
            read: the oldest steps of a long subagent are not available, and paging back ends \
            where the part read starts. The session must be running. `gap: true` in the answer: \
            the `after_event` you gave is no longer among the subagent's last steps, so the page \
            is its newest ones and the steps in between are read back with `earlier`. Reading \
            changes nothing.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "session": {
                        "type": "string",
                        "description": "The session's address `<project-slug>/<session-slug>` \
                            or its id. Required unless `subagent` is given; with `subagent`, \
                            the session that runs it (default: this session)."
                    },
                    "subagent": {
                        "type": "string",
                        "description": "A subagent's `agentId`: read what it has done, not the \
                            session."
                    },
                    "before_seq": {
                        "type": "integer",
                        "description": "The last events before this `seq`; `earlier` of the \
                            previous answer."
                    },
                    "after_seq": {
                        "type": "integer",
                        "description": "The first events after this `seq`; `later` of the \
                            previous answer. Not together with `before_seq`."
                    },
                    "before_event": {
                        "type": "string",
                        "description": "With `subagent`: the last events before this event; \
                            `earlier` of the previous answer. Not together with \
                            `after_event`."
                    },
                    "after_event": {
                        "type": "string",
                        "description": "With `subagent`: what the subagent did after this \
                            event, also steps added since; `later` of the previous answer. Not \
                            together with `before_event`."
                    },
                    "thinking": {
                        "type": "boolean",
                        "description": "With `subagent`: include its thinking."
                    }
                },
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
            machine, with your model, or your model provider if you run on one, unless \
            `model` or `provider` is given. With `isolation: \"worktree\"` \
            the new session works in a git worktree of that workspace of its own, on a new \
            branch (hqc/<session-slug> unless `branch` is given), so it does not edit the \
            checkout you work in; the call then waits until the worktree is made and also \
            returns its `worktree` path and branch. The new agent's commits stay on that \
            branch for your owner to merge; the worktree is not deleted automatically. When \
            the user chose \"New session\" for a task of their message: `project` is this \
            session's project (the part before / of the address hqc_list_sessions marks \
            `current: true`), `prompt` the brief: the task as the user would send it to a \
            fresh session (the files or area, what done looks like) and, in a few sentences, \
            what that session needs from this conversation; do not do the task here, and once \
            this returns, say in one line that it runs in its own session, with its address, \
            and end your turn.",
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
                    "provider": {
                        "type": "string",
                        "description": "A model provider of your owner by its name (Settings → Models), e.g. a local model; the workspace's machine must allow it. Not together with `model`."
                    },
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
        name: ROOM,
        description: "Create an hqc room of your owner: a shared conversation of several agents \
            that your owner can read and join, each agent a session of its own with its own \
            model and system prompt. `message` is the room's first message, from this session; \
            in a `conversation` (the default) the first agent gets it and the agents then \
            answer in turn, in a `council` every agent gets it and answers it on its own. \
            Returns the room's address at once and does not wait for the agents: when their \
            discussion ends (every agent passed, every agent of a `council` answered, the \
            posts limit, a stop), the room sends this session how it ended and its last two \
            entries (read the rest with hqc_read), which starts a turn here, \
            so do not poll the room; write to it again with hqc_send. The agents run \
            in a workspace of `project` (the project's default workspace when `workspace` is \
            absent) on that workspace's machine, in the ask mode, each with your model, or \
            your model provider if you run on one, unless its `model` or `provider` is given. \
            Each agent counts as a session you spawned. An agent's `name` titles its session; \
            its `prompt` is added to its system prompt.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": "A project slug or id."},
                    "workspace": {
                        "type": "string",
                        "description": "A workspace of the project: its id, label or path."
                    },
                    "title": {"type": "string", "description": "The room's title."},
                    "mode": {
                        "type": "string",
                        "enum": ["conversation", "council"],
                        "description": "conversation (the default): the agents speak in turn; council: every agent answers each message on its own."
                    },
                    "agents": {
                        "type": "array",
                        "minItems": 1,
                        "description": "The room's agents.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string", "description": "The agent's name, the title of its session."},
                                "model": {"type": "string", "description": "The agent's model."},
                                "provider": {
                                    "type": "string",
                                    "description": "A model provider of your owner by its name (Settings → Models); the workspace's machine must allow it. Not together with `model`."
                                },
                                "prompt": {"type": "string", "description": "The agent's system prompt."}
                            },
                            "additionalProperties": false
                        }
                    },
                    "message": {"type": "string", "description": "The room's first message."}
                },
                "required": ["project", "agents", "message"],
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
        name: SECRET,
        description: "Ask the user for a secret without it entering the conversation: a \
            password (`kind: secret`, the default) or an ssh private key (`kind: ssh_key`). The \
            user sees a card with `name` and `purpose` and fills it in, from a password manager \
            too; the call waits for the answer and fails if the user declines or does not answer \
            before the tool call times out. A secret becomes the file \"$HQC_SECRETS/<name>\" \
            for as long as your process runs: use it inside a command, as \
            \"$(cat \"$HQC_SECRETS/<name>\")\", never print it, echo it or write it into a reply \
            or a file of the repository. A key is loaded into your ssh-agent (ssh and git use \
            it); the user may keep it for this workspace. Where the worker reads Vaultwarden \
            (Bitwarden) and you know the entry that holds it, pass it as `vault`: the card shows \
            it for the user to confirm or change and the worker reads the value itself; \
            elsewhere `vault` is dropped and the user types the value. \
            Returns `{note, path?, fingerprint?}`. \
            Ask only for what the task needs, once: a secret you were given is still there.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "A name of letters, digits, '_', '-' and '.', as WIFI_PASS or pi-lab."
                    },
                    "purpose": {
                        "type": "string",
                        "description": "One line for the user: what it is for (\"the Wi-Fi password to write into the camera's config\")."
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["secret", "ssh_key"],
                        "description": "secret (a password, a token) or ssh_key (a private key); secret when absent."
                    },
                    "vault": {
                        "type": "string",
                        "description": "The Vaultwarden (Bitwarden) entry that holds it: its exact name or its id."
                    }
                },
                "required": ["name", "purpose"],
                "additionalProperties": false
            })
        },
    },
    Tool {
        name: SSH_KEYGEN,
        description: "Make a new ed25519 ssh key for this workspace, for a device or a server \
            you set up: the private key stays with hqc on this machine (not in the repository, \
            not in the conversation), is loaded into your ssh-agent now and into that of every \
            later session of this workspace. Returns `{name, fingerprint, public_key}`: put \
            `public_key` into the device's ~/.ssh/authorized_keys, then ssh uses the key. A name \
            that this workspace already has is an error.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "A name of letters, digits, '_', '-' and '.', as pi-kitchen."
                    },
                    "comment": {
                        "type": "string",
                        "description": "The key's comment; `hqc <name>` when absent."
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            })
        },
    },
    Tool {
        name: PUBLISH,
        description: "Publish a page or a file from your workspace at a permanent public link: \
            anyone who has the link can open it in a browser, on any device, without signing in \
            to hqc, until the user unpublishes it or deletes this session. Call it only when \
            the user asks you to \
            publish something: the link is public. `path` is a folder with an `index.html` \
            (the whole folder is published, except any `.git` and `.hqc` in it: the page \
            loads its own styles, scripts, images, \
            fonts, ES modules, fetch/XHR and wasm from it by relative links, and nothing from \
            other sites; Web Workers, target=_blank and popups, form submission, cookies and \
            localStorage do not work, links open in the same tab) or a single file; absolute \
            or relative to your working directory, inside the session's \
            workspace, at most 100 MB packed. The user is asked in the chat first, with the \
            list of files; the call fails if the user denies it or does not answer before the \
            tool call times out. Returns `{url, event_id}`: give the user the `url` in your \
            answer.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "A folder with index.html, or one file; absolute or relative to the working directory."
                    },
                    "caption": {
                        "type": "string",
                        "description": "A line shown with the publication in the chat."
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

/// `args` of the `mcp.call` for [`PUBLISH`] (since SPEC 0.209): the worker packs `path` into a
/// zip itself and uploads it as an artifact of kind `site` (`PUT /worker/artifacts`, SPEC §12);
/// `path` is the canonical path it packed, for the card that asks the owner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishCall {
    pub artifact_id: Uuid,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

/// The limits of a published archive (SPEC §12, §20), the same on the worker that packs it and
/// on the hub that opens it: files in it, their size unpacked, the length of a name in it.
pub const MAX_SITE_FILES: usize = 10_000;
pub const MAX_SITE_UNPACKED: u64 = 500 * 1024 * 1024;
pub const MAX_SITE_NAME: usize = 512;

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
        assert!(tool(ROOM).is_some());
        assert!(tool(ATTACH).is_some());
        assert!(tool(SEND).is_some() && tool(ASK).is_some());
        assert!(tool(SEARCH_HISTORY).is_some());
        assert!(tool(READ).is_some());
        assert!(tool(MOVE).is_some());
        assert!(tool(PUBLISH).is_some());
        assert!(tool(RENAME).is_some());
    }

    #[test]
    fn instructions_name_every_tool() {
        for t in TOOLS {
            assert!(INSTRUCTIONS.contains(t.name), "{}", t.name);
        }
    }
}
