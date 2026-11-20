-- SPEC 0.190: an artifact of kind `site` (`hqc_publish`) published by a public link. While
-- `published_at` is set, `public_id` (128 random bits, 32 hex characters) is the first segment
-- of the link on the publication domain (`HQC_PUBLISH_BASE`); unpublishing clears both. A
-- published site's `session_id` is the publishing session: its deletion (`ON DELETE SET NULL`)
-- takes the publication down.
ALTER TABLE artifacts ADD COLUMN published_at timestamptz;
ALTER TABLE artifacts ADD COLUMN public_id text UNIQUE;

-- Every `hqc_publish` uploads an archive of its own, even of the same files under the same
-- name, so that two publications are unpublished apart: the same content and name is one
-- record for every other kind only.
ALTER TABLE artifacts DROP CONSTRAINT artifacts_owner_id_sha256_name_key;
CREATE UNIQUE INDEX artifacts_owner_id_sha256_name_key ON artifacts (owner_id, sha256, name)
    WHERE kind <> 'site';
