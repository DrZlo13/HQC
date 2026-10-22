import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { parseEvent, parseFrame, parseServerFrame, ProtoError, isKnownType } from '../src/proto';

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

  it('parses every example file', () => {
    const unhandled = files.filter(
      (f) => !f.startsWith('event.') && !f.startsWith('hub.') && !f.startsWith('worker.'),
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
    const f = parseServerFrame({ type: 'event', payload: { event: e } });
    expect(f.type).toBe('event.invalid');
  });

  it('ignores unknown server frames', () => {
    expect(parseServerFrame({ type: 'presence', payload: {} }).type).toBe('other');
  });

  it('rejects unknown worker frames (frames are strict)', () => {
    expect(() => parseFrame({ id: 'x', type: 'session.dance', payload: {} })).toThrow(ProtoError);
  });
});
