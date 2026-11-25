-- SPEC 0.255 (the user's own quick messages, shown in every project; SPEC §3 User): empty until
-- the user adds some.
ALTER TABLE users
    ADD COLUMN quick_messages text[] NOT NULL DEFAULT '{}';
