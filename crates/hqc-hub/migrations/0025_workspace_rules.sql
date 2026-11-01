-- SPEC 0.52 (T48, hqc's allow rules of a workspace, SPEC §3 Workspace, §5.3, §6, §14).

-- Standing answers of the worker owner for one folder: the worker applies them in
-- `can_use_tool` after its `deny`. The syntax is checked by `hqc_proto::rules`.
CREATE TABLE workspace_rules (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces (id) ON DELETE CASCADE,
    rule         text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, rule)
);
