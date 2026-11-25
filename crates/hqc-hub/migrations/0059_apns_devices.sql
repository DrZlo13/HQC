-- SPEC 0.248 (Apple push to the native iOS client, SPEC §16): the devices of a user that take
-- notices over APNs. `sandbox`: a development build, sent through APNs' sandbox host.
CREATE TABLE IF NOT EXISTS apns_devices (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token text NOT NULL UNIQUE,
    sandbox boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS apns_devices_user ON apns_devices (user_id);
