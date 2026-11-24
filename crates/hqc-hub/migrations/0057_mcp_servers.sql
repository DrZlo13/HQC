-- SPEC 0.235 (iteration 7): the remote MCP servers an owner keeps on the hub. A session's agent
-- gets each server of its project under the server's own name, as a stdio relay to the hub,
-- and the hub makes the calls with the headers it keeps. Header values are sealed with
-- HQC_MCP_KEY (XChaCha20-Poly1305): a backup of the database alone does not give them.
CREATE TABLE mcp_servers (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name text NOT NULL,
    url text NOT NULL,
    -- The names of the headers, in clear for the client; their values only in `sealed`.
    header_names text[] NOT NULL DEFAULT '{}',
    -- nonce (24 bytes) || ciphertext of the JSON object {name: value}; NULL: no headers.
    sealed bytea,
    timeout_s integer NOT NULL DEFAULT 300 CHECK (timeout_s BETWEEN 30 AND 1800),
    all_projects boolean NOT NULL DEFAULT true,
    -- ok | failing | unknown; `last_error` says why it failed last.
    status text NOT NULL DEFAULT 'unknown',
    last_error text,
    -- The server's own `serverInfo`, `instructions` and `tools/list`, cached for every agent.
    server_info jsonb,
    instructions text,
    tools jsonb,
    cached_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (owner_id, name)
);

CREATE TABLE mcp_server_projects (
    server_id uuid NOT NULL REFERENCES mcp_servers(id) ON DELETE CASCADE,
    project_id uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    PRIMARY KEY (server_id, project_id)
);
CREATE INDEX mcp_server_projects_project ON mcp_server_projects (project_id);
