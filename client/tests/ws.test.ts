// The hub socket's compressed stream (SPEC §6): `?deflate=raw`, binary messages that are one raw
// DEFLATE stream with a sync flush after each frame, each frame's JSON followed by "\n".

import { constants, createDeflateRaw } from 'node:zlib';
import { expect, it, vi } from 'vitest';
import type { SessionSummary } from '../src/proto';
import { HubSocket } from '../src/ws';

class FakeWs {
  static all: FakeWs[] = [];
  readyState = 1;
  binaryType = 'blob';
  closed = false;
  onopen: (() => void) | null = null;
  onmessage: ((m: { data: unknown }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(readonly url: string) {
    FakeWs.all.push(this);
  }
  send(): void {}
  close(): void {
    this.closed = true;
    this.onclose?.();
  }
}

const summary = (title: string) => ({
  type: 'session.summary',
  session: { session_id: 's1', project_id: 'p1', title, state: 'idle', queue_len: 0, unread: 0, last_seq: 1, last_read_seq: 1 },
});

/** The bytes the hub would send for each frame: one stream, a sync flush after each. */
async function packed(frames: unknown[]): Promise<Uint8Array[]> {
  const z = createDeflateRaw();
  let chunks: Buffer[] = [];
  z.on('data', (c: Buffer) => chunks.push(c));
  const out: Uint8Array[] = [];
  for (const f of frames) {
    z.write(JSON.stringify(f) + '\n');
    await new Promise<void>((r) => z.flush(constants.Z_SYNC_FLUSH, r));
    out.push(new Uint8Array(Buffer.concat(chunks)));
    chunks = [];
  }
  z.destroy();
  return out;
}

const bytes = (b: Uint8Array): ArrayBuffer => b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength) as ArrayBuffer;
const join = (...parts: Uint8Array[]): Uint8Array => new Uint8Array(Buffer.concat(parts));

function connect(url = 'ws://hub/api/ws') {
  FakeWs.all = [];
  const socket = new HubSocket({ url, WebSocketImpl: FakeWs as unknown as new (url: string) => WebSocket });
  const titles: string[] = [];
  socket.onSummary((l: SessionSummary[]) => titles.push(l[0].title));
  socket.start();
  const ws = FakeWs.all[0];
  ws.onopen!();
  return { socket, ws, titles };
}

it('asks for the compressed stream and takes binary messages as arraybuffers', () => {
  const { ws, socket } = connect();
  expect(ws.url).toBe('ws://hub/api/ws?deflate=raw');
  expect(ws.binaryType).toBe('arraybuffer');
  socket.stop();
  expect(connect('ws://hub/api/ws?x=1').ws.url).toBe('ws://hub/api/ws?x=1&deflate=raw');
});

it('reads frames of one stream: one to a message, two in one, one split over two, text as it is', async () => {
  const { ws, socket, titles } = connect();
  const [a, b, c, d] = await packed([summary('first'), summary('second'), summary('third — with é, я and 日本'), summary('fourth')]);
  ws.onmessage!({ data: bytes(a) });
  ws.onmessage!({ data: bytes(b) });
  const half = Math.floor(c.length / 2);
  ws.onmessage!({ data: bytes(c.subarray(0, half)) });
  await vi.waitFor(() => expect(titles).toEqual(['first', 'second']));
  ws.onmessage!({ data: bytes(join(c.subarray(half), d)) });
  await vi.waitFor(() => expect(titles).toEqual(['first', 'second', 'third — with é, я and 日本', 'fourth']));
  ws.onmessage!({ data: JSON.stringify(summary('text')) });
  expect(titles.at(-1)).toBe('text');
  socket.stop();
});

it('reads two frames in a single message', async () => {
  const { ws, socket, titles } = connect();
  const [a, b] = await packed([summary('one'), summary('two')]);
  ws.onmessage!({ data: bytes(join(a, b)) });
  await vi.waitFor(() => expect(titles).toEqual(['one', 'two']));
  socket.stop();
});

it('a listener that throws loses its frame, and the next frame of the stream still comes', async () => {
  const { ws, socket, titles } = connect();
  const error = vi.spyOn(console, 'error').mockImplementation(() => {});
  let thrown = false;
  socket.onSummary(() => {
    if (thrown) return;
    thrown = true;
    throw new Error('a listener failed');
  });
  const [a, b] = await packed([summary('one'), summary('two')]);
  ws.onmessage!({ data: bytes(a) });
  ws.onmessage!({ data: bytes(b) });
  await vi.waitFor(() => expect(titles).toEqual(['one', 'two']));
  expect(ws.closed).toBe(false);
  expect(error).toHaveBeenCalledTimes(1);
  error.mockRestore();
  socket.stop();
});

it('drops what comes out of the stream after the socket was replaced, and reconnects on a broken stream', async () => {
  const { ws, socket, titles } = connect();
  const [a] = await packed([summary('late')]);
  ws.onmessage!({ data: bytes(a) });
  // The socket drops before the frame is inflated.
  ws.close();
  await new Promise((r) => setTimeout(r, 50));
  expect(titles).toEqual([]);
  socket.stop();

  const next = connect();
  next.ws.onmessage!({ data: bytes(new Uint8Array([0xff, 0xff, 0xff, 0xff, 0xff])) });
  await vi.waitFor(() => expect(next.ws.closed).toBe(true));
  expect(next.titles).toEqual([]);
  next.socket.stop();
});

it('tells a summary of every session from one session that changed', async () => {
  const { ws, socket } = connect();
  const seen: [string[], boolean][] = [];
  socket.onSummary((l, whole) => seen.push([l.map((s) => s.title), whole]));
  const one = summary('changed').session;
  ws.onmessage!({ data: JSON.stringify({ type: 'summary', sessions: [one, { ...one, session_id: 's2', title: 'other' }] }) });
  ws.onmessage!({ data: JSON.stringify(summary('changed')) });
  expect(seen).toEqual([
    [['changed', 'other'], true],
    [['changed'], false],
  ]);
  socket.stop();
});
