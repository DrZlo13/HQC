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
            inside the session's workspace.",
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
];

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
        assert!(tool(ATTACH).is_some());
        assert!(tool("hqc_send").is_none(), "no stubs (docs/ITERATION-3.md)");
    }
}
