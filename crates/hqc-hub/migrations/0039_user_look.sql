-- The client's look (SPEC §3 User): `compact` or `spacious`, the same on every device of the user.
ALTER TABLE users
    ADD COLUMN look text NOT NULL DEFAULT 'spacious'
        CHECK (look IN ('compact', 'spacious'));
