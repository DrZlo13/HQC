# Iteration 8: tasks

Goal (SPEC §19.8): the owner uploads a skill once in Settings, and agents of the projects it is
given to get it on every worker, with its `references/` and `scripts/`, without copying it to
each machine by hand. Issue #62.

Criterion: upload a skill folder (with a `scripts/` file) and a `.zip` in Settings → Skills and
give one of them to one project; a new session of that project on home-mac lists `hqc:<name>`
among the skills in `system/init`, `/<name>` typed in the chat (the menu offers it) runs it, and
the agent runs its script; a session of another project has no such skill; upload the folder
again with a change, and the next start of the agent has the change, while the running one keeps
what it started with; a worker environment without `hub_skills` starts the agent as before; a
skill with `hooks` in its frontmatter is refused by the hub on upload and by the worker if a hub
sends it anyway.

Not in this iteration: a running process picking up a changed skill (`reload_plugins`); skills
of a foreign or restricted session; skills for ACP agents; plugins with commands, agents, hooks
or MCP servers of their own (only skills); MCP servers that run on the machine (iteration 7's
sketch); editing a skill in the browser.

## Decisions

- **The worker builds a plugin, the agent gets `--plugin-dir`.** As vvzvlad checked on Claude
  Code 2.1.312, and again on 2.1.328 (2026-11-25): a folder with `.claude-plugin/plugin.json`
  `{"name": "hqc"}` and `skills/<name>/` given to the CLI with `--plugin-dir` shows each skill as
  `hqc:<name>` in `system/init`; `/<name>` in the text and `Skill("<name>")` find it without the
  prefix when the name is unique. References to a skill in `CLAUDE.md` and `AGENTS.md` keep
  working. The machine's `~/.claude/skills` is not touched, and VS Code sessions on the machine do
  not get these skills.
  - The worker writes `plugin.json` itself and puts the hub's files only under
    `skills/<name>/`, so nothing from the hub becomes a plugin's hooks, MCP servers, agents or
    commands.
  - The folder is per set of skills (a hash of the sorted `name:sha256` pairs) under
    `$HQC_HOME/hub-skills/sets/`, built once into a temporary folder and renamed into place;
    sessions with the same set share it. Each skill's archive is fetched once per hash into
    `$HQC_HOME/hub-skills/zips/`. Folders and archives not used for 7 days are removed when the
    worker starts.
- **What travels.** `session.start.skills: [{name, sha256}]`, names and hashes only. The worker
  fetches an archive it does not have with `GET /worker/skills/{sha256}` and its worker token,
  as it fetches attachments (`/worker/artifacts/{id}`), checks the hash and unpacks it with its
  own limits. A plain HTTP GET instead of a frame: the archive is up to a few MiB of binary, and
  the downloader with its limits already exists. The hub answers only a hash of a skill of the
  worker's owner. A skill that cannot be fetched or fails the checks is left out of that start
  (logged with the reason); the others and the session go on.
- **The hub keeps a normalized archive.** An upload is a folder (the client sends its files as
  multipart parts, paths in the file names) or a `.zip`. The hub opens it with limits, takes
  `SKILL.md` at the top or under one top folder, drops `__MACOSX/`, `.DS_Store` and `.git/`, and
  stores a zip it writes itself: paths sorted, fixed times, mode `0644` or `0755` (the executable
  bit of a `.zip` entry is kept; a folder from a browser has none, so a script is run as
  `bash scripts/x.sh` or `python3 …`, as most skills do). Its sha256 is the skill's hash.
  Limits: 4 MiB unpacked, 500 files, a path of at most 200 bytes, an upload of at most 8 MiB, at
  most 100 skills per owner. vvzvlad's 36 skills are about 2 MB, the largest 572 KB.
- **The name.** The frontmatter's `name` when it has one, else the folder's (or the `.zip`'s)
  name; `[a-z0-9][a-z0-9_-]{0,63}` (no `:` nor `.`). Uploading a skill with a name the owner has
  replaces its content and keeps its projects. The CLI shows a plugin's skill under its folder's
  name, so the frontmatter's `name`, if any, must be that name.
- **Which sessions get which skill.** "All projects" or chosen projects, as for MCP servers; a
  change reaches a session at its next process start. Only the owner's unrestricted Claude Code
  sessions on the owner's own workers (the hub checks it, and the worker again).
- **The worker decides whether it takes them.** `hub_skills = true` in an environment of
  `worker.toml` (off by default), as `hub_mcp`. Without it the worker drops the names (logged)
  and starts the agent as now.
- **A skill on the machine with the same name: the hub's copy wins [decision].** As for MCP
  servers in iteration 7, what the owner set in hqc is what the agent gets on every worker. The
  worker adds `"skillOverrides": {"<name>": "off"}` to `--settings` for each skill of the hub,
  which hides a same-named skill of `~/.claude/skills` (and of the repository's `.claude/skills`
  with `project_settings`) and leaves `hqc:<name>`. Accepted, from vvzvlad's check on 2.1.312:
  with a machine copy, `/<name>` typed in full answers "disabled via skillOverrides" and
  `Skill("<name>")` by that exact name is refused, while a plain request makes the model call
  `hqc:<name>`. Without a machine copy `/<name>` reaches `hqc:<name>` (checked on 2.1.328). So the
  chat's `/` menu matches the part after `hqc:` too and inserts `/hqc:<name>`.
- **Skills cannot get past the worker's `deny` [decision].** A skill is instructions and code the
  agent runs. Checked on 2.1.328 (2026-11-25):
  - `allowed-tools` in the frontmatter lets the listed tools run without a permission card while
    the skill is active, but the worker's `deny` still refuses them (a `deny` of
    `Bash(touch:*)` stopped a skill with `allowed-tools: Bash`). Accepted: a hub can already
    answer a card itself (§2.1). Settings shows a skill's `allowed-tools`.
  - `` !`command` `` in `SKILL.md` goes through the same permission check (a `deny` of `Bash`
    stopped it).
  - `hooks` in the frontmatter run shell commands outside the `deny`, the
    `--disallowedTools` and any card, as a repository's hooks do with `project_settings`.
    Not accepted: the hub refuses an upload whose `SKILL.md` (any file of that name in the
    skill) has a `hooks` key, and the worker refuses such a skill too, since it does not trust
    the hub for this. To keep YAML tricks out, a frontmatter is accepted only when each line at
    its top level is `key:` with a plain key (no quotes, no `{`, no `?`, no `<<` merge), and no
    line of the file at column 0 is a `hooks:` key.
  No flag lets hooks in: nobody needs it now.

## T67. Skills on the worker

Sections: §5.3, §8.1, §14.
- `worker.toml`: `hub_skills = true` in `[environments.<name>]`; `hqc-worker env` and
  `env which` show it.
- `session.start.skills?: [{name, sha256}]`. Taken only where `hub_skills` holds and the session
  is the owner's unrestricted Claude Code session; valid names and hashes only.
- Before the process starts: the archives fetched (`GET /worker/skills/{sha256}`, the hash
  checked), the set's plugin folder built, `--plugin-dir <folder>` and `skillOverrides` in
  `--settings`.
- Unpacking: only relative paths without `..`, no links, the limits above, mode `0644`/`0755`;
  the frontmatter checks (shared with the hub, `hqc_proto::skills`).
- Old sets and archives swept at start.

Acceptance: the common checks. Worker tests:
- without `hub_skills`, no `--plugin-dir`;
- a restricted or foreign session gets none;
- the plugin folder's layout and `plugin.json`, `--plugin-dir` and `skillOverrides`;
- a hash mismatch, a path out of the folder, and `hooks` refused, the other skills kept;
- one fetch per hash for two sessions;
- a live test behind `HQC_LIVE_CLAUDE=1` (`claude-haiku-5-5`, max-turns ≤ 2): `hqc:<name>` in
  `system/init`.

## T68. Skills on the hub

Sections: §5.3, §6, §20.
- Migration `0058_skills`: `skills` (owner, name, description, allowed_tools, sha256, size,
  files, the archive, all projects or not) and `skill_projects`.
- REST, owner only:
  - `GET /api/skills`;
  - `POST /api/skills` (multipart: a `zip` part or `file` parts, an optional `name`): creates
    or replaces by name;
  - `PATCH /api/skills/{id}` {all_projects?, project_ids?};
  - `DELETE /api/skills/{id}`;
  - `GET /api/skills/{id}/files`: the paths and sizes, for the list.
- `GET /worker/skills/{sha256}` with the worker token.
- `session.start.skills` from the session's project.

Acceptance: the common checks. Hub tests:
- a folder upload and a `.zip` with one top folder give the same hash;
- a re-upload replaces the content and keeps the projects;
- `hooks`, a path with `..`, a link, too large, a bad name refused;
- `session.start` carries the project's skills only, none for a restricted session or a
  foreign worker;
- a worker fetches its owner's skill, and not another owner's.

## T69. The client

Sections: §6 client layout.
- Settings → Skills: upload a folder or a `.zip` (a re-upload of a name says it replaces), the
  list with each skill's description, size, files and `allowed-tools`, "All projects" or the
  chosen projects, delete.
- A project's settings list the skills it gets.
- The chat's `/` menu matches `hqc:<name>` by `<name>`.
- The mock and its routes.

Acceptance: the common checks. Client tests: the list, the upload (folder and zip), the project
picker, delete, the menu's match.

Order: T67 and T68 together (the frame and the route are agreed here), then T69.
