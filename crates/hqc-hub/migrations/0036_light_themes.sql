-- SPEC 0.88: light themes, the light counterparts of the three dark ones (SPEC §3 User).
-- `users_theme_check` is the name Postgres gave the column's CHECK in 0024_appearance.
ALTER TABLE users
    DROP CONSTRAINT users_theme_check,
    ADD CONSTRAINT users_theme_check
        CHECK (theme IN ('warm', 'graphite', 'contrast', 'warm_light', 'graphite_light', 'contrast_light'));
