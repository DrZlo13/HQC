-- A project's quick messages (SPEC §3 Project): the texts the chat's quick-message button sends
-- to the agent with one press, in the order the user keeps them.
ALTER TABLE projects
    ADD COLUMN quick_messages text[] NOT NULL DEFAULT '{}';
