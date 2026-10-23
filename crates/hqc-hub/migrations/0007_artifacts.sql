-- SPEC 0.17 (T16): artifacts (SPEC §3 Artifact, §12). The content lives in the artifact store
-- (`HQC_ARTIFACT_DIR`) under `storage_key` (its sha256), shared by every row with that hash.
-- A user uploading the same content under the same name gets the existing row back.
CREATE TABLE artifacts (
    id uuid PRIMARY KEY,
    owner_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- The first session the artifact was attached to; `artifact_sessions` has all of them.
    session_id uuid REFERENCES sessions(id) ON DELETE SET NULL,
    kind text NOT NULL,
    mime text NOT NULL,
    size bigint NOT NULL,
    sha256 text NOT NULL,
    name text NOT NULL,
    storage_key text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (owner_id, sha256, name)
);

-- Sessions whose input referenced the artifact: the worker of such a session may download it
-- (`GET /worker/artifacts/{id}`, SPEC §5.3).
CREATE TABLE artifact_sessions (
    artifact_id uuid NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    session_id uuid NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (artifact_id, session_id)
);
CREATE INDEX artifact_sessions_session ON artifact_sessions (session_id);
