-- SPEC 0.36 (T35, `hqc_schedule` and the hub cron, SPEC §3 ScheduledMessage, §16).
-- A message the hub sends as the owner's `user.message` into `session_id`, once at `at` or
-- by `cron` (5 fields) in `timezone` (IANA). `next_run` is the next time it is due (UTC);
-- NULL: it will not run again (an `at` that ran or was skipped). The cron loop reads only
-- enabled rows with a `next_run`, so a restart of the hub loses nothing.
CREATE TABLE scheduled_messages (
    id         uuid PRIMARY KEY,
    owner_id   uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    session_id uuid NOT NULL REFERENCES sessions (id) ON DELETE CASCADE,
    cron       text,
    at         timestamptz,
    timezone   text NOT NULL,
    message    text NOT NULL,
    enabled    boolean NOT NULL DEFAULT true,
    catch_up   boolean NOT NULL DEFAULT false,
    next_run   timestamptz,
    last_run   timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((cron IS NULL) <> (at IS NULL))
);
CREATE INDEX scheduled_messages_due ON scheduled_messages (next_run)
    WHERE enabled AND next_run IS NOT NULL;
CREATE INDEX scheduled_messages_session ON scheduled_messages (session_id);
CREATE INDEX scheduled_messages_owner ON scheduled_messages (owner_id);
