-- SPEC 0.68: the OIDC subject (`sub`) a user is bound to on the first OIDC sign-in (SPEC §3
-- User, §6). The issuer is fixed by HQC_OIDC_ISSUER, so `sub` alone is the key. NULL: a user of
-- Cloudflare Access or dev mode, or one who has not signed in through OIDC yet.
ALTER TABLE users ADD COLUMN oidc_sub text UNIQUE;
