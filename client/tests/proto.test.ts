import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  isKnownType,
  parseApiError,
  parseEvent,
  parseEventPage,
  parseFrame,
  parseInputResponse,
  parsePushMessage,
  parseServerFrame,
  parseSessionInfo,
  parseVapidKey,
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
        expect(`event.${ev.type}.json`).toBe(f);
      }
    });
  }

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
  it('reads the session object', () => {
    const s = parseSessionInfo(load('api.session.json'));
    expect(s).toMatchObject({ title: 'hqc: run the tests', state: 'running', unread: 1, archived: false });
    const sum = (load('client.session.summary.json') as { session: Record<string, unknown> }).session;
    for (const k of ['project_id', 'title', 'state', 'queue_len', 'unread', 'last_seq', 'last_read_seq']) {
      expect((s as unknown as Record<string, unknown>)[k], k).toEqual(sum[k]);
    }
  });

  it('reads the other REST bodies', () => {
    expect(parseEventPage(load('api.events.json')).last_seq).toBe(43);
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
