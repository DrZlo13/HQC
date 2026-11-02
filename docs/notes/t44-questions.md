# T44: decisions

ssh keys of an environment, SPEC 0.57.

## Store

- **The store is checked at load, not only at `run`.** `Config::load` (used by `hqc-worker run`
  and `hqc-worker env`) checks `$HQC_HOME/keys/` and `$HQC_HOME/agents/`: each must be a real
  folder of the current user with no group/other bits, every entry of the store a regular file
  of the user with no group/other bits (0600 and 0400 pass), and every `ssh_keys` name a file
  of the store. The message names the `chmod`. Nothing is repaired: a store that was open may
  already have leaked, so the owner decides. A store that does not exist is fine until a name
  needs it. `Config::parse` stays free of file-system checks (tests parse configs without a
  store); `Config::check_ssh` is the check.
- **Every file of the store counts**, not only the keys in use: a `.pub` copied in by hand with
  0644 stops the worker too. Simpler to state than "only the named keys", and the store is for
  private keys.
- **`add-key` needs no `worker.toml`** (it is how a store is made before the config names the
  key). It refuses an open existing store instead of fixing it, refuses to replace a key of the
  same name (`create_new`), and prints the destination path, never the key. It does not check
  that the file is a key: `ssh-add` says so at the first session start.
- **Key names**: letters, digits, `_`, `-`, `.`, at most 64, not starting with `.` (hidden files)
  or `-` (an argument to `ssh-add`). The names are not in `hello` (no user on the hub, as with the
  names of `env`).

## Agents

- **One ssh-agent per agent process**, not per session: a session's process that is stopped for
  idleness and started again gets a new agent on a new socket. The socket name is random
  (`<16 hex>.sock`), so a socket is never reused, and the path stays short (`sun_path` is 104
  bytes on macOS; a longer path fails the start with a hint to shorten `HQC_HOME`).
- `ssh-agent -D -a <socket>` runs as a child of the worker (cleared environment, `PATH` only, its
  own process group so a Ctrl-C at the worker's terminal does not reach it, `kill_on_drop`). The
  worker waits for its first stdout line (printed once the socket listens), then runs `ssh-add -q
  <files>` with `SSH_ASKPASS_REQUIRE=force` and `SSH_ASKPASS=/usr/bin/false`, so a key with a
  passphrase fails the session's start with `ssh-add`'s message instead of prompting on the
  worker's terminal. 10 s for each step.
- **Lifetime**: the adapter's monitor task owns the `SshAgent` and drops it right after the CLI
  has exited (with the process's entry in the own-records registry). Drop sends SIGKILL and
  removes the socket. Covered: stop (the turn's interrupt does not end the process; the agent
  lives on with it), kill, crash, idle stop, worker shutdown (the supervisor kills every agent;
  past the shutdown timeout the runtime drops the tasks, and with them the guards), a failed
  spawn and an early exit (the guard is dropped on the error path).
- **A worker that died** (SIGKILL, power) cannot stop its agents. At the next `run`, before any
  session, every socket in `$HQC_HOME/agents/` is connected to and sent
  `SSH_AGENTC_REMOVE_ALL_IDENTITIES`, then removed. The orphaned ssh-agent process itself stays
  (keyless, unreachable) until the user's logout: killing it would need its pid, and a pid file
  can name another process after a reboot. Only sockets are touched. The cleanup is skipped if
  another daemon answers on `$HQC_HOME/mcp.sock` (that one would stop this start anyway, and its
  agents are live).
- **`SSH_AUTH_SOCK`** left the child allowlist (also for the title runs, which use the same
  list), is refused as a name in an environment's `env`, and is set by the worker after `env`,
  like `CLAUDE_CODE_ENTRYPOINT`.

## Live check

One run of `claude_live::live_environment_ssh_keys` (two `claude -p`, `claude-haiku-5-5`,
`--max-turns 2`, `tmp/spike-t44-*`, 2026-11-02). The test starts an unprivileged `sshd` on
loopback (`/usr/sbin/sshd -D -f <own config>`, `UsePAM no`, `authorized_keys` with only the
public half of the store key; the private key exists only in the store) and first checks that
`ssh -o BatchMode=yes` without an agent is refused (`~/.ssh` has no key for it). In the folder
bound to `infra` (`ssh_keys`, `Bash(ssh:*)` unblocked) the agent ran `ssh-add -l; ssh -F
/dev/null -o BatchMode=yes … 127.0.0.1 echo hqc-t44-ok`: `ssh-add -l` listed the store key, the
login printed `hqc-t44-ok`, sshd logged `Accepted publickey` with the store key's fingerprint,
and the socket was gone after the kill. In the other folder `ssh-add -l` said "Could not open a
connection to your authentication agent." and the command line had no `SSH_AUTH_SOCK`.
