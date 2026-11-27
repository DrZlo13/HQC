// Mock hub for client development: REST and WebSocket /api/ws as in SPEC §6, events built
// from crates/hqc-proto/examples/wire/*.json. Not a reference implementation of the hub,
// but it speaks the hub's shapes (hqc_proto::client_frame and hqc_proto::api): flat /api/ws
// frames, `delivery` frames for delivery changes, `{error, message}` errors, the session
// object of SPEC §6 and the hub's unread rule.
//
//   node mock/server.mjs            # listens on MOCK_PORT (default 8787)
//   MOCK_SPEED=0.2 node mock/...    # scales all scenario delays
//
// Scenario of a turn: status running -> agent.delta stream -> agent.message (final: false)
// -> tool.call/tool.result (Read) -> tool.call (Bash) -> approval.request, wait for the
// answer -> approval.response -> tool.result -> agent.delta -> agent.message (final) ->
// usage -> status idle. A deny answer may carry `message` (a text answer, SPEC §10.1): the
// final message quotes it. A message containing "question" or "clarify" asks a clarify.request
// instead of the approval; one containing "markdown" gets MARKDOWN_ANSWER streamed and
// nothing else. The Russian words "вопрос" and "разметка" trigger the same.
//
// The worker home-mac has its service session (SPEC §10.2, `system: true`): log lines
// (`system {code: worker_log, level}`) and an open confirmation of a new folder; input there
// is `409 system_session`.

import { createServer } from 'node:http';
import { randomUUID, generateKeyPairSync } from 'node:crypto';
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { WebSocketServer } from 'ws';

const WIRE = join(dirname(fileURLToPath(import.meta.url)), '../../crates/hqc-proto/examples/wire');
const STATUS = Symbol('status');
/** A reply that is not JSON: `{[RAW]: {type, data}}`. */
const RAW = Symbol('raw');
/** Project colors (T49, `hqc_proto::api::ProjectColor`). */
const COLORS = ['teal', 'plum', 'blue', 'rose', 'moss', 'coral', 'cyan', 'slate'];
/** Permission modes (T38, `hqc_proto::frame::PermissionMode`). */
const MODES = ['ask', 'auto', 'allow_all'];

/** The answer to a message with "markdown": every element of T11 to look at. */
export const MARKDOWN_ANSWER = `## Run summary

Checked **three** crates, *one* with warnings; details in [CI](https://example.com/ci/42).
The second line of the same paragraph, \`cargo test --workspace\` passed.

### What was done

1. Updated the parser
   - made it tolerate new fields
   - [x] fixture tests
   - [ ] fixtures for 2.2
2. Fixed the ~~old~~ error output

| crate | tests | status |
|:------|-------:|:------:|
| hqc-proto | 42 | ✓ |
| hqc-hub | 117 | ✓ |
| hqc-worker | 63 | warnings |

\`\`\`rust
fn main() {
    println!("<hello>");
}
\`\`\`

> Quote: unclosed code blocks in the stream do not break the layout.

---

HTML is shown as text: <img src=x onerror=alert(1)> and <script>alert(1)</script>,
the link [javascript:](javascript:alert(1)) is text too, the image is not loaded: ![diagram](https://example.com/d.png).
`;

/** Slash commands of every mock session: part of what Claude Code 2.1.308 lists (T27). */
export const MOCK_COMMANDS = [
  'clear',
  'compact',
  'context',
  'code-review',
  'config',
  'init',
  'model',
  'review',
  'security-review',
  'simplify',
  'usage',
  // A skill of the hub (SPEC 0.245), as the CLI lists a plugin's skill.
  'hqc:bench',
];

export function loadExamples(dir = WIRE) {
  const out = {};
  for (const f of readdirSync(dir)) {
    if (f.endsWith('.json')) out[f.replace(/\.json$/, '')] = JSON.parse(readFileSync(join(dir, f), 'utf8'));
  }
  return out;
}

export async function startMock({ port = 0, speed = 1, log = () => {} } = {}) {
  const ex = loadExamples();
  const userId = ex['event.user.message'].author.id;
  const me = {
    id: userId,
    email: 'dev@localhost',
    display_name: 'Dev',
    personal_project_id: '0199f3a2-7c10-7000-8000-0000000000d1',
    theme: 'warm',
    look: 'spacious',
    project_sort: 'custom',
    new_session_project_id: null,
    quick_messages: [],
  };
  const workerId = ex['hub.hello.ack'].payload.worker_id;
  const now = () => new Date().toISOString();
  // Projects and workspaces (T14): the personal project with one workspace on home-mac.
  /** @type {Map<string, any>} */
  const projects = new Map();
  /** @type {Map<string, any>} */
  const workspaces = new Map();
  /** Deleted workspaces, `folder_removed` when the worker deleted the folder (tests). */
  const removedWorkspaces = [];
  /** Clones and worktrees that wait for `finishCreating()`. */
  const held = [];
  const project = {
    ...ex['api.projects'].projects[0],
    id: me.personal_project_id,
    owner_id: userId,
    default_workspace_id: ex['api.workspace'].id,
  };
  projects.set(project.id, project);
  workspaces.set(ex['api.workspace'].id, { ...ex['api.workspace'], project_id: project.id, worker_id: workerId });
  // What `fs.list_dirs` shows on home-mac: allowed_roots = ['/Users/home-mac/work'].
  const ROOT = '/Users/home-mac/work';
  const TREE = {
    [ROOT]: ['cad', 'hqc', 'kicad'],
    [`${ROOT}/hqc`]: ['client', 'crates', 'docs', 'tools'],
    [`${ROOT}/hqc/crates`]: ['hqc-hub', 'hqc-proto', 'hqc-worker'],
    [`${ROOT}/kicad`]: ['power-board'],
  };
  const inRoot = (p) => p === ROOT || p.startsWith(`${ROOT}/`);
  const overlap = (a, b) => a === b || a.startsWith(`${b}/`) || b.startsWith(`${a}/`);
  const vapidPublicKey = generateKeyPairSync('ec', { namedCurve: 'P-256' })
    .publicKey.export({ format: 'jwk' });
  const vapid = Buffer.concat([
    Buffer.from([4]),
    Buffer.from(vapidPublicKey.x, 'base64url'),
    Buffer.from(vapidPublicKey.y, 'base64url'),
  ]).toString('base64url');
  const pushSubscriptions = [];
  const enrolled = [];
  const revoked = new Set();

  /** @type {Map<string, any>} */
  const sessions = new Map();
  /** The list order per project (T50): `{mode, session_ids}`; one user here. */
  const orders = new Map();
  const clients = new Set();
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms * speed));

  // ---- events ------------------------------------------------------------------------

  /** Builds an event from an example file: fresh ids and time, given session and payload. */
  function mk(s, example, payload = {}, extra = {}) {
    const t = structuredClone(ex[example]);
    t.session_id = s.id;
    t.event_id = randomUUID();
    t.ts = new Date().toISOString();
    delete t.seq;
    if (t.author.kind === 'agent') t.author.id = s.id;
    t.payload = { ...t.payload, ...payload };
    // `undefined` in `payload` drops an optional field the example has.
    for (const [k, v] of Object.entries(t.payload)) if (v === undefined) delete t.payload[k];
    for (const k of ['client_op_id', 'delivery', 'delivered_at', 'chain']) {
      if (!(k in extra) && k in t && example !== 'event.peer.message') delete t[k];
    }
    return Object.assign(t, extra);
  }

  function store(s, ev) {
    ev.seq = ++s.last_seq;
    s.events.push(ev);
    sendTo(s.id, ev);
    pushSummary(s);
    return ev;
  }

  /** A change of `delivery` of a stored input: a `delivery` frame, the event is not re-sent. */
  function setDelivery(s, ev, delivery) {
    ev.delivery = delivery;
    if (delivery === 'delivered') ev.delivered_at = new Date().toISOString();
    else delete ev.delivered_at;
    const frame = { type: 'delivery', session_id: s.id, event_id: ev.event_id, seq: ev.seq, delivery };
    if (ev.delivered_at) frame.delivered_at = ev.delivered_at;
    const data = JSON.stringify(frame);
    for (const c of clients) if (c.subs.has(s.id)) c.ws.send(data);
    pushSummary(s);
  }

  function delta(s, text) {
    sendTo(s.id, mk(s, 'event.agent.delta', { text }));
  }

  function sendTo(sessionId, event) {
    const data = JSON.stringify({ type: 'event', event });
    for (const c of clients) if (c.subs.has(sessionId)) c.ws.send(data);
  }

  /** SPEC §6: the final agent.message, requests, peer messages and system after the cursor. */
  function isUnread(e) {
    if (e.type === 'agent.message') return e.payload.final === true;
    // The worker's log lines in its service session do not count (SPEC §10.2).
    if (e.type === 'system' && e.payload.code === 'worker_log') return false;
    return ['approval.request', 'clarify.request', 'peer.message', 'system'].includes(e.type);
  }

  function summaryOf(s) {
    return {
      session_id: s.id,
      project_id: s.project_id,
      title: s.title,
      state: stateOf(s),
      queue_len: s.queue.length,
      unread: s.events.filter((e) => e.seq > s.read_seq && isUnread(e)).length,
      last_seq: s.last_seq,
      last_read_seq: s.read_seq,
      ...(s.held_by && !s.holder_hidden ? { held_by: s.held_by } : {}),
      ...modes(s),
    };
  }

  // T38: the mode hqc asks for and the one the mock worker runs (it allows auto).
  function modes(s) {
    return {
      ...(s.permission_mode ? { permission_mode: s.permission_mode } : {}),
      effective_permission_mode: s.permission_mode ?? 'ask',
    };
  }

  function pushSummary(s) {
    const data = JSON.stringify({ type: 'session.summary', session: summaryOf(s) });
    for (const c of clients) if (c.summary) c.ws.send(data);
  }

  function stateOf(s) {
    if (s.archived) return 'archived';
    if (s.locked === 'vscode') return 'locked_vscode';
    if (s.open) return 'waiting';
    if (s.events.some((e) => e.type === 'user.message' && e.delivery === 'failed')) return 'error';
    if (s.turn) return 'running';
    if (s.queue.length) return 'queued';
    return 'idle';
  }

  /** `model` of a create or `PATCH` body: absent stays, `null` is the runtime's default. */
  function agentSettings(s, body) {
    if (body?.model !== undefined) {
      if (body.model !== null && !/^(?!-)[A-Za-z0-9._:-]{1,64}$/.test(String(body.model))) {
        throw fail(400, 'bad_request', 'model must be 1..64 characters of A-Z a-z 0-9 . _ : -');
      }
      s.model = body.model ?? undefined;
    }
    // SPEC 0.148: only on a worker whose worker.toml has `browser = true`.
    if (body?.browser !== undefined) {
      if (typeof body.browser !== 'boolean') throw fail(400, 'bad_request', 'browser is true or false');
      if (s.system) throw fail(409, 'system_session', "the worker's service session has no agent");
      const w = allWorkers().find((x) => x.id === (workspaces.get(s.workspace_id)?.worker_id ?? workerId));
      if (body.browser && !w?.browser) {
        throw fail(422, 'browser_not_allowed', `worker ${w?.name ?? '?'} does not give a browser: add browser = true to its worker.toml`);
      }
      s.browser = body.browser;
    }
    return s;
  }

  /** The session object of SPEC §6 (hqc_proto::api::Session, examples/wire/api.session.json). */
  function info(s) {
    const sum = summaryOf(s);
    if (s.system) {
      const { workspace_id: _, ...rest } = info({ ...s, system: false });
      return { ...rest, agent: { ...rest.agent, runtime: 'none', runtime_session_id: undefined }, system: true };
    }
    return {
      id: s.id,
      project_id: s.project_id,
      owner_id: userId,
      workspace_id: s.workspace_id,
      worker_id: workspaces.get(s.workspace_id)?.worker_id ?? workerId,
      cwd: s.cwd,
      title: s.title,
      slug: s.slug,
      agent: {
        runtime: s.acp_agent ? 'acp' : 'claude_code',
        ...(s.acp_agent ? { acp_agent: s.acp_agent } : {}),
        ...(s.model ? { model: s.model } : {}),
        runtime_session_id: s.id,
        process_state: s.turn ? 'running' : 'stopped',
        disallowed_tools: [],
        restricted: false,
        start_failures: 0,
        ...(s.browser ? { browser: true } : {}),
      },
      auto_approve: false,
      wake_on_send: false,
      locked_by: s.locked ?? (s.turn ? 'hqc' : 'none'),
      last_seq: s.last_seq,
      archived: s.archived,
      ...(s.archived_at ? { archived_at: s.archived_at } : {}),
      state: sum.state,
      queue_len: sum.queue_len,
      last_read_seq: s.read_seq,
      unread: sum.unread,
      created_at: s.created_at,
      updated_at: s.events.at(-1)?.ts ?? s.created_at,
      ...(s.held_by && !s.holder_hidden ? { held_by: s.held_by } : {}),
      // As if a process of the session had reported them (T27): `/` completes them.
      commands: MOCK_COMMANDS,
      ...modes(s),
    };
  }

  function status(s, state, detail) {
    store(s, mk(s, 'event.status', { state, detail }));
  }

  // ---- sessions ----------------------------------------------------------------------

  function newSession(title, id = randomUUID(), where = {}) {
    const s = {
      id,
      project_id: where.project_id ?? project.id,
      workspace_id: where.workspace_id ?? project.default_workspace_id,
      cwd: where.cwd ?? '.',
      title,
      // Titles may be in Russian: Cyrillic letters stay in the slug.
      slug: title.toLowerCase().replace(/[^a-z0-9а-я]+/gi, '-').replace(/^-|-$/g, '') || 'session',
      archived: false,
      created_at: new Date().toISOString(),
      last_seq: 0,
      read_seq: 0,
      events: [],
      queue: [],
      turn: null,
      open: null,
    };
    sessions.set(id, s);
    store(s, mk(s, 'event.system'));
    return s;
  }

  // Session with a history that shows every card kind from examples/wire.
  {
    const s = newSession('hqc: test run', ex['event.user.message'].session_id);
    const u = store(s, mk(s, 'event.user.message', {}, {
      client_op_id: 'op-1', delivery: 'delivered', delivered_at: new Date().toISOString(),
    }));
    store(s, mk(s, 'event.agent.thinking'));
    store(s, mk(s, 'event.tool.call'));
    store(s, mk(s, 'event.tool.result'));
    store(s, mk(s, 'event.agent.message', { in_reply_to: u.event_id }));
    store(s, mk(s, 'event.usage'));
    store(s, mk(s, 'event.attachment'));
    store(s, mk(s, 'event.peer.message', {}, { delivery: 'delivered' }));
    store(s, mk(s, 'event.unknown'));
    status(s, 'idle');
    s.read_seq = 3;
  }
  {
    const s = newSession('Input not delivered');
    store(s, mk(s, 'event.user.message', { text: 'Build the release', artifact_ids: [] }, {
      client_op_id: randomUUID(), delivery: 'failed',
    }));
    store(s, mk(s, 'event.system', {
      text: 'The agent did not start, input was not delivered: claude: command not found',
      code: 'agent_start_failed',
    }));
  }
  newSession('Empty session');

  // The service session of home-mac (SPEC §10.2): no workspace, no agent, no input.
  {
    const s = {
      id: ex['api.session.system'].id,
      project_id: project.id,
      cwd: '.',
      title: 'home-mac',
      slug: `worker-${workerId}`,
      archived: false,
      system: true,
      created_at: new Date().toISOString(),
      last_seq: 0,
      read_seq: 0,
      events: [],
      queue: [],
      turn: null,
      open: null,
    };
    sessions.set(s.id, s);
    const log = (level, text) =>
      store(s, mk(s, 'event.system', { text, code: 'worker_log', level }));
    log('warn', 'hqc_worker::connection: hub connection lost: closed by hub; reconnecting in 1s');
    log('error', 'hqc_worker::supervisor: known folders not saved: Permission denied');
    const req = ex['worker.worker.approval.request'].payload;
    const ev = store(s, mk(s, 'event.approval.request', {
      request_id: req.request_id,
      tool: req.kind,
      description: req.description,
      options: [
        { id: 'once', label: 'Allow', canonical: 'once' },
        { id: 'deny', label: 'Deny', canonical: 'deny' },
      ],
      expires_at: new Date(Date.now() + 3_600_000).toISOString(),
      policy_hint: undefined,
    }, { author: { kind: 'system' } }));
    s.open = { kind: 'approval', request_id: req.request_id, event: ev, resolve: () => {} };
    log('warn', 'skipped 3');
  }

  // ---- turns -------------------------------------------------------------------------

  function enqueue(s, ev) {
    s.queue.push(ev);
    pushSummary(s);
    if (!s.turn) void runQueue(s);
  }

  async function runQueue(s) {
    while (s.queue.length && !s.turn) {
      const input = s.queue.shift();
      s.turn = { aborted: false };
      setDelivery(s, input, 'delivered');
      pushSummary(s);
      try {
        await turn(s, input, s.turn);
      } catch (e) {
        if (e !== ABORT) throw e;
      }
      s.turn = null;
      s.open = null;
      status(s, 'idle');
    }
  }

  const ABORT = Symbol('abort');

  async function step(t, ms) {
    await sleep(ms);
    if (t.aborted) throw ABORT;
  }

  async function stream(s, t, text) {
    for (const chunk of text.match(/\S+\s*/g) ?? []) {
      delta(s, chunk);
      await step(t, 120);
    }
  }

  async function turn(s, input, t) {
    const askClarify = /question|clarify|вопрос/i.test(input.payload.text);
    // SPEC 0.147: the agent asks for a secret with hqc_secret; the owner gives it in the card.
    const askSecret = /\bsecret\b|password|парол/i.test(input.payload.text) && !/HQC_SECRETS/.test(input.payload.text);
    status(s, 'running');
    await step(t, 300);

    if (/markdown|разметк/i.test(input.payload.text)) {
      await stream(s, t, MARKDOWN_ANSWER);
      store(s, mk(s, 'event.agent.message', { text: MARKDOWN_ANSWER, final: true, in_reply_to: input.event_id }));
      store(s, mk(s, 'event.usage'));
      return;
    }

    const intro = 'Let me look at the project and run the tests.';
    await stream(s, t, intro);
    store(s, mk(s, 'event.agent.message', { text: intro, final: false, in_reply_to: input.event_id }));

    const readId = `toolu_${randomUUID().slice(0, 8)}`;
    store(s, mk(s, 'event.tool.call', {
      call_id: readId, tool: 'Read', args_preview: '{"file_path":"Cargo.toml"}',
    }));
    await step(t, 400);
    store(s, mk(s, 'event.tool.result', {
      call_id: readId, result_preview: '[workspace]\nmembers = ["crates/*"]', error: false, duration_ms: 12,
      error_text: undefined, artifact_id: undefined,
    }));
    await step(t, 300);

    let final;
    if (askSecret) {
      const request_id = `toolu_${randomUUID().slice(0, 8)}`;
      const answer = await request(s, t, 'approval', mk(s, 'event.approval.request', {
        request_id,
        tool: 'hqc_secret',
        description: 'The Wi-Fi password to write into the camera config',
        options: [{ id: 'deny', label: 'Decline', canonical: 'deny' }],
        secret: { name: 'WIFI_PASS', kind: 'secret', vault: 'home-wifi', reads_vault: true },
        expires_at: undefined,
        policy_hint: undefined,
        rule_suggestions: undefined,
      }));
      if (answer.cancel) return;
      status(s, 'running');
      final = answer.option_id === 'give'
        ? 'Got it: the password is in "$HQC_SECRETS/WIFI_PASS"; writing it into the config.'
        : 'OK, no password: leaving the Wi-Fi unset.';
    } else if (askClarify) {
      const request_id = `req-${randomUUID().slice(0, 8)}`;
      const answer = await request(s, t, 'clarify', mk(s, 'event.clarify.request', { request_id }));
      if (answer.cancel) return;
      final = `Got it: ${Object.entries(answer.answers)
        .map(([k, v]) => `${k} = ${Array.isArray(v) ? v.join(', ') : v}`).join('; ') || 'no answers'}.`;
    } else {
      const call_id = `toolu_${randomUUID().slice(0, 8)}`;
      store(s, mk(s, 'event.tool.call', { call_id }));
      const request_id = `req-${randomUUID().slice(0, 8)}`;
      const answer = await request(s, t, 'approval', mk(s, 'event.approval.request', {
        request_id, expires_at: new Date(Date.now() + 3600_000).toISOString(),
      }));
      if (answer.cancel) return;
      const opt = ex['event.approval.request'].payload.options.find((o) => o.id === answer.option_id);
      status(s, 'running');
      if (opt?.canonical?.startsWith('deny')) {
        store(s, mk(s, 'event.tool.result', {
          call_id, result_preview: '', error: true, error_text: 'Denied by user', duration_ms: 0,
          artifact_id: undefined,
        }));
        final = answer.message ? `OK, not running the tests. You wrote: “${answer.message}”.` : 'OK, not running the tests.';
      } else {
        await step(t, 800);
        store(s, mk(s, 'event.tool.result', {
          call_id, result_preview: 'test result: ok. 42 passed; 0 failed', error: false, duration_ms: 5120,
          error_text: undefined, artifact_id: undefined,
        }));
        final = 'All 42 tests passed.';
      }
    }
    await step(t, 200);
    await stream(s, t, final);
    store(s, mk(s, 'event.agent.message', { text: final, final: true, in_reply_to: input.event_id }));
    store(s, mk(s, 'event.usage'));
  }

  /** Stores a request event and waits for its answer or cancellation. */
  function request(s, t, kind, ev) {
    return new Promise((resolve, reject) => {
      s.open = { kind, request_id: ev.payload.request_id, event: ev, resolve, t };
      store(s, ev);
      status(s, 'waiting', kind === 'approval' ? 'approval requested' : 'clarify requested');
      t.onAbort = () => reject(ABORT);
    });
  }

  function cancelOpen(s, reason) {
    const o = s.open;
    if (!o) return;
    s.open = null;
    store(s, mk(s, o.kind === 'approval' ? 'event.approval.cancel' : 'event.clarify.cancel', {
      request_id: o.request_id, reason,
    }));
    o.resolve({ cancel: reason });
  }

  // ---- HTTP --------------------------------------------------------------------------

  const routes = [];
  const route = (method, pattern, fn) => {
    const keys = [];
    const re = new RegExp(`^${pattern.replace(/:(\w+)/g, (_, k) => (keys.push(k), '([^/]+)'))}$`);
    routes.push({ method, re, keys, fn });
  };
  const fail = (status, code, message, extra) => Object.assign(new Error(message), { status, code, extra });
  const projectOr404 = (id) => {
    const p = projects.get(id);
    if (!p) throw fail(404, 'not_found', 'unknown project');
    return p;
  };
  const workerOnline = (id) =>
    id === workerId && !revoked.has(id);
  /** A reply with a status other than 200. */
  const withStatus = (status, body) => ({ [STATUS]: status, body });
  const sessionOr404 = (id) => {
    const s = sessions.get(id);
    if (!s) throw fail(404, 'not_found', 'unknown session');
    return s;
  };

  route('GET', '/api/me', () => me);
  // SPEC 0.144: the owner's model providers; the mock's one worker allows each it keeps, so
  // the model menus offer them.
  const providers = new Map();
  const providerFields = (body) => {
    // As the hub's `ProviderRequest`: no other field.
    const extra = Object.keys(body ?? {}).filter((k) => !['name', 'label', 'base_url', 'model', 'context_tokens'].includes(k));
    if (extra.length) throw fail(422, 'unprocessable', `unknown field \`${extra[0]}\``);
    const f = { name: String(body?.name ?? '').trim(), label: String(body?.label ?? '').trim(), base_url: String(body?.base_url ?? '').trim().replace(/\/+$/, ''), model: String(body?.model ?? '').trim() };
    if (!/^[A-Za-z0-9_-]+$/.test(f.name) || !f.label || !/^https?:\/\//.test(f.base_url) || !f.model) throw fail(400, 'bad_request', 'name, label, base_url and model are needed');
    if (body?.context_tokens !== undefined) f.context_tokens = body.context_tokens;
    return f;
  };
  const providerView = (p) => ({ ...p, workers: allWorkers().filter((w) => w.status !== "revoked").map((w) => w.id) });
  route('GET', '/api/providers', () => ({ providers: [...providers.values()].map(providerView) }));
  route('POST', '/api/providers', (_, body) => {
    const f = providerFields(body);
    if ([...providers.values()].some((p) => p.name === f.name)) throw fail(409, 'provider_exists', `you have a provider named "${f.name}"`);
    const p = { id: randomUUID(), ...f, created_at: now(), updated_at: now() };
    providers.set(p.id, p);
    return withStatus(201, providerView(p));
  });
  route('PATCH', '/api/providers/:id', (p, body) => {
    const old = providers.get(p.id);
    if (!old) throw fail(404, 'not_found', 'unknown provider');
    const next = { ...old, ...providerFields(body), updated_at: now() };
    providers.set(p.id, next);
    return providerView(next);
  });
  // SPEC 0.236: the models at a base URL, as the mock's one worker reads them.
  route('POST', '/api/providers/probe', (_, body) => {
    const base = String(body?.base_url ?? '').trim();
    if (!/^https?:\/\/\S+$/.test(base)) throw fail(400, 'bad_request', 'base_url must be an http:// or https:// URL');
    const online = allWorkers().find((w) => w.status === 'online');
    if (!online) throw fail(409, 'worker_offline', 'no worker of yours is online to read the endpoint');
    return { models: [{ id: 'qwen3.8-27b', context_tokens: 131072 }, { id: 'qwen3.8-flash-next' }], worker_id: online.id };
  });
  route('DELETE', '/api/providers/:id', (p) => {
    if (!providers.delete(p.id)) throw fail(404, 'not_found', 'unknown provider');
    return withStatus(204, null);
  });
  // SPEC 0.235: the owner's remote MCP servers. The mock reaches no server: `test` flips the
  // status between ok and failing. Header values are dropped at once, only their names are
  // kept, as the hub keeps them sealed and never sends them back. A server named `no-key`
  // with headers gets `422 mcp_key_missing`, as a hub without HQC_MCP_KEY answers.
  const mcpServers = new Map();
  const MCP_NAME = (n) => /^[a-z0-9_-]{1,32}$/.test(n) && n !== 'hqc' && !n.startsWith('_') && !n.endsWith('_') && !n.includes('__');
  {
    const id = randomUUID();
    mcpServers.set(id, {
      id,
      name: 'github',
      url: 'https://api.githubcopilot.com/mcp/',
      header_names: ['Authorization'],
      timeout_s: 300,
      all_projects: true,
      project_ids: [],
      status: 'ok',
      tools: ['get_me', 'create_issue'],
      auth: 'headers',
      signed_in: false,
    });
    const lid = randomUUID();
    mcpServers.set(lid, {
      id: lid,
      name: 'linear',
      url: 'https://mcp.linear.app/mcp',
      header_names: [],
      timeout_s: 300,
      all_projects: true,
      project_ids: [],
      status: 'sign_in',
      tools: [],
      auth: 'oauth',
      signed_in: false,
    });
  }
  /** Checks the fields of a POST or PATCH body and puts them into `into`. */
  const mcpFields = (body, into) => {
    const known = ['name', 'url', 'headers', 'timeout_s', 'all_projects', 'project_ids', 'auth', 'oauth_client_id', 'oauth_client_secret', 'oauth_scope'];
    const extra = Object.keys(body ?? {}).filter((k) => !known.includes(k));
    if (extra.length) throw fail(422, 'unprocessable', `unknown field \`${extra[0]}\``);
    if (body.name !== undefined) {
      if (typeof body.name !== 'string' || !MCP_NAME(body.name)) throw fail(400, 'bad_request', 'a bad server name');
      if ([...mcpServers.values()].some((x) => x.name === body.name && x.id !== into.id)) throw fail(409, 'name_taken', `you have a server named "${body.name}"`);
      into.name = body.name;
    }
    if (body.url !== undefined) {
      let u;
      try {
        u = new URL(body.url);
      } catch {
        throw fail(400, 'bad_request', 'url: not a URL');
      }
      const loopback = /^(localhost|127(\.\d+){3}|\[::1\])$/.test(u.hostname);
      if (!(u.protocol === 'https:' || (u.protocol === 'http:' && loopback)) || u.username || u.password) throw fail(400, 'bad_request', 'url: https only, without a user or password');
      into.url = body.url;
    }
    if (body.timeout_s !== undefined) {
      if (!Number.isInteger(body.timeout_s) || body.timeout_s < 30 || body.timeout_s > 1800) throw fail(400, 'bad_request', 'timeout_s: 30 to 1800');
      into.timeout_s = body.timeout_s;
    }
    if (body.all_projects !== undefined) into.all_projects = !!body.all_projects;
    // T65: OAuth; the secret goes nowhere, a change asks for a new sign-in.
    if (body.auth !== undefined) {
      if (!['headers', 'oauth'].includes(body.auth)) throw fail(400, 'bad_request', 'auth is headers or oauth');
      into.auth = body.auth;
      into.signed_in = false;
      into.status = body.auth === 'oauth' ? 'sign_in' : 'unknown';
    }
    if (body.oauth_client_id !== undefined) into.oauth_client_id = body.oauth_client_id || undefined;
    if (body.oauth_scope !== undefined) into.oauth_scope = body.oauth_scope || undefined;
    if (body.project_ids !== undefined) {
      if (!Array.isArray(body.project_ids) || body.project_ids.some((id) => !projects.has(id))) throw fail(400, 'bad_request', 'project_ids: not your projects');
      into.project_ids = [...body.project_ids];
    }
    if (body.headers !== undefined) {
      if (!Array.isArray(body.headers) || body.headers.length > 8) throw fail(400, 'bad_request', 'headers: at most 8');
      for (const h of body.headers) {
        if (!/^[A-Za-z0-9_-]+$/.test(h?.name ?? '') || typeof h.value !== 'string' || !h.value || /[\r\n]/.test(h.value)) throw fail(400, 'bad_request', 'a bad header');
      }
      if (body.headers.length && (into.name ?? body.name) === 'no-key') throw fail(422, 'mcp_key_missing', 'the hub has no HQC_MCP_KEY: it cannot keep header values');
      // The names only: the values go nowhere.
      into.header_names = body.headers.map((h) => h.name);
      into.status = 'unknown';
      into.tools = [];
      delete into.last_error;
    }
    if (body.url !== undefined) {
      into.status = 'unknown';
      into.tools = [];
      delete into.last_error;
    }
    return into;
  };
  // SPEC 0.236: how a server signs in, guessed from its host (the mock reaches no server).
  route('POST', '/api/mcp/probe', (_, body) => {
    let u;
    try {
      u = new URL(String(body?.url ?? ''));
    } catch {
      throw fail(400, 'bad_request', 'url: not a URL');
    }
    const loopback = /^(localhost|127(\.\d+){3}|\[::1\])$/.test(u.hostname);
    if (!(u.protocol === 'https:' || (u.protocol === 'http:' && loopback)) || u.username || u.password) throw fail(400, 'bad_request', 'url: https only, without a user or password');
    const labels = u.hostname.split('.');
    const label = labels.slice(0, Math.max(1, labels.length - 1)).find((l) => !['mcp', 'api', 'www', 'server'].includes(l));
    const n = (label ?? '').toLowerCase().replace(/[^a-z0-9_-]/g, '-').replace(/-+/g, '-').replace(/^[-_]+|[-_]+$/g, '').slice(0, 32);
    const auth = /linear|notion/.test(u.hostname) ? 'oauth' : /githubcopilot/.test(u.hostname) ? 'token' : 'none';
    return { name: MCP_NAME(n) ? n : null, server_name: null, auth };
  });
  const mcpOr404 = (id) => {
    const s = mcpServers.get(id);
    if (!s) throw fail(404, 'not_found', 'unknown server');
    return s;
  };
  const mcpList = () => [...mcpServers.values()].sort((a, b) => a.name.localeCompare(b.name));
  // SPEC 0.262: the servers a worker's worker.toml declares, read-only.
  route('GET', '/api/mcp/servers', () => ({
    servers: mcpList(),
    local: [{ name: 'figma', worker_id: 'w-drzlo-mac', worker_name: 'drzlo-mac', environment: 'firmware' }],
  }));
  route('POST', '/api/mcp/servers', (_, body) => {
    if (typeof body?.name !== 'string' || typeof body?.url !== 'string') throw fail(400, 'bad_request', 'name and url are needed');
    if (mcpServers.size >= 50) throw fail(409, 'too_many_servers', 'at most 50 servers');
    const id = randomUUID();
    const s = mcpFields(body, { id, header_names: [], timeout_s: 300, all_projects: false, project_ids: [], status: 'unknown', tools: [], auth: 'headers', signed_in: false });
    mcpServers.set(id, s);
    return withStatus(201, s);
  });
  route('PATCH', '/api/mcp/servers/:id', (p, body) => {
    const next = mcpFields(body, { ...mcpOr404(p.id) });
    mcpServers.set(p.id, next);
    return next;
  });
  route('DELETE', '/api/mcp/servers/:id', (p) => {
    mcpOr404(p.id);
    mcpServers.delete(p.id);
    return withStatus(204, null);
  });
  // T65: the mock signs in at once; the page it gives is the hub's own callback page.
  route('POST', '/api/mcp/servers/:id/sign_in', (p) => {
    const s = mcpOr404(p.id);
    if (s.auth !== 'oauth') throw fail(400, 'bad_request', 'this server signs in with headers');
    mcpServers.set(s.id, { ...s, signed_in: true, status: 'unknown' });
    return { url: 'https://example.com/oauth/authorize?mock=1' };
  });
  route('DELETE', '/api/mcp/servers/:id/sign_in', (p) => {
    const s = mcpOr404(p.id);
    mcpServers.set(s.id, { ...s, signed_in: false, status: 'sign_in' });
    return withStatus(204, null);
  });
  route('POST', '/api/mcp/servers/:id/test', (p) => {
    const s = mcpOr404(p.id);
    const next =
      s.status === 'ok'
        ? { ...s, status: 'failing', last_error: 'the server answered 401 Unauthorized' }
        : { ...s, status: 'ok', tools: s.tools.length ? s.tools : ['echo', 'get_time'] };
    if (next.status === 'ok') delete next.last_error;
    mcpServers.set(p.id, next);
    return next;
  });
  // SPEC 0.245 (iteration 8): the owner's skills. A folder comes as `file` parts (paths in
  // the file names), a `.zip` as one `zip` part; the mock does not open a zip, it takes the
  // `name` part for its name. A `SKILL.md` with `hooks` is refused, as the hub refuses it.
  const skills = new Map();
  const SKILL_NAME = /^[a-z0-9][a-z0-9_-]{0,63}$/;
  {
    const sample = (name, description, allowed_tools, files, size, all_projects) => {
      const id = randomUUID();
      skills.set(id, {
        id,
        name,
        description,
        allowed_tools,
        sha256: createHash('sha256').update(name).digest('hex'),
        size,
        files,
        all_projects,
        project_ids: [],
        updated_at: now(),
        file_list: [{ path: 'SKILL.md', size: 1200 }, ...(files > 1 ? [{ path: 'scripts/run.sh', size: size - 1200, executable: true }] : [])],
      });
    };
    sample('bench', 'Runs the benchmarks and compares them with the last run.', ['Bash(cargo bench:*)', 'Read'], 2, 5300, true);
    sample('release-notes', 'Writes release notes from the merged pull requests.', [], 1, 1900, false);
  }
  /** The parts of a multipart body: `{name, filename?, data}`. */
  const multipartParts = (raw) => {
    if (!Buffer.isBuffer(raw)) return [];
    const eol = raw.indexOf('\r\n');
    if (eol < 0) return [];
    const delim = raw.subarray(0, eol);
    const parts = [];
    let at = eol + 2;
    for (;;) {
      const next = raw.indexOf(delim, at);
      if (next < 0) break;
      const part = raw.subarray(at, next - 2);
      const head = part.indexOf('\r\n\r\n');
      if (head >= 0) {
        const headers = part.subarray(0, head).toString('utf8');
        const name = /name="([^"]*)"/.exec(headers)?.[1];
        const filename = /filename="([^"]*)"/.exec(headers)?.[1];
        if (name) parts.push({ name, filename, data: part.subarray(head + 4) });
      }
      at = next + delim.length + 2;
      if (raw.subarray(next + delim.length, next + delim.length + 2).toString() === '--') break;
    }
    return parts;
  };
  /** The frontmatter's `key: value` lines of a SKILL.md. */
  const frontmatter = (text) => {
    const m = /^---\r?\n([\s\S]*?)\r?\n---/.exec(text);
    const out = {};
    for (const line of (m?.[1] ?? '').split(/\r?\n/)) {
      const kv = /^([A-Za-z_-]+):\s*(.*)$/.exec(line);
      if (kv) out[kv[1]] = kv[2].trim();
    }
    return out;
  };
  const skillOr404 = (id) => {
    const s = skills.get(id);
    if (!s) throw fail(404, 'not_found', 'unknown skill');
    return s;
  };
  const skillView = ({ file_list, ...s }) => s;
  route('GET', '/api/skills', () => ({ skills: [...skills.values()].sort((a, b) => a.name.localeCompare(b.name)).map(skillView) }));
  route('POST', '/api/skills', (_, body) => {
    const parts = multipartParts(body);
    const zip = parts.find((x) => x.name === 'zip');
    const files = parts.filter((x) => x.name === 'file');
    const given = parts.find((x) => x.name === 'name')?.data.toString('utf8');
    let fm = {};
    let fileList;
    if (zip) {
      if (files.length) throw fail(400, 'bad_request', 'either a zip or files');
      fileList = [{ path: 'SKILL.md', size: zip.data.length }];
    } else {
      if (!files.length) throw fail(400, 'bad_request', 'no files');
      for (const f of files) {
        if (!f.filename || f.filename.startsWith('/') || f.filename.split('/').includes('..')) throw fail(400, 'bad_request', `a bad path: ${f.filename}`);
      }
      const md = files.find((f) => f.filename === 'SKILL.md');
      if (!md) throw fail(400, 'bad_request', 'no SKILL.md at the top of the skill');
      fm = frontmatter(md.data.toString('utf8'));
      if ('hooks' in fm) throw fail(422, 'skill_hooks', 'SKILL.md has hooks, which would run outside the worker’s deny; remove them');
      fileList = files.map((f) => ({ path: f.filename, size: f.data.length })).sort((a, b) => (a.path < b.path ? -1 : 1));
    }
    const name = fm.name || given || '';
    if (!SKILL_NAME.test(name)) throw fail(400, 'bad_request', `a bad skill name: "${name}"`);
    const size = fileList.reduce((n, f) => n + f.size, 0);
    if (size > 4 << 20) throw fail(413, 'too_large', 'a skill is at most 4 MiB unpacked');
    const old = [...skills.values()].find((x) => x.name === name);
    if (!old && skills.size >= 100) throw fail(409, 'too_many_skills', 'at most 100 skills');
    const s = {
      id: old?.id ?? randomUUID(),
      name,
      description: fm.description || undefined,
      allowed_tools: (fm['allowed-tools'] ?? '').split(/,\s*/).filter(Boolean),
      sha256: createHash('sha256').update(body).digest('hex'),
      size,
      files: fileList.length,
      all_projects: old?.all_projects ?? true,
      project_ids: old?.project_ids ?? [],
      updated_at: now(),
      file_list: fileList,
    };
    skills.set(s.id, s);
    return withStatus(old ? 200 : 201, skillView(s));
  });
  route('PATCH', '/api/skills/:id', (p, body) => {
    const s = { ...skillOr404(p.id) };
    if (body?.all_projects !== undefined) s.all_projects = !!body.all_projects;
    if (body?.project_ids !== undefined) {
      if (!Array.isArray(body.project_ids) || body.project_ids.some((id) => !projects.has(id))) throw fail(400, 'bad_request', 'project_ids: not your projects');
      s.project_ids = [...body.project_ids];
    }
    s.updated_at = now();
    skills.set(s.id, s);
    return skillView(s);
  });
  route('DELETE', '/api/skills/:id', (p) => {
    skillOr404(p.id);
    skills.delete(p.id);
    return withStatus(204, null);
  });
  route('GET', '/api/skills/:id/files', (p) => ({ files: skillOr404(p.id).file_list }));
  // SPEC 0.147: a secret or an ssh key for a session's agent, through the worker; the mock
  // keeps no value. The keys a workspace remembers, by workspace id (names and fingerprints).
  const workspaceKeys = new Map();
  const SECRET_NAME = /^(?![.-])[A-Za-z0-9_.-]{1,64}$/;
  const fingerprint = (seed) => `SHA256:${Buffer.from(seed).toString('base64').replace(/=+$/, '').slice(0, 43)}`;
  route('POST', '/api/sessions/:id/secrets', (p, body) => {
    const s = sessionOr404(p.id);
    const extra = Object.keys(body ?? {}).filter((k) => !['kind', 'name', 'value', 'vault', 'vault_field', 'passphrase', 'remember', 'request_id'].includes(k));
    if (extra.length) throw fail(400, 'bad_request', `unknown field \`${extra[0]}\``);
    const { kind, name, vault, vault_field: field } = body ?? {};
    // The worker reads a vault entry itself: the mock has no value then, only a stand-in.
    const value = vault !== undefined ? 'PRIVATE KEY (from the vault)' : body?.value;
    if (!['secret', 'ssh_key'].includes(kind)) throw fail(400, 'bad_request', 'kind is secret or ssh_key');
    if (typeof name !== 'string' || !SECRET_NAME.test(name)) throw fail(422, 'secret_refused', 'a bad name');
    if ((body?.value !== undefined) === (vault !== undefined)) throw fail(400, 'bad_request', 'give exactly one of value and vault');
    if (vault !== undefined && (typeof vault !== 'string' || !vault.trim())) throw fail(400, 'bad_request', 'vault is one line of 1 to 200 characters');
    if (field !== undefined && vault === undefined) throw fail(400, 'bad_request', 'vault_field needs vault');
    if (field !== undefined && (typeof field !== 'string' || !field.trim())) throw fail(400, 'bad_request', 'vault_field is one line of 1 to 200 characters');
    if (typeof value !== 'string' || !value) throw fail(400, 'bad_request', 'value is required');
    if (s.system) throw fail(409, 'unavailable', 'a service session takes no secret');
    if (kind === 'ssh_key' && !value.includes('PRIVATE KEY')) throw fail(422, 'secret_refused', 'not an OpenSSH private key');
    if (kind === 'ssh_key' && /ENCRYPTED/.test(value) && body.passphrase !== 'pass') throw fail(422, 'secret_refused', 'a wrong passphrase');
    let o;
    if (body.request_id !== undefined) {
      o = s.open;
      if (!o || o.request_id !== body.request_id || o.event.payload.tool !== 'hqc_secret') throw fail(409, 'request_closed', 'the request is already closed');
      const asked = o.event.payload.secret;
      if (asked.name !== name || asked.kind !== kind) throw fail(400, 'bad_request', `the request asks for ${asked.kind} ${asked.name}`);
    }
    const fp = kind === 'ssh_key' ? fingerprint(value) : undefined;
    const remembered = kind === 'ssh_key' && body.remember === true && !!s.workspace_id;
    if (remembered) {
      const keys = workspaceKeys.get(s.workspace_id) ?? [];
      if (keys.some((k) => k.name === name)) throw fail(422, 'secret_refused', `this workspace has a key named ${name}`);
      workspaceKeys.set(s.workspace_id, [...keys, { name, fingerprint: fp }]);
    }
    const from = vault !== undefined ? ` from Vaultwarden entry ${vault}${field !== undefined ? `, field ${field}` : ''}` : '';
    const text = kind === 'ssh_key'
      ? `ssh key ${name} (${fp}) given${from}${remembered ? ', remembered for this workspace' : ''}`
      : `Secret ${name} given${from}`;
    store(s, mk(s, 'event.system', { text, code: 'secret_given' }));
    if (o) {
      s.open = null;
      store(s, mk(s, 'event.approval.response', { request_id: o.request_id, option_id: 'give', by: userId, message: undefined }));
      o.resolve({ option_id: 'give' });
    }
    return kind === 'ssh_key'
      ? { note: `The ssh key ${name} (${fp}) is loaded in your ssh-agent.`, fingerprint: fp, ...(remembered ? { remembered } : {}) }
      : {
          note: `The secret ${name} is in the file "$HQC_SECRETS/${name}" for as long as your process runs: read it in a command, as "$(cat "$HQC_SECRETS/${name}")", and do not print it.`,
          path: `$HQC_SECRETS/${name}`,
        };
  });
  route('GET', '/api/workspaces/:id/keys', (p) => {
    if (!workspaces.has(p.id)) throw fail(404, 'not_found', 'unknown workspace');
    return { keys: workspaceKeys.get(p.id) ?? [] };
  });
  route('DELETE', '/api/workspaces/:id/keys/:name', (p) => {
    const keys = workspaceKeys.get(p.id) ?? [];
    if (!keys.some((k) => k.name === p.name)) throw fail(404, 'not_found', 'no such key');
    workspaceKeys.set(p.id, keys.filter((k) => k.name !== p.name));
    return withStatus(204, null);
  });
  // T49: the app theme; the look and the project bar's order (SPEC §3 User); the project's color
  // and icon, the icon image (checked loosely: the hub checks the magic bytes and the size in
  // pixels).
  route('PATCH', '/api/me', (_, body) => {
    if (body?.theme !== undefined) {
      if (!['warm', 'graphite', 'contrast', 'warm_light', 'graphite_light', 'contrast_light'].includes(body.theme)) {
        throw fail(422, 'unprocessable', 'unknown theme');
      }
      me.theme = body.theme;
    }
    if (body?.look !== undefined) {
      if (!['compact', 'spacious'].includes(body.look)) throw fail(422, 'unprocessable', 'unknown look');
      me.look = body.look;
    }
    if (body?.project_sort !== undefined) {
      if (!['custom', 'name', 'recent'].includes(body.project_sort)) throw fail(422, 'unprocessable', 'unknown project order');
      me.project_sort = body.project_sort;
    }
    if (body?.new_session_project_id !== undefined) {
      if (!projects.has(body.new_session_project_id)) throw fail(404, 'not_found', 'unknown project');
      me.new_session_project_id = body.new_session_project_id;
    }
    const quick = body?.quick_messages;
    if (quick !== undefined) {
      if (quick.length > 100) throw fail(400, 'bad_request', 'more than 100 quick messages');
      if (quick.some((m) => !m.trim())) throw fail(400, 'bad_request', 'a quick message is blank');
      if (quick.some((m) => [...m].length > 10000)) throw fail(400, 'bad_request', 'a quick message is longer than 10000 characters');
      me.quick_messages = quick;
    }
    if (body?.timezone !== undefined) me.timezone = body.timezone;
    return me;
  });
  /** @type {Map<string, Buffer>} */
  const icons = new Map();
  route('PUT', '/api/projects/:pid/icon', (p, body) => {
    const pr = projectOr404(p.pid);
    const raw = Buffer.isBuffer(body) ? body : Buffer.alloc(0);
    const start = raw.indexOf(Buffer.from('\x89PNG', 'latin1'));
    if (start < 0) throw fail(422, 'invalid_icon', 'the image is not a PNG, JPEG or WebP');
    icons.set(pr.id, raw.subarray(start));
    pr.icon = { kind: 'image', url: `/api/projects/${pr.id}/icon?v=${icons.size}${Date.now() % 1000}` };
    return pr;
  });
  route('GET', '/api/projects/:pid/icon', (p) => {
    const data = icons.get(projectOr404(p.pid).id);
    if (!data) throw fail(404, 'not_found', 'no icon');
    return { [RAW]: { type: 'image/png', data } };
  });
  // The user's order of the project bar (SPEC 0.64): the rest last, oldest first. Archived
  // projects are left out, or with `archived` only they are listed.
  let projectOrder = [];
  const allProjects = () => {
    const placed = projectOrder.map((id) => projects.get(id)).filter(Boolean);
    return [...placed, ...[...projects.values()].filter((p) => !projectOrder.includes(p.id))];
  };
  const orderedProjects = (archived = false) => allProjects().filter((p) => !!p.archived_at === archived);
  route('GET', '/api/projects', (_, __, q) => ({ projects: orderedProjects(q.get('archived') === '1') }));
  // As the hub: an archived project the new order leaves out keeps the place it had in the
  // order the bar had, right after the one before it when that one is there, else at its old
  // index.
  route('PUT', '/api/me/project_order', (_, body) => {
    const next = [...new Set(body?.project_ids ?? [])].filter((id) => projects.has(id));
    const old = allProjects().map((p) => p.id);
    old.forEach((id, i) => {
      if (!projects.get(id)?.archived_at || next.includes(id)) return;
      const after = i > 0 ? next.indexOf(old[i - 1]) : -1;
      next.splice(after >= 0 ? after + 1 : Math.min(i, next.length), 0, id);
    });
    projectOrder = next;
    return { projects: orderedProjects() };
  });
  route('POST', '/api/projects/:pid/archive', (p) => {
    const pr = projectOr404(p.pid);
    if (pr.is_personal) throw fail(409, 'personal_project', 'the personal project cannot be archived');
    pr.archived_at ??= now();
    return pr;
  });
  route('POST', '/api/projects/:pid/unarchive', (p) => {
    const pr = projectOr404(p.pid);
    delete pr.archived_at;
    return pr;
  });
  function newProject(name, description = '') {
    const base = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'project';
    let slug = base;
    for (let i = 2; [...projects.values()].some((p) => p.slug === slug); i++) slug = `${base}-${i}`;
    const p = {
      id: randomUUID(), owner_id: userId, name, slug, description,
      is_personal: false, color: COLORS[projects.size % COLORS.length], created_at: now(), updated_at: now(),
      quick_messages: [],
    };
    projects.set(p.id, p);
    return p;
  }
  route('POST', '/api/projects', (_, body) => {
    const name = String(body?.name ?? '').trim();
    if (!name) throw fail(400, 'bad_request', 'name must not be empty');
    return withStatus(201, newProject(name, String(body?.description ?? '')));
  });
  route('GET', '/api/projects/:pid', (p) => projectOr404(p.pid));
  route('PATCH', '/api/projects/:pid', (p, body) => {
    const pr = projectOr404(p.pid);
    if (body?.name !== undefined && pr.is_personal && body.name !== pr.name) {
      throw fail(409, 'personal_project', 'the personal project cannot be renamed');
    }
    if (body?.default_workspace_id !== undefined && workspaces.get(body.default_workspace_id)?.project_id !== pr.id) {
      throw fail(400, 'bad_request', 'default_workspace_id is not a workspace of this project');
    }
    if (body?.color !== undefined && !COLORS.includes(body.color)) throw fail(422, 'unprocessable', 'unknown color');
    if (body?.icon !== undefined) {
      const ok = ['emoji', 'letters'].includes(body.icon?.kind) && String(body.icon.value ?? '').trim();
      if (!ok) throw fail(422, 'invalid_icon', 'an emoji or 1-2 letters');
      icons.delete(pr.id);
      body.icon = { kind: body.icon.kind, value: body.icon.kind === 'letters' ? body.icon.value.toUpperCase() : body.icon.value };
    }
    const quick = body?.quick_messages;
    if (quick !== undefined) {
      if (quick.length > 100) throw fail(400, 'bad_request', 'more than 100 quick messages');
      if (quick.some((m) => !m.trim())) throw fail(400, 'bad_request', 'a quick message is blank');
      if (quick.some((m) => [...m].length > 10000)) throw fail(400, 'bad_request', 'a quick message is longer than 10000 characters');
    }
    for (const k of ['name', 'description', 'default_workspace_id', 'color', 'icon', 'quick_messages']) if (body?.[k] !== undefined) pr[k] = body[k];
    pr.updated_at = now();
    return pr;
  });
  route('DELETE', '/api/projects/:pid', (p) => {
    const pr = projectOr404(p.pid);
    if (pr.is_personal) throw fail(409, 'personal_project', 'the personal project cannot be deleted');
    for (const [id, s] of sessions) if (s.project_id === pr.id) sessions.delete(id);
    for (const [id, w] of workspaces) if (w.project_id === pr.id) workspaces.delete(id);
    projects.delete(pr.id);
    return withStatus(204, null);
  });
  route('GET', '/api/projects/:pid/sessions', (p) => ({
    sessions: [...sessions.values()].filter((s) => s.project_id === p.pid).map(info),
  }));
  route('GET', '/api/sessions', () => ({ sessions: [...sessions.values()].map(info) }));
  // `GET /api/search` (SPEC §15) over the mock's own sessions, in the hub's shape: a word
  // finds the words of a text that begin with it less its last letter (a stand-in for the
  // hub's stemmer, so other forms are found and named in `also`); every word must be found;
  // newest first whatever the order asked. Filters, counts on the first page (not of one
  // session), the hits of a hit's session, at most `per_session` of each, pages by an offset.
  const KIND_OF = {
    'user.message': 'message', 'agent.message': 'message', 'peer.message': 'message', system: 'message',
    'tool.call': 'command', 'tool.result': 'output', 'clarify.response': 'answer',
  };
  const textOf = (e) => {
    const p = e.payload;
    if (e.type === 'tool.call') return `${p.tool} ${p.args_preview ?? ''}`;
    if (e.type === 'tool.result') return String(p.result_preview ?? '');
    if (e.type === 'clarify.response') return Object.values(p.answers ?? {}).flat().join('; ');
    return String(p.text ?? '');
  };
  const wordsOf = (t) => t.toLowerCase().split(/[^\p{L}\p{N}]+/u).filter(Boolean);
  route('GET', '/api/search', (_, __, q) => {
    const text = (q.get('q') ?? '').trim();
    if (!text) throw fail(400, 'bad_request', 'the query is empty');
    const typed = wordsOf(text);
    const stems = typed.map((w) => (w.length > 4 ? w.slice(0, -1) : w));
    const found = (t) => stems.every((st) => wordsOf(t).some((w) => w.startsWith(st)));
    const since = q.get('since');
    const all = [];
    for (const s of sessions.values()) {
      if (s.system || (q.get('project') && s.project_id !== q.get('project')) || (q.get('session') && s.id !== q.get('session'))) continue;
      const hit = { session_id: s.id, address: `${projects.get(s.project_id)?.slug ?? 'personal'}/${s.slug}`, title: s.title, archived: s.archived };
      if (found(s.title)) all.push({ hit: { ...hit, ts: s.created_at, snippet: s.title }, text: s.title });
      for (const e of s.events) {
        const kind = KIND_OF[e.type];
        const t = kind ? textOf(e) : '';
        if (t && found(t)) {
          all.push({ kind, text: t, hit: { ...hit, seq: e.seq, type: e.type, ts: e.ts, snippet: t.replace(/\s+/g, ' ').slice(0, 160) } });
        }
      }
    }
    const inTime = all.filter((h) => !since || h.hit.ts >= since).sort((a, b) => b.hit.ts.localeCompare(a.hit.ts));
    const countOf = (list) => ({ hits: list.length, sessions: new Set(list.map((h) => h.hit.session_id)).size });
    const counts = { all: countOf(inTime) };
    for (const k of ['message', 'command', 'output', 'answer']) counts[k] = countOf(inTime.filter((h) => h.kind === k));
    let list = inTime.filter((h) => !q.get('kind') || h.kind === q.get('kind'));
    const inSession = new Map();
    for (const h of list) inSession.set(h.hit.session_id, (inSession.get(h.hit.session_id) ?? 0) + 1);
    const cap = Number(q.get('per_session') || 0);
    if (cap) {
      const nth = new Map();
      list = list.filter((h) => {
        nth.set(h.hit.session_id, (nth.get(h.hit.session_id) ?? 0) + 1);
        return nth.get(h.hit.session_id) <= cap;
      });
    }
    const offset = Number(q.get('cursor') || 0);
    const limit = Math.min(Number(q.get('limit') || 20), 50);
    const page = list.slice(offset, offset + limit);
    const also = [...new Set(page.flatMap((h) => wordsOf(h.text).filter((w) => !typed.includes(w) && stems.some((st) => w.startsWith(st)))))];
    return {
      results: page.map((h) => ({ ...h.hit, session_hits: inSession.get(h.hit.session_id) })),
      ...(offset + limit < list.length ? { next_cursor: String(offset + limit) } : {}),
      ...(also.length ? { also: also.slice(0, 8) } : {}),
      ...(q.get('cursor') || q.get('session') ? {} : { counts }),
    };
  });
  // The order of the list (T50), by the hub's rules: only the project's unarchived sessions,
  // the missing ones first, newest first.
  const orderOf = (pid) => {
    const live = [...sessions.values()]
      .filter((s) => s.project_id === pid && !s.archived && !s.system)
      .sort((a, b) => b.created_at.localeCompare(a.created_at))
      .map((s) => s.id);
    const o = orders.get(pid) ?? { mode: 'state', session_ids: [] };
    const kept = [...new Set(o.session_ids)].filter((id) => live.includes(id));
    return { mode: o.mode, session_ids: [...live.filter((id) => !kept.includes(id)), ...kept] };
  };
  route('GET', '/api/projects/:pid/session_order', (p) => orderOf(projectOr404(p.pid).id));
  route('PUT', '/api/projects/:pid/session_order', (p, body) => {
    const pid = projectOr404(p.pid).id;
    if (body?.mode !== undefined && !['state', 'recent', 'custom'].includes(body.mode)) throw fail(422, 'bad_request', 'unknown mode');
    const cur = orders.get(pid) ?? { mode: 'state', session_ids: [] };
    orders.set(pid, { mode: body?.mode ?? cur.mode, session_ids: body?.session_ids ?? cur.session_ids });
    if (body?.session_ids) orders.set(pid, orderOf(pid));
    return orderOf(pid);
  });
  route('POST', '/api/projects/:pid/sessions', async (p, body) => {
    const pr = projectOr404(p.pid);
    const wsId = body?.workspace_id ?? pr.default_workspace_id;
    let ws = workspaces.get(wsId);
    if (!wsId) throw fail(409, 'no_workspace', 'the project has no workspace; add one first');
    if (!ws || ws.project_id !== pr.id) throw fail(400, 'bad_request', 'workspace_id is not a workspace of this project');
    if (ws.state && ws.state !== 'ready') throw fail(409, 'workspace_not_ready', `the worker is still making ${ws.path}`);
    const n = [...sessions.values()].filter((s) => s.project_id === pr.id).length + 1;
    const mode = body?.permission_mode;
    if (mode !== undefined && !MODES.includes(mode)) throw fail(400, 'bad_request', 'permission_mode is ask, auto or allow_all');
    const agent = agentSettings({}, body);
    // SPEC §8.2: an ACP agent the workspace's worker named at its start.
    if (body?.runtime === 'acp' || body?.acp_agent !== undefined) {
      const w = allWorkers().find((x) => x.id === ws.worker_id);
      if (body.runtime !== 'acp' || !body.acp_agent) throw fail(400, 'bad_request', 'runtime acp goes with acp_agent');
      if (!w?.capabilities?.acp_agents?.some((a) => a.name === body.acp_agent)) {
        throw fail(422, 'acp_agent_unavailable', `the worker ${w?.name ?? '?'} has no ACP agent ${body.acp_agent}`);
      }
      agent.acp_agent = body.acp_agent;
    }
    if (body?.isolation === 'worktree') {
      // SPEC 0.265: a new ephemeral worktree of the workspace, made for a moment; a new branch
      // whose name has `exists` in it exists already.
      if (ws.kind === 'worktree') throw fail(400, 'invalid_origin', 'the origin is a worktree itself: make the worktree from its origin');
      const branch = String(body.branch || `hqc/session-${n}`);
      await new Promise((r) => setTimeout(r, 300 * speed));
      if (branch.includes('exists') && !body.existing_branch) {
        throw fail(409, 'branch_exists', `the worktree could not be made: the branch ${branch} exists already: pick it as an existing branch`);
      }
      const path = `${ws.path}-${branch.replace(/\//g, '-')}`;
      const w = {
        ...ws, id: randomUUID(), path, label: '', kind: 'worktree', origin_workspace_id: ws.id, branch,
        ephemeral: true, created_by_hqc: true, mirror_enabled: false, hqc_rules: 0, state: 'ready',
        created_at: now(), updated_at: now(),
      };
      workspaces.set(w.id, w);
      const cut = path.lastIndexOf('/');
      (TREE[path.slice(0, cut)] ??= []).push(path.slice(cut + 1));
      ws = w;
    }
    const s = newSession(String(body?.title || `Session ${n}`), randomUUID(), {
      project_id: pr.id, workspace_id: ws.id, cwd: String(body?.cwd || '.'),
    });
    if (mode) s.permission_mode = mode;
    Object.assign(s, agent);
    return withStatus(201, info(s));
  });
  route('GET', '/api/workspaces', (_, __, q) => ({
    workspaces: [...workspaces.values()]
      .filter((w) => !q.get('project_id') || w.project_id === q.get('project_id'))
      // T47: an ephemeral worktree whose sessions are all archived is shown for deletion.
      .map((w) => ({
        ...w,
        marked_for_deletion:
          w.ephemeral && w.state === 'ready' && ![...sessions.values()].some((s) => s.workspace_id === w.id && !s.archived),
      })),
  }));
  route('POST', '/api/workspaces', (_, body) => {
    const pr = projectOr404(String(body?.project_id));
    if (!workerOnline(body?.worker_id)) {
      if (body?.worker_id === workerId || enrolled.some((w) => w.id === body?.worker_id)) {
        throw fail(409, 'worker_offline', 'the worker is offline');
      }
      throw fail(400, 'bad_request', 'unknown worker');
    }
    const clone = body?.kind === 'clone';
    const worktree = body?.kind === 'worktree';
    // T46: the origin is a ready workspace of the same project and worker, not a worktree.
    const origin = worktree ? workspaces.get(body?.origin_workspace_id) : undefined;
    if (worktree) {
      if (!origin || origin.project_id !== pr.id || origin.worker_id !== body.worker_id || origin.kind === 'worktree' || origin.state !== 'ready') {
        throw fail(400, 'invalid_origin', 'the origin is not a ready workspace of this project and worker');
      }
      if (!/^[\w.][\w./-]*$/.test(String(body?.branch ?? '')) || String(body.branch).includes('..')) {
        throw fail(400, 'invalid_branch', `${JSON.stringify(body?.branch)} is not a branch name`);
      }
    }
    const path = String(body?.path || (worktree ? `${origin.path}-${body.branch.replace(/\//g, '-')}` : ''))
      .trim()
      .replace(/\/+$/, '');
    if (!inRoot(path)) throw fail(400, 'outside_allowed_roots', `${path} is outside the worker's allowed_roots`);
    const cut = path.lastIndexOf('/');
    const known = path in TREE || (TREE[path.slice(0, cut)] ?? []).includes(path.slice(cut + 1));
    if (worktree && known) throw fail(400, 'path_exists', `${path} already exists on the worker`);
    if (clone) {
      // T45: the parent must be a folder of the tree, the new one must not exist.
      const url = String(body?.url ?? '');
      if (!/^(https:\/\/|ssh:\/\/|[\w.-]+@[\w.-]+:)/.test(url)) throw fail(400, 'invalid_url', 'use https://, ssh:// or user@host:path');
      if (!(path.slice(0, cut) in TREE) && path.slice(0, cut) !== ROOT) throw fail(400, 'path_not_found', 'the parent folder does not exist on the worker');
      if (known) throw fail(400, 'path_exists', `${path} already exists on the worker`);
    } else if (!known && !worktree && body?.create) {
      // SPEC 0.64: the worker makes the missing folder.
      (TREE[path.slice(0, cut)] ??= []).push(path.slice(cut + 1));
    } else if (!known && !worktree) throw fail(400, 'path_not_found', `${path} does not exist on the worker`);
    const x = [...workspaces.values()].find((w) => w.worker_id === body.worker_id && overlap(w.path, path));
    if (x) {
      const name = projects.get(x.project_id)?.name ?? '';
      throw fail(409, 'workspace_conflict', `${path} overlaps workspace ${x.path} of project ${name}`, {
        existing_workspace_id: x.id, existing_project_id: x.project_id, existing_path: x.path,
      });
    }
    const w = {
      ...ex['api.workspace'], id: randomUUID(), project_id: pr.id, worker_id: body.worker_id, path,
      // Mirrored by default (T21); only the explicit import brings sessions in the mock.
      label: String(body?.label ?? ''), mirror_enabled: body?.mirror_enabled !== false,
      created_at: now(), updated_at: now(),
    };
    if (clone || worktree) {
      // The worker works for a moment. A clone of a URL with `missing` in it is not found; a
      // worktree of the branch `taken` is checked out elsewhere; a URL or branch with `hold`
      // waits for `finishCreating()` (tests).
      Object.assign(w, { kind: body.kind, created_by_hqc: true, state: 'creating', hqc_rules: 0 });
      if (clone && body.branch) w.branch = body.branch;
      if (worktree) Object.assign(w, { origin_workspace_id: origin.id, branch: body.branch });
      const error = clone
        ? String(body.url).includes('missing') && 'git clone failed: fatal: repository not found'
        : body.branch === 'taken' && !body.new_branch && `the branch taken is already checked out at ${origin.path}`;
      const finish = () => {
        if (workspaces.get(w.id) !== w) return;
        if (error) {
          Object.assign(w, { state: 'failed', error });
        } else {
          w.state = 'ready';
          (TREE[path.slice(0, cut)] ??= []).push(path.slice(cut + 1));
        }
        w.updated_at = now();
      };
      if (String(clone ? body.url : body.branch).includes('hold')) held.push(finish);
      else setTimeout(finish, 400 * speed);
    }
    workspaces.set(w.id, w);
    pr.default_workspace_id ??= w.id;
    if (body?.mirror_enabled === true) importClaude(w);
    return withStatus(201, w);
  });
  // T45: what deleting a folder hqc made would lose (`workspace.inspect`); a worktree (T46)
  // has one commit on its own branch and nothing uncommitted, unless its branch says `dirty`.
  route('GET', '/api/workspaces/:id/inspect', (p) => {
    const w = workspaces.get(p.id);
    if (!w) throw fail(404, 'not_found', 'unknown workspace');
    if (!w.created_by_hqc || w.state !== 'ready') throw fail(400, 'bad_request', 'only a ready folder hqc created is inspected');
    if (!workerOnline(w.worker_id)) throw fail(409, 'worker_offline', 'the worker is offline');
    if (w.kind !== 'worktree') return ex['api.workspace.inspect'];
    const dirty = w.branch.includes('dirty') ? ['notes.txt'] : [];
    return { git: true, dirty, dirty_count: dirty.length, unpushed: [{ branch: w.branch, commits: 1 }] };
  });
  // The import from Claude Code (SPEC §9.1): what the worker's mirror would send for the
  // folder: one session with its history, driven by VS Code, quiet for a minute.
  function importClaude(w) {
    const s = newSession(`From VS Code: ${w.path.split('/').at(-1)}`, randomUUID(), {
      project_id: w.project_id, workspace_id: w.id,
    });
    const u = store(s, mk(s, 'event.user.message', { text: 'Run echo hqc-vscode', artifact_ids: [] }, {
      delivery: 'delivered', delivered_at: now(),
    }));
    store(s, mk(s, 'event.agent.message', { text: 'VSCODE-DONE', final: true, in_reply_to: u.event_id }));
    s.locked = 'vscode';
    s.vscode_active_at = Date.now() - 60_000;
    pushSummary(s);
    // T20 (SPEC §9.3): one more, open in VS Code right now, whose transcript forked: VS Code
    // continued from its own last record after hqc's turn.
    const o = newSession(`Open in VS Code: ${w.path.split('/').at(-1)}`, randomUUID(), {
      project_id: w.project_id, workspace_id: w.id,
    });
    const p = store(o, mk(o, 'event.user.message', { text: 'Reply with exactly: ONE', artifact_ids: [] }, {
      delivery: 'delivered', delivered_at: now(),
    }));
    store(o, mk(o, 'event.agent.message', { text: 'ONE', final: true, in_reply_to: p.event_id }));
    store(o, mk(o, 'event.system', {
      code: 'session_forked',
      text: 'VS Code continued from an earlier point: 1 turn from hqc is not in its context',
      at_uuid: randomUUID(), branch_from: 'vscode', orphaned_turns: 1,
    }));
    store(o, mk(o, 'event.user.message', { text: 'Reply with exactly: VSCODE-TURN', artifact_ids: [] }, {
      delivery: 'delivered', delivered_at: now(),
    }));
    o.locked = 'vscode';
    o.held_by = { entrypoint: 'claude-vscode', status: 'idle' };
    o.vscode_active_at = Date.now() - 60_000;
    pushSummary(o);
  }
  /** Test knob: the holder exists (takeover still refuses) but summaries do not mention it,
   *  as for a client that has not heard of it yet. */
  function hideHolder(id, hidden) {
    const s = sessions.get(id);
    if (!s) return;
    s.holder_hidden = hidden;
    pushSummary(s);
  }
  /** The user closed session `id` in VS Code: nobody holds it, it is free at once. */
  function closeVsCode(id) {
    const s = sessions.get(id);
    if (!s?.held_by) return;
    s.held_by = undefined;
    s.locked = undefined;
    pushSummary(s);
  }
  // Folders with Claude Code sessions on home-mac (`workspace.list_candidates`).
  route('GET', '/api/workers/:id/candidates', (p) => {
    if (!workerOnline(p.id)) throw fail(409, 'worker_offline', 'the worker is offline');
    return {
      candidates: [
        { path: `${ROOT}/cad`, sessions_count: 3, last_active: '2026-10-22T18:04:11Z' },
        { path: `${ROOT}/kicad/power-board`, sessions_count: 1, last_active: '2026-10-23T00:10:17Z' },
      ],
    };
  });
  // SPEC §9.3: not while VS Code holds it open; else after 30 s without writes from VS Code.
  route('POST', '/api/sessions/:id/takeover', (p) => {
    const s = sessionOr404(p.id);
    if (s.held_by) {
      throw fail(409, 'vscode_open', 'the session is open in VS Code: close it there and take it over again', {
        held_by: s.held_by,
      });
    }
    if (s.locked === 'vscode' && Date.now() - (s.vscode_active_at ?? 0) < 30_000) {
      throw fail(409, 'vscode_active', 'VS Code wrote to the session recently');
    }
    s.locked = undefined;
    store(s, mk(s, 'event.system', {
      code: 'session_taken_over',
      text: 'hqc took over the session. If it is open in VS Code, its context there is stale: reopen the session in VS Code after you switch back.',
    }));
    return info(s);
  });
  // SPEC §6: the session continues in another folder of its worker, in another project: a
  // workspace of the user at exactly that path with its project, or a new workspace of
  // `project` or of a new project named after the folder; checks as for a new workspace.
  route('POST', '/api/sessions/:id/move', (p, body) => {
    const s = sessionOr404(p.id);
    if (s.locked === 'vscode' || s.held_by) {
      throw fail(409, 'unavailable', 'the session is driven or open outside hqc (VS Code): close it there first');
    }
    const worker = workspaces.get(s.workspace_id)?.worker_id ?? workerId;
    if (!workerOnline(worker)) throw fail(409, 'worker_offline', 'the worker is offline');
    const path = String(body?.path ?? '').trim().replace(/\/+$/, '');
    if (!inRoot(path)) throw fail(400, 'outside_allowed_roots', `${path} is outside the worker's allowed_roots`);
    const cut = path.lastIndexOf('/');
    const known = path in TREE || (TREE[path.slice(0, cut)] ?? []).includes(path.slice(cut + 1));
    if (!known && body?.create) (TREE[path.slice(0, cut)] ??= []).push(path.slice(cut + 1));
    else if (!known) throw fail(400, 'path_not_found', `${path} does not exist on the worker`);
    const wanted = body?.project && [...projects.values()].find((x) => x.id === body.project || x.slug === body.project);
    if (body?.project && !wanted) throw fail(404, 'not_found', 'unknown project');
    let ws = [...workspaces.values()].find((w) => w.worker_id === worker && overlap(w.path, path));
    if (ws && ws.path !== path) {
      const name = projects.get(ws.project_id)?.name ?? '';
      throw fail(409, 'workspace_conflict', `${path} overlaps workspace ${ws.path} of project ${name}`, {
        existing_workspace_id: ws.id, existing_project_id: ws.project_id, existing_path: ws.path,
      });
    }
    if (ws && wanted && ws.project_id !== wanted.id) {
      throw fail(400, 'bad_request', `${path} is a workspace of another project, not of ${wanted.slug}`);
    }
    if (!ws) {
      const pr = wanted || newProject(path.slice(cut + 1));
      ws = {
        ...ex['api.workspace'], id: randomUUID(), project_id: pr.id, worker_id: worker, path,
        label: '', mirror_enabled: true, created_at: now(), updated_at: now(),
      };
      workspaces.set(ws.id, ws);
      pr.default_workspace_id ??= ws.id;
    }
    if (s.workspace_id === ws.id) return info(s);
    const from = projects.get(s.project_id)?.name ?? '?';
    const to = projects.get(ws.project_id);
    const order = orders.get(s.project_id);
    if (order) order.session_ids = order.session_ids.filter((id) => id !== s.id);
    const base = s.slug;
    let slug = base;
    for (let i = 2; [...sessions.values()].some((x) => x !== s && x.project_id === to.id && x.slug === slug); i++) slug = `${base}-${i}`;
    Object.assign(s, { project_id: to.id, workspace_id: ws.id, cwd: '.', slug });
    store(s, mk(s, 'event.system', { code: 'session_moved', text: `The session moved from ${from} to ${to.name} (${ws.path})` }));
    return info(s);
  });
  route('PATCH', '/api/workspaces/:id', (p, body) => {
    const w = workspaces.get(p.id);
    if (!w) throw fail(404, 'not_found', 'unknown workspace');
    if (typeof body?.label === 'string') w.label = body.label.trim();
    if (typeof body?.mirror_enabled === 'boolean') w.mirror_enabled = body.mirror_enabled;
    return w;
  });
  route('DELETE', '/api/workspaces/:id', (p, _, q) => {
    const w = workspaces.get(p.id);
    if (!w) throw fail(404, 'not_found', 'unknown workspace');
    if (w.state === 'creating') throw fail(409, 'workspace_creating', 'the worker is still making this workspace');
    const worktrees = [...workspaces.values()].filter((x) => x.origin_workspace_id === w.id).length;
    if (worktrees) throw fail(409, 'workspace_in_use', `${worktrees} worktree(s) of this workspace exist; delete them first`);
    const live = [...sessions.values()].filter((s) => s.workspace_id === w.id && !s.archived);
    if (live.length && q.get('archive_sessions') !== 'true')
      throw fail(409, 'workspace_in_use', `${live.length} session(s) still work in this workspace; archive them first`);
    live.forEach(archiveSession);
    if (q.get('remove_folder') === 'true') {
      if (!w.created_by_hqc || w.state !== 'ready') throw fail(400, 'bad_request', 'remove_folder: only a ready folder hqc created is deleted');
      if (!workerOnline(w.worker_id)) throw fail(409, 'worker_offline', 'the worker is offline');
      const cut = w.path.lastIndexOf('/');
      const names = TREE[w.path.slice(0, cut)] ?? [];
      names.splice(names.indexOf(w.path.slice(cut + 1)) >>> 0, 1);
      w.folder_removed = true;
      // T46: `git worktree remove --force` only after the dialog showed the dirty files.
      if (q.get('force') === 'true') w.forced = true;
    }
    removedWorkspaces.push(w);
    workspaces.delete(w.id);
    const pr = projects.get(w.project_id);
    if (pr?.default_workspace_id === w.id) {
      pr.default_workspace_id = [...workspaces.values()].find((x) => x.project_id === pr.id)?.id;
      if (!pr.default_workspace_id) delete pr.default_workspace_id;
    }
    return withStatus(204, null);
  });
  route('GET', '/api/workers/:id/dirs', (p, _, q) => {
    if (!workerOnline(p.id)) throw fail(409, 'worker_offline', 'the worker is offline');
    const path = q.get('path');
    if (!path) return { entries: [{ name: 'work', path: ROOT }] };
    if (!inRoot(path)) throw fail(400, 'invalid_path', `${path} is outside allowed_roots`);
    return { entries: (TREE[path] ?? []).map((name) => ({ name, path: `${path}/${name}` })) };
  });
  route('GET', '/api/sessions/:id', (p) => info(sessionOr404(p.id)));
  route('PATCH', '/api/sessions/:id', (p, body) => {
    const s = sessionOr404(p.id);
    if (typeof body?.title === 'string' && body.title.trim()) {
      s.title = body.title.trim();
      pushSummary(s);
    }
    const mode = body?.permission_mode;
    if (mode !== undefined) {
      if (!MODES.includes(mode)) throw fail(400, 'bad_request', 'permission_mode is ask, auto or allow_all');
      s.permission_mode = mode;
      pushSummary(s);
    }
    agentSettings(s, body);
    return info(s);
  });
  route('GET', '/api/sessions/:id/events', (p, _, q) => {
    const s = sessionOr404(p.id);
    const limit = Math.min(1000, Number(q.get('limit') ?? 200));
    if (q.has('before_seq')) {
      const before = Number(q.get('before_seq'));
      return { events: s.events.filter((e) => e.seq < before).slice(-limit), last_seq: s.last_seq };
    }
    const after = Number(q.get('after_seq') ?? 0);
    return { events: s.events.filter((e) => e.seq > after).slice(0, limit), last_seq: s.last_seq };
  });
  route('POST', '/api/sessions/:id/input', (p, body) => {
    const s = sessionOr404(p.id);
    if (s.system) throw fail(409, 'system_session', "this is the worker's service session");
    if (s.locked === 'vscode') {
      throw fail(409, 'locked_vscode', 'the session is being driven from VS Code; take it over first', {
        hint: 'takeover',
        ...(s.held_by && !s.holder_hidden ? { held_by: s.held_by } : {}),
      });
    }
    if (typeof body?.text !== 'string' || !body.text.trim()) throw fail(400, 'bad_request', 'text required');
    // A resend with the same client_op_id is the same input: the stored event, 200 (SPEC §6).
    const dup = body.client_op_id && s.events.find((e) => e.client_op_id === String(body.client_op_id));
    if (dup) return { event: dup };
    const ev = store(s, mk(s, 'event.user.message', {
      text: body.text, artifact_ids: body.artifact_ids ?? [],
    }, {
      author: { kind: 'user', id: userId },
      ...(body.client_op_id ? { client_op_id: String(body.client_op_id) } : {}),
      delivery: 'pending',
    }));
    enqueue(s, ev);
    return withStatus(201, { event: ev });
  });
  // Like the hub: open requests are cancelled with `stopped`, the turn ends without a
  // system event (the runtime reports the interrupted turn as `status idle`).
  // Archiving stops the agent and closes open requests; the session stays as history
  // (SPEC §17.4).
  const archiveSession = (s) => {
    if (s.turn) {
      s.turn.aborted = true;
      cancelOpen(s, 'archived');
      s.turn.onAbort?.();
    }
    s.archived = true;
    s.archived_at = now();
    pushSummary(s);
  };
  route('POST', '/api/sessions/:id/archive', (p) => {
    const s = sessionOr404(p.id);
    if (s.system) throw fail(409, 'system_session', "the worker's service session cannot be archived");
    archiveSession(s);
    return info(s);
  });
  route('POST', '/api/sessions/:id/unarchive', (p) => {
    const s = sessionOr404(p.id);
    s.archived = false;
    delete s.archived_at;
    pushSummary(s);
    return info(s);
  });
  route('DELETE', '/api/sessions/:id', (p) => {
    const s = sessionOr404(p.id);
    if (s.system) throw fail(409, 'system_session', "the worker's service session cannot be deleted");
    sessions.delete(p.id);
    return { deleted: true };
  });
  route('POST', '/api/sessions/:id/stop', (p) => {
    const s = sessionOr404(p.id);
    if (s.turn) {
      s.turn.aborted = true;
      cancelOpen(s, 'stopped');
      s.turn.onAbort?.();
    }
    return { sent: true };
  });
  route('POST', '/api/sessions/:id/retry', (p) => {
    const s = sessionOr404(p.id);
    let requeued = 0;
    for (const ev of s.events) {
      if (ev.type === 'user.message' && ev.delivery === 'failed') {
        s.queue.push(ev);
        setDelivery(s, ev, 'pending');
        requeued++;
      }
    }
    if (!s.turn) void runQueue(s);
    return { requeued };
  });
  // First answer wins; a second one gets 409 request_closed and no event (SPEC §10.1).
  route('POST', '/api/sessions/:id/requests/:rid', (p, body) => {
    const s = sessionOr404(p.id);
    const o = s.open;
    if (!o || o.request_id !== p.rid) {
      const known = s.events.some((e) => e.payload?.request_id === p.rid && e.type.endsWith('.request'));
      throw known ? fail(409, 'request_closed', 'the request is already closed') : fail(404, 'not_found', 'not found');
    }
    let ev;
    if (o.kind === 'approval') {
      const opt = o.event.payload.options.find((x) => x.id === body?.option_id);
      if (!opt) throw fail(400, 'bad_request', `unknown option \`${body?.option_id}\``);
      // A text answer instead of the call (SPEC §10.1): only with a deny option.
      const message = body?.message;
      if (message !== undefined) {
        if (s.system) throw fail(400, 'bad_request', 'a worker request takes no message');
        if (!['deny', 'deny_always'].includes(opt.canonical)) throw fail(400, 'bad_request', 'message goes only with a deny option');
        if (typeof message !== 'string' || !message.trim()) throw fail(400, 'bad_request', 'message must not be blank');
      }
      s.open = null;
      ev = store(s, mk(s, 'event.approval.response', {
        request_id: o.request_id, option_id: body.option_id, by: userId, message,
      }));
      o.resolve({ option_id: body.option_id, message });
    } else {
      const answers = body?.answers;
      if (typeof answers !== 'object' || answers === null || !Object.keys(answers).length) {
        throw fail(400, 'bad_request', 'answers must not be empty');
      }
      s.open = null;
      ev = store(s, mk(s, 'event.clarify.response', { request_id: o.request_id, answers }));
      o.resolve({ answers });
    }
    return { event_id: ev.event_id, seq: ev.seq };
  });
  route('PUT', '/api/sessions/:id/read_cursor', (p, body) => {
    const s = sessionOr404(p.id);
    s.read_seq = Math.min(s.last_seq, Math.max(s.read_seq, Number(body?.last_read_seq) || 0));
    pushSummary(s);
    return { last_read_seq: s.read_seq };
  });
  route('GET', '/api/artifacts/:id/url', (p) => ({ url: `/api/mock/artifacts/${p.id}` }));
  route('GET', '/api/mock/artifacts/:id', (p) => ({ artifact_id: p.id, note: 'mock artifact' }));
  // Workers as the hub lists them (SPEC §6): `revoked` only with `include_revoked=1`.
  const allWorkers = () =>
    [{ id: workerId, name: 'home-mac', os: 'macos', status: 'online', session_secrets: true, browser: true, capabilities: { claude_code: { version: '2.1.322', login: true }, acp_agents: [{ name: 'agy', version: '1.5.18', environments: ['default'] }] } }, ...enrolled].map((w) =>
      revoked.has(w.id) ? { ...w, status: 'revoked', revoked_at: new Date().toISOString() } : w,
    );
  route('GET', '/api/workers', (_, __, q) => ({
    workers: allWorkers().filter((w) => q.get('include_revoked') === '1' || w.status !== 'revoked'),
  }));
  route('DELETE', '/api/workers/:id', (p, _, q) => {
    if (q.get('purge') !== '1') {
      revoked.add(p.id);
      // Revoking archives the worker's service session (SPEC §10.2).
      const sys = [...sessions.values()].find((s) => s.system && s.slug === `worker-${p.id}`);
      if (sys) {
        sys.archived = true;
        pushSummary(sys);
      }
      return withStatus(204, null);
    }
    if (!revoked.has(p.id)) throw fail(409, 'worker_not_revoked', 'revoke the worker before deleting it');
    // home-mac carries the mock's sessions; enrolled workers have none.
    if (p.id === workerId) {
      throw fail(409, 'worker_has_sessions', 'sessions still work on this worker', { sessions_count: 3 });
    }
    const i = enrolled.findIndex((w) => w.id === p.id);
    if (i < 0) throw fail(404, 'not_found', 'unknown worker');
    enrolled.splice(i, 1);
    revoked.delete(p.id);
    return withStatus(204, null);
  });
  // SPEC 0.185: the sign-in of a worker's own Claude Code. The mock's home-mac starts signed
  // out; the code "ok" signs it in, any other is refused as the CLI would.
  const claudeAuth = new Map();
  const claudeLogins = new Map();
  const claudeWorker = (id) => {
    const w = allWorkers().find((x) => x.id === id);
    if (!w) throw fail(404, 'not_found', 'unknown worker');
    if (w.status !== 'online') throw fail(409, 'worker_offline', 'the worker is offline');
    if (!w.capabilities?.claude_code?.login) throw fail(422, 'unsupported', 'the worker cannot sign Claude Code in');
    return w;
  };
  const claudeStatus = (id) => claudeAuth.get(id) ?? { logged_in: false };
  route('GET', '/api/workers/:id/claude', (p) => {
    claudeWorker(p.id);
    return claudeStatus(p.id);
  });
  route('POST', '/api/workers/:id/claude/login', (p, body) => {
    claudeWorker(p.id);
    const extra = Object.keys(body ?? {});
    if (extra.length) throw fail(400, 'bad_request', `unknown field \`${extra[0]}\``);
    if ([...claudeLogins.values()].some((l) => l.worker === p.id)) throw fail(409, 'login_running', 'a sign-in is already running');
    const login_id = randomUUID();
    claudeLogins.set(login_id, { worker: p.id });
    return { login_id, url: `https://claude.com/cai/oauth/authorize?code=true&state=${login_id}` };
  });
  route('POST', '/api/workers/:id/claude/login/:login', (p, body) => {
    claudeWorker(p.id);
    const extra = Object.keys(body ?? {}).filter((k) => k !== 'code');
    if (extra.length) throw fail(400, 'bad_request', `unknown field \`${extra[0]}\``);
    if (typeof body?.code !== 'string' || !body.code) throw fail(400, 'bad_request', 'code is needed');
    const l = claudeLogins.get(p.login);
    if (!l || l.worker !== p.id) throw fail(404, 'not_found', 'unknown or expired sign-in');
    if (body.code !== 'ok') return { ok: false, message: 'Invalid code: paste the code the page shows.', status: claudeStatus(p.id) };
    claudeLogins.delete(p.login);
    const status = {
      logged_in: true,
      auth_method: 'claude.ai',
      email: 'owner@example.com',
      org_name: "Owner's Organization",
      subscription_type: 'max',
      expires_at: new Date(Date.now() + 30 * 864e5).toISOString(),
    };
    claudeAuth.set(p.id, status);
    return { ok: true, status };
  });
  route('DELETE', '/api/workers/:id/claude/login/:login', (p) => {
    const l = claudeLogins.get(p.login);
    if (l && l.worker === p.id) claudeLogins.delete(p.login);
    return withStatus(204, null);
  });
  route('GET', '/api/workers/enroll/:code', (p) => {
    const code = String(p.code).trim().toUpperCase();
    if (!/^[A-Z0-9]{4}-?[A-Z0-9]{4}$/.test(code)) throw fail(404, 'not_found', 'not found');
    const replaceable = allWorkers()
      .filter((w) => w.status !== 'online')
      .map((w) => ({ id: w.id, name: w.name, os: w.os ?? '', status: w.status, sessions_count: w.id === workerId ? 3 : 0 }));
    return {
      name: `worker-${code.slice(0, 4).toLowerCase()}`,
      os: 'macos',
      created_at: new Date().toISOString(),
      country: 'DE',
      ...(replaceable.length ? { replaceable } : {}),
    };
  });
  route('POST', '/api/workers/enroll/confirm', (_, body) => {
    const code = String(body?.code ?? '').trim().toUpperCase();
    if (!/^[A-Z0-9]{4}-?[A-Z0-9]{4}$/.test(code)) throw fail(404, 'not_found', 'not found');
    const name = `worker-${code.slice(0, 4).toLowerCase()}`;
    if (body?.replace_worker_id) {
      // The same machine (SPEC §5.1): same id, new name, revocation lifted.
      const w = allWorkers().find((x) => x.id === body.replace_worker_id);
      if (!w) throw fail(400, 'bad_request', 'unknown worker');
      if (w.status === 'online') throw fail(409, 'worker_online', 'the worker is online');
      revoked.delete(w.id);
      const e = enrolled.find((x) => x.id === w.id);
      if (e) e.name = name;
      return { worker_id: w.id, name, replaced: w.name };
    }
    const w = { id: randomUUID(), name, os: 'macos', status: 'offline' };
    enrolled.push(w);
    return { worker_id: w.id, name: w.name };
  });
  route('GET', '/api/push/vapid', () => ({ public_key: vapid }));
  route('POST', '/api/push/subscribe', (_, body) => {
    if (typeof body?.endpoint !== 'string' || typeof body?.keys?.p256dh !== 'string') {
      throw fail(400, 'bad_request', 'PushSubscription.toJSON() expected');
    }
    pushSubscriptions.push(body);
    log(`push subscription stored: ${body.endpoint.slice(0, 60)}...`);
    return withStatus(201, { id: randomUUID() });
  });

  const server = createServer(async (req, res) => {
    const url = new URL(req.url ?? '/', 'http://mock');
    const r = routes.find((x) => x.method === req.method && x.re.test(url.pathname));
    const reply = (status, data) => {
      res.writeHead(status, { 'content-type': 'application/json' });
      res.end(status === 204 ? undefined : JSON.stringify(data));
    };
    if (!r) return reply(404, { error: 'not_found', message: `${req.method} ${url.pathname}` });
    let body;
    try {
      const chunks = [];
      for await (const c of req) chunks.push(c);
      const raw = Buffer.concat(chunks);
      const multipart = String(req.headers['content-type'] ?? '').startsWith('multipart/');
      body = !chunks.length ? undefined : multipart ? raw : JSON.parse(raw.toString('utf8'));
      const m = url.pathname.match(r.re);
      const params = Object.fromEntries(r.keys.map((k, i) => [k, decodeURIComponent(m[i + 1])]));
      log(`${req.method} ${url.pathname}`);
      const out = (await r.fn(params, body, url.searchParams)) ?? {};
      if (RAW in out) {
        res.writeHead(200, { 'content-type': out[RAW].type, 'x-content-type-options': 'nosniff' });
        res.end(out[RAW].data);
      } else if (STATUS in out) reply(out[STATUS], out.body);
      else reply(200, out);
    } catch (e) {
      reply(e.status ?? 500, { ...(e.extra ?? {}), error: e.code ?? 'internal', message: e.message });
    }
  });

  // ---- WebSocket /api/ws -------------------------------------------------------------

  const wss = new WebSocketServer({ noServer: true });
  server.on('upgrade', (req, socket, head) => {
    if (new URL(req.url ?? '/', 'http://mock').pathname !== '/api/ws') return socket.destroy();
    wss.handleUpgrade(req, socket, head, (ws) => {
      const c = { ws, subs: new Set(), summary: false };
      clients.add(c);
      ws.on('close', () => clients.delete(c));
      ws.on('message', (data) => {
        let f;
        try {
          f = JSON.parse(String(data));
        } catch {
          return;
        }
        // Flat frames (hqc_proto::client_frame), like the hub.
        const send = (frame) => ws.send(JSON.stringify(frame));
        const err = (code, message, session_id) =>
          send(session_id ? { type: 'error', code, message, session_id } : { type: 'error', code, message });
        switch (f.type) {
          case 'subscribe': {
            const s = sessions.get(f.session_id);
            if (!s) return err('not_found', 'unknown session', f.session_id);
            c.subs.add(s.id);
            const after = Number(f.after_seq) || 0;
            for (const ev of s.events) if (ev.seq > after) send({ type: 'event', event: ev });
            send({ type: 'subscribed', session_id: s.id, last_seq: s.last_seq });
            break;
          }
          case 'unsubscribe':
            c.subs.delete(f.session_id);
            break;
          case 'subscribe_summary':
            c.summary = true;
            send({ type: 'summary', sessions: [...sessions.values()].map(summaryOf) });
            break;
          case 'ping':
            send({ type: 'pong' });
            break;
          default:
            err('invalid_frame', `unknown variant \`${f.type}\``);
        }
      });
    });
  });

  // Loopback only: the mock has no authentication.
  await new Promise((r) => server.listen(port, '127.0.0.1', r));
  const actual = server.address().port;
  return {
    port: actual,
    url: `http://127.0.0.1:${actual}`,
    sessions,
    pushSubscriptions,
    closeVsCode,
    hideHolder,
    removedWorkspaces,
    /** Ends the clones and worktrees that wait (a URL or branch with `hold`). */
    finishCreating() {
      for (const f of held.splice(0)) f();
    },
    /** What `hqc_spawn` with `isolation: "worktree"` leaves (T47): an ephemeral worktree of
     *  `originId` on `hqc/<slug>` and the spawned session in it; the session's id. */
    spawnWorktree(originId, title) {
      const origin = workspaces.get(originId);
      const s = newSession(title, randomUUID(), { project_id: origin.project_id });
      const path = `${origin.path}-hqc-${s.slug}`;
      const w = {
        ...origin, id: randomUUID(), path, label: '', kind: 'worktree', origin_workspace_id: origin.id,
        branch: `hqc/${s.slug}`, ephemeral: true, created_by_hqc: true, mirror_enabled: false, hqc_rules: 0,
        state: 'ready', created_at: now(), updated_at: now(),
      };
      workspaces.set(w.id, w);
      const cut = path.lastIndexOf('/');
      (TREE[path.slice(0, cut)] ??= []).push(path.slice(cut + 1));
      s.workspace_id = w.id;
      pushSummary(s);
      return s.id;
    },
    /** Drops every client socket, to exercise reconnect. */
    dropClients() {
      for (const c of clients) c.ws.terminate();
    },
    async close() {
      for (const s of sessions.values()) if (s.turn) s.turn.aborted = true;
      for (const c of clients) c.ws.terminate();
      wss.close();
      await new Promise((r) => server.close(r));
    },
  };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const port = Number(process.env.MOCK_PORT ?? 8787);
  const speed = Number(process.env.MOCK_SPEED ?? 1);
  const m = await startMock({ port, speed, log: (l) => console.log(`[mock] ${l}`) });
  console.log(`[mock] hub on ${m.url}`);
}
