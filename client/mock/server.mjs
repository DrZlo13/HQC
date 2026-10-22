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
// usage -> status idle. A message containing "вопрос" or "clarify" asks a clarify.request
// instead of the approval; one containing "markdown" or "разметка" gets MARKDOWN_ANSWER
// streamed and nothing else.

import { createServer } from 'node:http';
import { randomUUID, generateKeyPairSync } from 'node:crypto';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { WebSocketServer } from 'ws';

const WIRE = join(dirname(fileURLToPath(import.meta.url)), '../../crates/hqc-proto/examples/wire');
const STATUS = Symbol('status');

/** The answer to a message with "markdown" or "разметка": every element of T11 to look at. */
export const MARKDOWN_ANSWER = `## Итоги прогона

Проверил **три** крейта, *один* с предупреждениями; подробности в [CI](https://example.com/ci/42).
Вторая строка того же абзаца, \`cargo test --workspace\` прошёл.

### Что сделано

1. Обновил парсер
   - добавил терпимость к новым полям
   - [x] тесты на фикстуры
   - [ ] фикстуры для 2.2
2. Поправил ~~старый~~ вывод ошибок

| крейт | тестов | статус |
|:------|-------:|:------:|
| hqc-proto | 42 | ✓ |
| hqc-hub | 117 | ✓ |
| hqc-worker | 63 | предупреждения |

\`\`\`rust
fn main() {
    println!("<hello>");
}
\`\`\`

> Цитата: незакрытые блоки кода в потоке не ломают вёрстку.

---

HTML показывается текстом: <img src=x onerror=alert(1)> и <script>alert(1)</script>,
ссылка [javascript:](javascript:alert(1)) тоже текст, картинка не грузится: ![схема](https://example.com/d.png).
`;

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
  };
  const workerId = ex['hub.hello.ack'].payload.worker_id;
  const now = () => new Date().toISOString();
  // Projects and workspaces (T14): the personal project with one workspace on home-mac.
  /** @type {Map<string, any>} */
  const projects = new Map();
  /** @type {Map<string, any>} */
  const workspaces = new Map();
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
    };
  }

  function pushSummary(s) {
    const data = JSON.stringify({ type: 'session.summary', session: summaryOf(s) });
    for (const c of clients) if (c.summary) c.ws.send(data);
  }

  function stateOf(s) {
    if (s.archived) return 'archived';
    if (s.open) return 'waiting';
    if (s.events.some((e) => e.type === 'user.message' && e.delivery === 'failed')) return 'error';
    if (s.turn) return 'running';
    if (s.queue.length) return 'queued';
    return 'idle';
  }

  /** The session object of SPEC §6 (hqc_proto::api::Session, examples/wire/api.session.json). */
  function info(s) {
    const sum = summaryOf(s);
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
        runtime: 'claude_code',
        runtime_session_id: s.id,
        process_state: s.turn ? 'running' : 'stopped',
        disallowed_tools: [],
        restricted: false,
        start_failures: 0,
      },
      auto_approve: false,
      wake_on_send: false,
      locked_by: s.turn ? 'hqc' : 'none',
      last_seq: s.last_seq,
      archived: s.archived,
      state: sum.state,
      queue_len: sum.queue_len,
      last_read_seq: s.read_seq,
      unread: sum.unread,
      created_at: s.created_at,
      updated_at: s.events.at(-1)?.ts ?? s.created_at,
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
    const s = newSession('hqc: прогон тестов', ex['event.user.message'].session_id);
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
    const s = newSession('Ввод не доставлен');
    store(s, mk(s, 'event.user.message', { text: 'Собери релиз', artifact_ids: [] }, {
      client_op_id: randomUUID(), delivery: 'failed',
    }));
    store(s, mk(s, 'event.system', {
      text: 'The agent did not start, input was not delivered: claude: command not found',
      code: 'agent_start_failed',
    }));
  }
  newSession('Пустая сессия');

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
    const askClarify = /вопрос|clarify/i.test(input.payload.text);
    status(s, 'running');
    await step(t, 300);

    if (/markdown|разметк/i.test(input.payload.text)) {
      await stream(s, t, MARKDOWN_ANSWER);
      store(s, mk(s, 'event.agent.message', { text: MARKDOWN_ANSWER, final: true, in_reply_to: input.event_id }));
      store(s, mk(s, 'event.usage'));
      return;
    }

    const intro = 'Сейчас посмотрю на проект и запущу тесты.';
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
    if (askClarify) {
      const request_id = `req-${randomUUID().slice(0, 8)}`;
      const answer = await request(s, t, 'clarify', mk(s, 'event.clarify.request', { request_id }));
      if (answer.cancel) return;
      final = `Принято: ${Object.entries(answer.answers)
        .map(([k, v]) => `${k} = ${Array.isArray(v) ? v.join(', ') : v}`).join('; ') || 'без ответов'}.`;
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
        final = 'Хорошо, тесты не запускаю.';
      } else {
        await step(t, 800);
        store(s, mk(s, 'event.tool.result', {
          call_id, result_preview: 'test result: ok. 42 passed; 0 failed', error: false, duration_ms: 5120,
          error_text: undefined, artifact_id: undefined,
        }));
        final = 'Все 42 теста прошли.';
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
  route('GET', '/api/projects', () => ({ projects: [...projects.values()] }));
  route('POST', '/api/projects', (_, body) => {
    const name = String(body?.name ?? '').trim();
    if (!name) throw fail(400, 'bad_request', 'name must not be empty');
    const base = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'project';
    let slug = base;
    for (let i = 2; [...projects.values()].some((p) => p.slug === slug); i++) slug = `${base}-${i}`;
    const p = {
      id: randomUUID(), owner_id: userId, name, slug, description: String(body?.description ?? ''),
      is_personal: false, created_at: now(), updated_at: now(),
    };
    projects.set(p.id, p);
    return withStatus(201, p);
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
    for (const k of ['name', 'description', 'default_workspace_id']) if (body?.[k] !== undefined) pr[k] = body[k];
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
  route('POST', '/api/projects/:pid/sessions', (p, body) => {
    const pr = projectOr404(p.pid);
    const wsId = body?.workspace_id ?? pr.default_workspace_id;
    const ws = workspaces.get(wsId);
    if (!wsId) throw fail(409, 'no_workspace', 'the project has no workspace; add one first');
    if (!ws || ws.project_id !== pr.id) throw fail(400, 'bad_request', 'workspace_id is not a workspace of this project');
    const n = [...sessions.values()].filter((s) => s.project_id === pr.id).length + 1;
    const s = newSession(String(body?.title || `Сессия ${n}`), randomUUID(), {
      project_id: pr.id, workspace_id: ws.id, cwd: String(body?.cwd || '.'),
    });
    return withStatus(201, info(s));
  });
  route('GET', '/api/workspaces', (_, __, q) => ({
    workspaces: [...workspaces.values()].filter((w) => !q.get('project_id') || w.project_id === q.get('project_id')),
  }));
  route('POST', '/api/workspaces', (_, body) => {
    const pr = projectOr404(String(body?.project_id));
    if (!workerOnline(body?.worker_id)) {
      if (body?.worker_id === workerId || enrolled.some((w) => w.id === body?.worker_id)) {
        throw fail(409, 'worker_offline', 'the worker is offline');
      }
      throw fail(400, 'bad_request', 'unknown worker');
    }
    const path = String(body?.path ?? '').trim().replace(/\/+$/, '');
    if (!inRoot(path)) throw fail(400, 'outside_allowed_roots', `${path} is outside the worker's allowed_roots`);
    const cut = path.lastIndexOf('/');
    const known = path in TREE || (TREE[path.slice(0, cut)] ?? []).includes(path.slice(cut + 1));
    if (!known) throw fail(400, 'path_not_found', `${path} does not exist on the worker`);
    const x = [...workspaces.values()].find((w) => w.worker_id === body.worker_id && overlap(w.path, path));
    if (x) {
      const name = projects.get(x.project_id)?.name ?? '';
      throw fail(409, 'workspace_conflict', `${path} overlaps workspace ${x.path} of project ${name}`, {
        existing_workspace_id: x.id, existing_project_id: x.project_id, existing_path: x.path,
      });
    }
    const w = {
      ...ex['api.workspace'], id: randomUUID(), project_id: pr.id, worker_id: body.worker_id, path,
      label: String(body?.label ?? ''), created_at: now(), updated_at: now(),
    };
    workspaces.set(w.id, w);
    pr.default_workspace_id ??= w.id;
    return withStatus(201, w);
  });
  route('PATCH', '/api/workspaces/:id', (p, body) => {
    const w = workspaces.get(p.id);
    if (!w) throw fail(404, 'not_found', 'unknown workspace');
    if (typeof body?.label === 'string') w.label = body.label.trim();
    return w;
  });
  route('DELETE', '/api/workspaces/:id', (p) => {
    const w = workspaces.get(p.id);
    if (!w) throw fail(404, 'not_found', 'unknown workspace');
    const live = [...sessions.values()].filter((s) => s.workspace_id === w.id && !s.archived).length;
    if (live) throw fail(409, 'workspace_in_use', `${live} session(s) still work in this workspace; archive them first`);
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
  route('GET', '/api/sessions/:id/events', (p, _, q) => {
    const s = sessionOr404(p.id);
    const after = Number(q.get('after_seq') ?? 0);
    const limit = Math.min(1000, Number(q.get('limit') ?? 200));
    return { events: s.events.filter((e) => e.seq > after).slice(0, limit), last_seq: s.last_seq };
  });
  route('POST', '/api/sessions/:id/input', (p, body) => {
    const s = sessionOr404(p.id);
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
      const ok = o.event.payload.options.some((x) => x.id === body?.option_id);
      if (!ok) throw fail(400, 'bad_request', `unknown option \`${body?.option_id}\``);
      s.open = null;
      ev = store(s, mk(s, 'event.approval.response', { request_id: o.request_id, option_id: body.option_id, by: userId }));
      o.resolve({ option_id: body.option_id });
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
  route('GET', '/api/workers', () => ({
    workers: [
      { id: workerId, name: 'home-mac', os: 'macos', status: 'online' },
      ...enrolled,
    ].filter((w) => !revoked.has(w.id)),
  }));
  route('DELETE', '/api/workers/:id', (p) => {
    revoked.add(p.id);
    return withStatus(204, null);
  });
  route('GET', '/api/workers/enroll/:code', (p) => {
    const code = String(p.code).trim().toUpperCase();
    if (!/^[A-Z0-9]{4}-?[A-Z0-9]{4}$/.test(code)) throw fail(404, 'not_found', 'not found');
    return {
      name: `worker-${code.slice(0, 4).toLowerCase()}`,
      os: 'macos',
      created_at: new Date().toISOString(),
      country: 'DE',
    };
  });
  route('POST', '/api/workers/enroll/confirm', (_, body) => {
    const code = String(body?.code ?? '').trim().toUpperCase();
    if (!/^[A-Z0-9]{4}-?[A-Z0-9]{4}$/.test(code)) throw fail(404, 'not_found', 'not found');
    const w = { id: randomUUID(), name: `worker-${code.slice(0, 4).toLowerCase()}`, status: 'offline' };
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
      body = chunks.length ? JSON.parse(Buffer.concat(chunks).toString('utf8')) : undefined;
      const m = url.pathname.match(r.re);
      const params = Object.fromEntries(r.keys.map((k, i) => [k, decodeURIComponent(m[i + 1])]));
      log(`${req.method} ${url.pathname}`);
      const out = (await r.fn(params, body, url.searchParams)) ?? {};
      if (STATUS in out) reply(out[STATUS], out.body);
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
