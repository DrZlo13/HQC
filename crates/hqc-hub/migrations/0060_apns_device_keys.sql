-- SPEC 0.249 (end-to-end encrypted notices, SPEC §16): the key a device made for its notices.
-- With one, the hub seals a notice's content (AES-256-GCM) and sends Apple, and a relay, only a
-- generic alert; the app's notification extension opens it on the device. NULL: an app that
-- registered before, whose notices go as before.
ALTER TABLE apns_devices ADD COLUMN IF NOT EXISTS notice_key bytea;
