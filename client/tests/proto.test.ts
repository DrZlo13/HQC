import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  isKnownType,
  parseApiError,
  parseArtifact,
  parseArtifactUrl,
  parseCandidateList,
  parseDirList,
  parseEnrollConfirmed,
  parseEnrollInfo,
  parseEvent,
  parseEventPage,
  parseFavorite,
  parseFrame,
  parseInputResponse,
  parseMe,
  parseProject,
  parseProjectList,
  parsePushMessage,
  parseSearchPage,
  parseServerFrame,
  parseSessionInfo,
  parseSessionOrder,
  parseVapidKey,
  parseWorkspace,
  parseWorkspaceConflict,
  parseWorkspaceInspection,
  parseWorkspaceList,
  ProtoError,
  type ClientFrame,
} from '../src/proto';

const WIRE = join(import.meta.dirname, '../../crates/hqc-proto/examples/wire');
const files = readdirSync(WIRE).filter((f) => f.endsWith('.json'));
const load = (f: string): unknown => JSON.parse(readFileSync(join(WIRE, f), 'utf8'));

describe('examples/wire', () => {
  it('has the files the client depends on', () => {
    expect(files).toContain('event.approval.request.json');
    expect(files).toContain('event.unknown.json');
    expect(files.length).toBeGreaterThan(20);
  });

  for (const f of files.filter((f) => f.startsWith('event.'))) {
    it(`parses ${f}`, () => {
      const raw = load(f) as { type: string };
      const ev = parseEvent(raw);
      if (f === 'event.unknown.json') {
        expect(ev.type).toBe('unknown');
        expect(ev.type === 'unknown' && ev.wire_type).toBe(raw.type);
      } else {
        expect(isKnownType(raw.type)).toBe(true);
        expect(ev.type).toBe(raw.type);
        // `event.<type>.json`, or a variant `event.<type>.<variant>.json`.
        expect(f.startsWith(`event.${ev.type}.`)).toBe(true);
        expect(f.slice(`event.${ev.type}.`.length)).toMatch(/^([a-z_]+\.)?json$/);
      }
    });
  }

  it('reads the text answer of approval.response (T26)', () => {
    const ev = parseEvent(load('event.approval.response.message.json'));
    expect(ev.type === 'approval.response' && ev.payload.message).toBe('Leave the root alone, look only at ~/work/hqc');
    const plain = parseEvent(load('event.approval.response.json'));
    expect(plain.type === 'approval.response' && plain.payload.message).toBeUndefined();
  });

  for (const f of files.filter((f) => f.startsWith('hub.') || f.startsWith('worker.'))) {
    it(`parses frame ${f}`, () => {
      const raw = load(f) as { type: string };
      expect(parseFrame(raw).type).toBe(raw.type);
    });
  }

  // /api/ws frames (hqc_proto::client_frame): what the client sends must be exactly the
  // example, what the hub sends must parse into the matching frame.
  const CLIENT_TO_HUB: Record<string, ClientFrame> = {
    'client.subscribe.json': {
      type: 'subscribe',
      session_id: '0199f3a2-7c10-7000-8000-000000000001',
      after_seq: 41,
    },
    'client.unsubscribe.json': { type: 'unsubscribe', session_id: '0199f3a2-7c10-7000-8000-000000000001' },
    'client.subscribe_summary.json': { type: 'subscribe_summary' },
    'client.ping.json': { type: 'ping' },
  };
  for (const [f, frame] of Object.entries(CLIENT_TO_HUB)) {
    it(`sends ${f}`, () => expect(frame).toEqual(load(f)));
  }

  for (const f of files.filter((f) => f.startsWith('client.') && !(f in CLIENT_TO_HUB))) {
    it(`parses hub frame ${f}`, () => {
      const raw = load(f) as { type: string };
      const frame = parseServerFrame(raw);
      expect(frame.type).toBe(raw.type);
    });
  }

  it('reads summary, session.summary and delivery as the hub sends them', () => {
    const s = parseServerFrame(load('client.summary.json'));
    expect(s.type === 'summary' && s.sessions[0]).toMatchObject({ state: 'waiting', unread: 2, last_read_seq: 40 });
    const one = parseServerFrame(load('client.session.summary.json'));
    expect(one.type === 'session.summary' && one.session.title).toBe('hqc: run the tests');
    const d = parseServerFrame(load('client.delivery.json'));
    expect(d).toMatchObject({ type: 'delivery', seq: 42, delivery: 'delivered' });
  });

  // REST bodies (hqc_proto::api).
  it('reads the list order (T50)', () => {
    const o = parseSessionOrder(load('api.session_order.json'));
    expect(o).toEqual(load('api.session_order.request.json'));
    expect(parseSessionOrder({ mode: 'alphabetical', session_ids: [] }).mode).toBe('state');
    expect(() => parseSessionOrder({ mode: 'custom', session_ids: [1] })).toThrow();
  });

  it('reads the session object', () => {
    const s = parseSessionInfo(load('api.session.json'));
    expect(s).toMatchObject({ title: 'hqc: run the tests', state: 'running', unread: 1, archived: false });
    expect(s.system).toBe(false);
    // The worker's service session (T17, SPEC §10.2).
    const sys = parseSessionInfo(load('api.session.system.json'));
    expect(sys).toMatchObject({ system: true, title: 'home-mac', state: 'waiting' });
    expect(sys.workspace_id).toBeUndefined();
    // T27: the runtime's slash commands; absent until a process reported them.
    expect(s.commands).toEqual(['compact', 'context', 'review', 'code-review']);
    expect(sys.commands).toBeUndefined();
    // T34: the session that started it with hqc_spawn.
    expect(s.spawned_by).toEqual({ session_id: '0199f3a2-7c10-7000-8000-000000000002', address: 'hqc/planner' });
    expect(sys.spawned_by).toBeUndefined();
    expect(() => parseSessionInfo({ ...(load('api.session.json') as object), commands: [1] })).toThrow();
    // T50: when it was archived; absent for a live one.
    expect(parseSessionInfo(load('api.session.archived.json'))).toMatchObject({ archived: true, archived_at: '2026-11-01T22:45:00Z' });
    expect(s.archived_at).toBeUndefined();
    // T39: a turn capped by a message, in the session and its summary; the cap in a chain.
    expect(s.permission_capped_by).toBe('hqc/build-bot');
    expect(sys.permission_capped_by).toBeUndefined();
    const one = parseServerFrame(load('client.session.summary.json'));
    expect(one.type === 'session.summary' && one.session.permission_capped_by).toBe('hqc/build-bot');
    const peer = parseEvent(load('event.peer.message.json'));
    expect(peer.chain).toMatchObject({ permission_cap: 'ask', capped_by: 'hqc/build-bot' });
    const usage = parseEvent(load('event.usage.json'));
    expect(usage.type === 'usage' && [usage.payload.cost_usd, usage.payload.session_cost_usd]).toEqual([0.0123, 0.4567]);
    // SPEC 0.68: the subagents the agent runs now, in the session and its summary; absent is 0.
    expect(s.subagents).toBe(2);
    expect(sys.subagents).toBeUndefined();
    expect(one.type === 'session.summary' && one.session.subagents).toBe(2);
    // Favorites: the user's, in the session and its summary; absent is not a favorite.
    expect(s.favorite_at).toBe('2026-10-22T09:30:00Z');
    expect(sys.favorite_at).toBeUndefined();
    expect(one.type === 'session.summary' && one.session.favorite_at).toBe('2026-10-22T09:30:00Z');
    const sum =(load('client.session.summary.json') as { session: Record<string, unknown> }).session;
    for (const k of ['project_id', 'title', 'state', 'queue_len', 'unread', 'last_seq', 'last_read_seq']) {
      expect((s as unknown as Record<string, unknown>)[k], k).toEqual(sum[k]);
    }
  });

  it('reads the other REST bodies', () => {
    expect(parseEventPage(load('api.events.json')).last_seq).toBe(43);
    const sp = parseSearchPage(load('api.search.json'));
    expect(sp.results.map((h) => [h.address, h.seq, h.archived])).toEqual([
      ['hqc/hqc-run-the-tests', 42, false],
      ['personal/migration-plan', undefined, true],
    ]);
    expect(sp.next_cursor).toBeTruthy();
    const input = parseInputResponse(load('api.input.json'));
    expect(input.client_op_id).toBe((load('api.input.request.json') as { client_op_id: string }).client_op_id);
    expect(parseApiError(load('api.error.json')).error).toBe('request_closed');
    expect(parseVapidKey(load('api.vapid.json'))).toMatch(/^B/);
    expect(parseVapidKey({ public_key: null })).toBeNull();
    const push = parsePushMessage(load('api.push.json'));
    expect(push.payload?.type).toBe(push.type);
    expect(push.payload?.session_id).toBe(push.session_id);
    const sub = load('api.push.subscribe.json') as { endpoint: string; keys: Record<string, string> };
    expect(Object.keys(sub.keys).sort()).toEqual(['auth', 'p256dh']);
  });

  it('reads projects, workspaces, folders and the conflict body (T14)', () => {
    const p = parseProject(load('api.project.json'));
    expect(p).toMatchObject({ name: 'hqc', is_personal: false, color: 'plum' });
    expect(p.icon).toEqual({ kind: 'image', url: `/api/projects/${p.id}/icon?v=9f86d081884c7d65` });
    const list = parseProjectList(load('api.projects.json'));
    expect(list.map((x) => x.is_personal)).toEqual([true, false]);
    expect(list[0].default_workspace_id).toBeUndefined();
    // T49: no icon, the letters; an unknown color or icon kind does not break the list.
    expect(list[0]).toMatchObject({ color: 'slate', icon: undefined });
    // Favorites: the user's; the reply of PUT .../favorite.
    expect([p.favorite_at, list[0].favorite_at]).toEqual(['2026-10-24T08:00:00Z', undefined]);
    expect(parseFavorite({ favorite_at: '2026-10-24T08:00:00Z' })).toBe('2026-10-24T08:00:00Z');
    expect(() => parseFavorite({})).toThrow();
    const odd = parseProject({ ...(load('api.project.json') as object), color: 'mauve', icon: { kind: 'svg', value: 'x' } });
    expect(odd).toMatchObject({ color: 'teal', icon: undefined });
    expect(parseMe(load('api.me.json')).theme).toBe('contrast');
    expect(parseMe({ ...(load('api.me.json') as object), theme: undefined }).theme).toBe('warm');
    const w = parseWorkspace(load('api.workspace.json'));
    expect(p.default_workspace_id).toBe(w.id);
    expect(w).toMatchObject({ project_id: p.id, kind: 'dir', label: 'home-mac' });
    expect(parseWorkspaceList(load('api.workspaces.json'))).toEqual([w]);
    // T45: a clone's state and error; a hub before T45 sends no state.
    expect(w).toMatchObject({ state: 'ready', created_by_hqc: false });
    expect(parseWorkspace(load('api.workspace.clone.json'))).toMatchObject({
      kind: 'clone',
      created_by_hqc: true,
      state: 'failed',
      error: 'git clone failed: fatal: Could not read from remote repository.',
    });
    // T46: a worktree names its origin and branch.
    const clone = parseWorkspace(load('api.workspace.clone.json'));
    expect(parseWorkspace(load('api.workspace.worktree.json'))).toMatchObject({
      kind: 'worktree',
      origin_workspace_id: clone.id,
      branch: 'hqc/fix-backlight',
      state: 'creating',
    });
    expect(w.origin_workspace_id).toBeUndefined();
    // T47: the worktree of a spawned session, marked once its sessions are archived.
    expect(parseWorkspace(load('api.workspace.ephemeral.json'))).toMatchObject({
      kind: 'worktree',
      origin_workspace_id: clone.id,
      branch: 'hqc/session-7',
      ephemeral: true,
      marked_for_deletion: true,
    });
    expect(w).toMatchObject({ ephemeral: false, marked_for_deletion: false });
    const { state: _, ...old } = load('api.workspace.json') as Record<string, unknown>;
    expect(parseWorkspace(old).state).toBe('ready');
    expect(parseWorkspace({ ...old, state: 'moving' }).state).toBe('unknown');
    expect(parseWorkspaceInspection(load('api.workspace.inspect.json'))).toEqual({
      git: true,
      dirty: ['src/panel.rs', 'notes.txt'],
      dirty_count: 2,
      unpushed: [{ branch: 'hqc/fix-backlight', commits: 3 }],
    });
    expect(parseDirList(load('api.dirs.json')).map((d) => d.name)).toEqual(['cad', 'hqc']);
    expect(parseWorkspaceConflict(load('api.workspace.conflict.json'))).toEqual({
      existing_workspace_id: w.id,
      existing_project_id: p.id,
      existing_path: w.path,
    });
    expect(parseWorkspaceConflict(load('api.error.json'))).toBeNull();
    expect(parseArtifact(load('api.artifact.json'))).toMatchObject({ kind: 'image', name: 'screen.png', size: 48213 });
    expect(parseArtifactUrl(load('api.artifact.url.json')).url).toMatch(/^https:\/\/artifacts\./);
    // T15: candidates for the import from Claude Code; a mirrored workspace.
    expect(parseCandidateList(load('api.candidates.json')).map((c) => c.sessions_count)).toEqual([3, 41]);
    expect(w.mirror_enabled).toBe(false);
    expect(parseWorkspace({ ...(load('api.workspace.json') as object), mirror_enabled: true }).mirror_enabled).toBe(true);
    const s = parseSessionInfo(load('api.session.json'));
    expect(s).toMatchObject({ workspace_id: w.id, cwd: '.' });
    const req = load('api.session.request.json') as { workspace_id: string };
    expect(req.workspace_id).toBe(w.id);
    // T19: enrollment that replaces a worker.
    expect(parseEnrollInfo(load('api.enroll.info.json')).replaceable).toEqual([]);
    const info = parseEnrollInfo(load('api.enroll.info.replace.json'));
    expect(info.replaceable.map((r) => [r.name, r.status, r.sessions_count])).toEqual([
      ['home-mac-old', 'offline', 7],
      ['llm-server', 'revoked', 0],
    ]);
    expect(parseEnrollConfirmed(load('api.enroll.confirm.json')).replaced).toBeUndefined();
    const c = parseEnrollConfirmed(load('api.enroll.confirm.replace.json'));
    expect(c.replaced).toBe('home-mac-old');
    expect((load('api.enroll.confirm.replace.request.json') as { replace_worker_id: string }).replace_worker_id).toBe(
      c.worker_id,
    );
  });

  it('parses every example file', () => {
    const unhandled = files.filter(
      (f) => !['event.', 'hub.', 'worker.', 'client.', 'api.'].some((p) => f.startsWith(p)),
    );
    expect(unhandled).toEqual([]);
  });
});

describe('tolerance (SPEC §4)', () => {
  const approval = load('event.approval.request.json') as any;

  it('reads unknown enum values as unknown', () => {
    const e = structuredClone(approval);
    e.payload.options[0].canonical = 'allow_forever';
    const ev = parseEvent(e);
    expect(ev.type === 'approval.request' && ev.payload.options[0].canonical).toBe('unknown');

    const h = structuredClone(approval);
    h.payload.policy_hint = 'audit';
    const hev = parseEvent(h);
    expect(hev.type === 'approval.request' && hev.payload.policy_hint).toBe('unknown');
    h.payload.policy_hint = null;
    const nev = parseEvent(h);
    expect(nev.type === 'approval.request' && nev.payload.policy_hint).toBeUndefined();

    const s = structuredClone(load('event.status.json')) as any;
    s.payload.state = 'hibernating';
    const st = parseEvent(s);
    expect(st.type === 'status' && st.payload.state).toBe('unknown');
  });

  it('ignores unknown payload fields and accepts null for optional ones', () => {
    const e = structuredClone(approval);
    e.payload.extra = { anything: 1 };
    e.payload.expires_at = null;
    const ev = parseEvent(e);
    expect(ev.type === 'approval.request' && ev.payload.expires_at).toBeUndefined();
  });

  it('reads the header and option descriptions of a question, absent ones too', () => {
    const e = structuredClone(load('event.clarify.request.json')) as any;
    e.payload.questions[0].header = 'Focus';
    e.payload.questions[0].option_descriptions = ['axum + Postgres hub work', ''];
    delete e.payload.questions[1].header;
    delete e.payload.questions[1].option_descriptions;
    const ev = parseEvent(e);
    if (ev.type !== 'clarify.request') throw new Error('not a clarify.request');
    expect(ev.payload.questions[0]).toMatchObject({
      header: 'Focus',
      option_descriptions: ['axum + Postgres hub work', ''],
    });
    expect(ev.payload.questions[1].header).toBeUndefined();
    expect(ev.payload.questions[1].option_descriptions).toBeUndefined();
  });

  it('rejects a known type with a wrong payload', () => {
    const e = structuredClone(approval);
    delete e.payload.options;
    expect(() => parseEvent(e)).toThrow(ProtoError);
  });

  it('turns a bad event on the client socket into event.invalid, not an exception', () => {
    const e = structuredClone(approval);
    e.payload.tool = 5;
    const f = parseServerFrame({ type: 'event', event: e });
    expect(f.type).toBe('event.invalid');
  });

  it('ignores unknown server frames', () => {
    expect(parseServerFrame({ type: 'presence' }).type).toBe('other');
  });

  it('rejects unknown worker frames (frames are strict)', () => {
    expect(() => parseFrame({ id: 'x', type: 'session.dance', payload: {} })).toThrow(ProtoError);
  });
});
