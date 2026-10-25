//! The hqc MCP tools (SPEC §7): the list `hqc-worker mcp` gives in `tools/list` and the hub
//! implements behind `mcp.call` (SPEC §5.2). A tool appears here only once its task lands
//! (docs/ITERATION-3.md): no stubs.

use serde_json::{json, Value};

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
pub const SEND: &str = "hqc_send";
pub const ASK: &str = "hqc_ask";

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
        name: SEND,
        description: "Send a message to another hqc session of your owner without waiting for a \
            reply. The other agent reads it before its next input (or at once if that session \
            wakes on messages); it may answer you later with hqc_send. `session` is an address \
            from hqc_list_sessions (<project-slug>/<session-slug>) or a session id.",
        input_schema: || {
            json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "The message."},
                    "session": {
                        "type": "string",
                        "description": "The target session: <project-slug>/<session-slug> or its id."
                    }
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
            (<project-slug>/<session-slug>) or a session id.",
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
                    }
                },
                "required": ["text", "session"],
                "additionalProperties": false
            })
        },
    },
];

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
    /// `hqc_ask` got no reply within its timeout (SPEC §7.1).
    pub const TIMEOUT: &str = "timeout";
    /// A chain limit of SPEC §7.1 (`depth`, `hops`): only a human's turn goes on.
    pub const LIMIT_EXCEEDED: &str = "limit_exceeded";
    /// The worker restarted while `hqc_ask` waited: the blocking call died with it.
    pub const WORKER_RESTART: &str = "worker_restart";
    pub const INTERNAL: &str = "internal";
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
        assert!(tool(SEND).is_some() && tool(ASK).is_some());
        assert!(
            tool("hqc_spawn").is_none(),
            "no stubs (docs/ITERATION-3.md)"
        );
    }
}
