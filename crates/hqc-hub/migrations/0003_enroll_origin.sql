-- T9 (SPEC §5.1, §20): where an enrollment came from. `country` is Cloudflare's
-- CF-IPCountry, shown before the user confirms the code; `client_ip` limits unconfirmed
-- codes per address.
ALTER TABLE enroll_codes ADD COLUMN country text;
ALTER TABLE enroll_codes ADD COLUMN client_ip text;
CREATE INDEX enroll_codes_client_ip ON enroll_codes (client_ip);
