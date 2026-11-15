# Iteration 6: tasks

Goal (SPEC §19.6): give a running session a secret or an ssh key from the chat, without the
value reaching the model, the transcript or the hub's storage. The owner's three cases
(2026-11-15):

- an agent sets up a device and has to write the Wi-Fi password into it;
- an agent needs a password once, to look at a router;
- a session gets the ssh key of a Raspberry Pi it has just set up, and keeps it for that
  workspace.

`env` and `ssh_keys` of an environment (§14) stay what they are for standing access; this is
for what comes up in the middle of a conversation.

Criterion: from a phone, give a session on home-mac a password with the composer's "Secret",
and have the agent use it in a command without the password showing in the feed, the
transcript JSONL, the hub's database or the logs; an agent asks for a password with
`hqc_secret`, the owner fills the card with 1Password's autofill, the agent goes on; paste a
Pi's private key with "remember for this workspace" and have a new session of that workspace
`ssh` to the Pi; an agent makes a key with `hqc_ssh_keygen`, installs its public half on the
Pi and connects, and the private half never leaves the worker; a worker without
`session_secrets` refuses all of it.

Not in this iteration: 1Password's `op` CLI on the worker (the owner declined it: the phone's
autofill covers it); remembering a plain secret (a secret that is used again goes into the
device's or the project's config, where the agent puts it); secrets of a foreign or restricted
session.

## Decisions

- **The value goes to the process, not to the model.** A secret is a file the agent reads in
  a command (`"$(cat "$HQC_SECRETS/WIFI_PASS")"`); the agent is told the path, never the value.
  A key is loaded into the process's own `ssh-agent`; the agent gets the socket, as with
  `ssh_keys` now. What the agent then prints is its business: the boundary is that the owner
  gives the secret, seeing which session asks and why. A prompt-injected agent that has a
  secret can leak it; nothing here prevents that, and nothing gives a secret without the owner.
- **The worker decides.** `session_secrets = true` in `worker.toml` (top level, off by
  default) turns all of it on; the hub cannot. A restricted session (§8.4) and a session whose
  owner is not the worker's owner (`owner_is_worker_owner`) get none of it.
- **The hub only passes it on.** The value goes client → hub → worker in one request: the hub
  sends it to the worker in a frame of its own and answers the client with the worker's
  answer; it is not in an event, the outbox, the database or a log line, and it is not queued
  for a worker that is offline (`409 worker_offline`, the client keeps the form filled). The
  hub sees the value in memory while it passes it; encrypting it to a worker key would not
  hide it from a hub that hands out the workers' keys, so it is not done.
- **A secret lives as long as the agent's process.** `$HQC_HOME/secrets/<process>/` (0700,
  files 0600), removed when the process ends (stop, idle stop, restart, crash: the next start
  of the worker removes what a dead one left), as the `ssh-agent` socket is. After a restart
  the agent asks again.
- **A key is remembered per workspace.** "Remember for this workspace" (and every
  `hqc_ssh_keygen` key) goes into `$HQC_HOME/workspace-keys/<hash of the workspace's
  canonical path>/` (its path in a file beside the keys; the worker knows a workspace by its
  path, as `env_bindings` do), apart from the store of `ssh_keys`: through the hub one only adds a new key to one's own
  workspace and never binds a key of the worker's store to a folder by its name (which would
  give any folder the owner's `infra_ed25519`). Every agent process of a session in that
  workspace loads them, beside its environment's `ssh_keys`. A key with a passphrase is
  stored without it (the worker decrypts it with the passphrase given once; at a start there
  is no one to ask).
- **Every process gets an ssh-agent** when `session_secrets` is on, empty if nothing is to be
  loaded, so a key can be added in the middle of a conversation.

## T57. Secrets and keys on the worker (done, SPEC 0.147)

Sections: §2.1, §5.3, §8.1, §14.
- `worker.toml`: `session_secrets = true`. `hello` carries `session_secrets: true` so the
  client shows "Secret" only where it works.
- A frame hub → worker `secret.put {request_id, session_id, kind: "secret" | "ssh_key", name,
  value, passphrase?, remember?}`; the worker answers `secret.result {request_id, ok, error?,
  path?, fingerprint?}`. Refused: the feature is off, the session is restricted or has no live
  process (an agent that is stopped gets nothing: the owner gives it after the start), a bad
  name (as `ssh_keys` names), a value over 64 KiB, a key `ssh-keygen` does not read (or a wrong
  passphrase).
- `secret`: written to `$HQC_SECRETS/<name>`; `HQC_SECRETS` is in the process's environment
  from its start. `ssh_key`: decrypted in the worker (the `ssh-key` crate, no passphrase on
  a command line), loaded with `ssh-add` from a 0600 temporary file that is removed at once;
  with `remember`, also written to the workspace's key store.
- The answer's `note` tells the agent where the secret is (`$HQC_SECRETS/WIFI_PASS`, to use as
  `"$(cat "$HQC_SECRETS/WIFI_PASS")"`) or that the key is loaded; the worker adds nothing to the
  next input: the client puts `note` into the message being written (T59), and `hqc_secret`
  returns it.
- Workspace keys: `workspace.keys {path}` lists names and fingerprints,
  `workspace.key_delete {path, name}` removes one; `hqc-worker env` lists them.
- Never in a log, a frame other than `secret.put`, an error message or `hqc-worker env`.
Acceptance: the common checks; worker tests: a secret's file and the line of the next input,
the file gone when the process ends and after a dead worker's restart, a key loaded (a live
`ssh-agent`), a passphrase-protected key, a remembered key in a new process of the workspace
and not in another workspace, every refusal, the value in no log line (a test greps the
captured log).

## T58. Passing them through the hub, `hqc_secret` and `hqc_ssh_keygen` (done, SPEC 0.147)

Sections: §5.3, §6, §7, §10.1.
- `POST /api/sessions/{id}/secrets {kind, name, value, passphrase?, remember?, request_id?}`:
  the owner's own session, its worker online and with `session_secrets`; the hub sends
  `secret.put` and returns the worker's answer (`200`, `422` with its error, `409
  worker_offline`, `504` after 30 s). It writes the event `secret.given {kind, name,
  fingerprint?, remembered}` (no value) to the feed.
- `hqc_secret(name, kind?, purpose)`: deferred as `hqc_move` is (a PendingRequest `secret`,
  a card in the feed with the session, the name and the purpose); the owner's answer is the
  `POST` above with `request_id`; the result tells the agent the path or the key's
  fingerprint; "Decline" gives the agent a refusal. Same timeout as `hqc_ask`.
- `hqc_ssh_keygen(name, comment?)`: the worker makes an ed25519 key in the workspace's key
  store, loads it into the process's ssh-agent and returns the public key; no card (it gives
  the agent nothing it could not do with `ssh-keygen` itself, but the private half stays out
  of the workspace). The feed shows the call as any tool call.
- `GET /api/workspaces/{id}/keys`, `DELETE /api/workspaces/{id}/keys/{name}` through the
  frames of T57.
Acceptance: the common checks; hub tests: the owner checks, a worker without the feature, an
offline worker, the value in no event, row or log line, `hqc_secret` answered and declined,
the request closed by the runtime's cancel.

## T59. The client (done, SPEC 0.147)

Sections: §6 client layout, §10.1.
- The composer's attach menu has "Secret" (only for a session whose worker has
  `session_secrets`): a form with the kind (password / ssh key), a name, the value (`type=
  password`, `autocomplete="current-password"`, so a phone's password manager fills it; a
  key is a textarea that is not kept in a draft), a passphrase for a key, "remember for this
  workspace". It sends the `POST`; the value is cleared from the form at once, success or not.
- The `hqc_secret` card: the same form, with the agent's name and purpose, "Give" and
  "Decline".
- `secret.given` in the feed as a service line ("Secret WIFI_PASS given", "ssh key pi-lab
  remembered for this workspace").
- The workspace settings: its remembered keys (name, fingerprint, delete).
Acceptance: the common checks; client tests: the form, the value never in a draft, local
storage or the feed, the card, the service line, the key list.

Order: T57 first (the frames are agreed here), then T58 and T59.

Done in one pull request with this plan (2026-11-15). Proto: `secret.put`, `workspace.keys`,
`workspace.key_delete` and their results (`frame.rs`, `SecretValue`, whose `Debug` shows the
length only), `hello.session_secrets`, `SecretRequest` (`api.rs`, unknown fields refused), the
tools `hqc_secret` and `hqc_ssh_keygen` (`mcp.rs`, `SECRET_TOOLS`), `approval.request.secret`
(`event.rs`). Worker: `session_secrets` (`config.rs`, `SessionSecrets` in `StartSpec`),
`secrets.rs` (the process's folder, the workspace key store, keys read, decrypted and made with
the `ssh-key` crate), `ssh::ssh_add` and an empty ssh-agent, `HQC_SECRETS` and
`HQC_SECRET_TOOLS` in the adapter, `Supervisor::put_secret`, the MCP server's `listed_tools`,
`hqc-worker env`, the folders of a dead worker removed at start. Hub: migration `0044`,
`secrets.rs` (`POST /api/sessions/{id}/secrets`, `hqc_secret`, `hqc_ssh_keygen`, the keys of a
workspace), the request kind `secret` (`requests.rs`: decline and cancel), a worker's
`approval.request.secret` dropped (`events.rs`). Client: `ui/secrets.tsx` (the form, the card,
the kept keys), the composer's key button, the requests screen, the rules screen, the mock.
Tests: worker `secrets::tests`, `config::tests::secrets_only_for_the_owners_unrestricted_sessions`,
`mcp::server::tests::secret_tools_only_where_secrets_are_given`,
`tests/claude_code.rs::secrets_and_keys_reach_the_running_agent` (a real ssh-agent: a secret's
file, a key with a passphrase, a made key, both kept and loaded by the next process, the
refusals) and `without_session_secrets_nothing_is_given`; hub `tests/secrets.rs` (the value in no
table, the refusals, `hqc_secret` given, declined and cancelled, a worker's fake card,
`hqc_ssh_keygen`, the keys); proto `a_secret_is_not_in_debug`, `a_secret_card_roundtrips`;
client `secrets.test.tsx`.

To use it: `session_secrets = true` at the top of a worker's `worker.toml`, then a restart of the
worker.

## T60–T62. The browser in a session (done, SPEC 0.148)

The owner's case (2026-11-15): an agent that looks through listings on eBay, AliExpress and
Amazon in the owner's accounts and reports what it found. An iframe cannot do it (those sites
forbid framing, and a cross-origin frame is out of reach); an extension of our own would redo
Anthropic's Claude in Chrome, which Claude Code drives with `--chrome`. Checked by hand on
drzlo-mac (Claude Code 2.1.322): under hqc's flags the CLI connects `claude-in-chrome` and its
tools reach the user's Chrome; the agent gets a tab group of its own, and a tab the user drags
into it is the agent's too.

Decisions: given per session by the owner, on a worker whose `worker.toml` has `browser = true`;
no rules of our own on the browser's tools (the owner watches the tab group, the extension has
its own site permissions): everything but `file_upload` and `upload_image`, under the session's
mode; every other session gets `--no-chrome`, so `/chrome`'s default cannot give one the browser.

- T60 (worker): `browser` in `worker.toml` and `hello`, `session.start.browser` →
  `--chrome` and the two tools in `--disallowedTools`, `--no-chrome` otherwise, `system
  browser_refused`, `hqc-worker env`. Tests: `config::tests::the_browser_only_where_the_worker_gives_it`,
  `adapter::claude_code::tests::the_browser_only_where_it_is_given`.
- T61 (hub): migration `0045`, `PATCH /api/sessions/{id} {browser}` (only the worker owner,
  only a worker with `browser`, a change restarts the agent), `session.start.browser`,
  `agent.browser`, `browser` in `GET /api/workers`. Tests: `tests/browser.rs`.
- T62 (client): "Browser" in the chat's "More" with a confirmation, the header's "Browser",
  the mock. Tests: `chat.test.tsx`.

To use it: install the Claude in Chrome extension in the machine's Chrome and sign it in to the
same Claude account as the worker's `claude`; `browser = true` at the top of `worker.toml`; a
restart of the worker; then "Browser" in a session's "More".
