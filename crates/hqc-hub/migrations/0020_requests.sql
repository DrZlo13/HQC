-- SPEC 0.41 (T37, requests shown where the user is looking, SPEC §6, §10.1).

-- The list of open requests carries each request's event: found by session and
-- `request_id` among the request events, without a scan of the session's feed.
CREATE INDEX events_requests ON events (session_id, (payload ->> 'request_id'))
    WHERE type IN ('approval.request', 'clarify.request');
