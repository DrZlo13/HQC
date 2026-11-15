-- SPEC 0.144 (iteration 5, model providers, SPEC §3 Provider).

-- The owner's providers: an Anthropic-compatible endpoint with one model. Nothing secret: a
-- key lives in the worker's worker.toml.
CREATE TABLE providers (
    id             uuid PRIMARY KEY,
    owner_id       uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name           text NOT NULL,
    label          text NOT NULL,
    base_url       text NOT NULL,
    model          text NOT NULL,
    context_tokens integer,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (owner_id, name)
);

-- The provider a session runs on; NULL: the runtime's own endpoint. A provider in use is not
-- deleted.
ALTER TABLE sessions ADD COLUMN provider_id uuid REFERENCES providers (id);

-- The provider names the worker's worker.toml allows, from its hello.
ALTER TABLE workers ADD COLUMN providers jsonb NOT NULL DEFAULT '[]';
