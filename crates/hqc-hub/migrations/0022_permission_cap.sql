-- SPEC 0.43 (T39, a message cannot raise permissions, SPEC §5.4, §6, §7.1).

-- While the worker runs the current turn under a peer message's cap
-- (`session.state.capped_input`): the address of the session up the chain whose `ask` set
-- it. Null otherwise.
ALTER TABLE sessions ADD COLUMN permission_capped_by text;
