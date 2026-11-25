// @vitest-environment happy-dom
// History a chat does not download: the older pages load on demand, and what the device
// already holds of a session (`HistoryCache`) shows before the hub answers.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { ApiError, eventPageOf, type Api, type EventPage } from '../src/api';
import { AppState } from '../src/app';
import { memoryHistoryCache, noHistoryCache, type HistoryCache } from '../src/historycache';
import { parseEvent, type DeliveryChange, type SessionInfo } from '../src/proto';
import { ChatSession } from '../src/session';
import { HubSocket, type SessionSubscriber } from '../src/ws';
import { Chat } from '../src/ui/Chat';

const SID = 's1';
const eventId = (seq: number, tag = '') => `0199f3a2-7c10-7000-8000-${tag}${String(seq).padStart(12 - tag.length, '0')}`;

/** An event of the hub as the wire carries it. */
const wire = (seq: number, more: Record<string, unknown> = {}) => ({
  session_id: SID,
  event_id: eventId(seq),
  seq,
  ts: '2026-10-24T18:00:00Z',
  author: { kind: 'agent', id: SID },
  type: 'agent.message',
  payload: { text: `answer ${seq}`, final: true },
  ...more,
});
const range = (from: number, to: number, more: Record<string, unknown> = {}) =>
  Array.from({ length: to - from + 1 }, (_, i) => wire(from + i, more));
const usage = (seq: number) =>
  wire(seq, { type: 'usage', payload: { input_tokens: 1, output_tokens: 2, cost_usd: 0.5, model: 'claude-opus-5-5' } });

/** The hub with `events` stored: the pages `eventsBefore` answers. */
function hub(events: unknown[]) {
  const api = {
    eventsBefore: vi.fn(async (_id: string, before = Number.MAX_SAFE_INTEGER, limit = 200): Promise<EventPage> =>
      eventPageOf(events.filter((e) => (e as { seq: number }).seq < before).slice(-limit), 0),
    ),
  };
  return { api, events };
}

function session(api: object, cache: HistoryCache) {
  const subs: SessionSubscriber[] = [];
  const socket = { subscribe: vi.fn((_id: string, sub: SessionSubscriber) => (subs.push(sub), () => {})) } as unknown as HubSocket;
  const chat = new ChatSession(api as Api, socket, SID, cache);
  return { chat, subs, socket };
}

/** The event of `wire(seq)`, parsed. */
const parsed = (seq: number) => eventPageOf([wire(seq)], 0).events[0];
const seqs = (chat: ChatSession) => chat.timeline.events().map((e) => e.seq!);
const tick = (ms = 0) => new Promise((r) => setTimeout(r, ms));
/** Waits for `cond` (a run that starts with a test pays for the first imports). */
async function until(cond: () => boolean): Promise<void> {
  for (let i = 0; i < 300 && !cond(); i++) await act(async () => void (await tick(10)));
}

describe('a chat with no copy on the device', () => {
  it('asks for the newest page only when it opens', async () => {
    const { api } = hub(range(1, 450));
    const { chat } = session(api, noHistoryCache);
    await chat.open();
    expect(api.eventsBefore.mock.calls).toEqual([[SID, undefined, 100]]);
    expect(seqs(chat)).toHaveLength(100);
    expect(chat.timeline.olderFrom).toBe(351);
    await tick(20);
    expect(api.eventsBefore).toHaveBeenCalledTimes(1);
  });

  it('a history that is one page needs no older one', async () => {
    const { api } = hub(range(1, 100));
    const { chat } = session(api, noHistoryCache);
    await chat.open();
    expect(chat.timeline.olderFrom).toBe(0);
  });

  it('loadOlder loads one page of 200 before what is loaded, one call at a time', async () => {
    const { api } = hub(range(1, 450));
    const { chat } = session(api, noHistoryCache);
    await chat.open();
    const first = chat.loadOlder();
    expect(chat.loadingOlder).toBe(true);
    expect(chat.loadOlder()).toBe(first);
    await first;
    expect(chat.loadingOlder).toBe(false);
    expect(api.eventsBefore.mock.calls.slice(1)).toEqual([[SID, 351, 200]]);
    expect(chat.timeline.olderFrom).toBe(151);
    expect(seqs(chat)[0]).toBe(151);
    await chat.loadOlder();
    expect(chat.timeline.olderFrom).toBe(0);
    expect(seqs(chat)).toHaveLength(450);
  });

  it('loadOlder down to a seq loads pages of 1000 until the seq is in', async () => {
    const { api } = hub(range(1, 3000));
    const { chat } = session(api, noHistoryCache);
    await chat.open();
    await chat.loadOlder(1500);
    expect(api.eventsBefore.mock.calls.slice(1)).toEqual([
      [SID, 2901, 1000],
      [SID, 1901, 1000],
    ]);
    expect(chat.timeline.olderFrom).toBe(901);
  });

  it('close stops the older pages', async () => {
    const { api } = hub(range(1, 3000));
    const { chat } = session(api, noHistoryCache);
    await chat.open();
    const loading = chat.loadOlder(1);
    chat.close();
    await loading;
    expect(api.eventsBefore).toHaveBeenCalledTimes(2);
    expect(chat.timeline.olderFrom).toBe(2901);
  });

  it('keeps the page and the events of the subscription on the device', async () => {
    const cache = memoryHistoryCache();
    const { api } = hub(range(1, 450));
    const { chat, subs } = session(api, cache);
    await chat.open();
    await tick();
    expect(await cache.range(SID)).toEqual({ from: 351, to: 450 });
    subs[0].onEvent(parsed(451), wire(451));
    subs[0].onInvalid(wire(452, { type: 'agent.message', payload: 'bad' }), 'bad payload');
    // Not at once: together with the ones that follow.
    expect(await cache.range(SID)).toEqual({ from: 351, to: 450 });
    await tick(350);
    expect(await cache.range(SID)).toEqual({ from: 351, to: 452 });
    expect(((await cache.read(SID, 452, 452)) as { payload: unknown }[])[0].payload).toBe('bad');
  });

  it('keeps the events that wait for the timer when the chat closes', async () => {
    const cache = memoryHistoryCache();
    const { api } = hub(range(1, 150));
    const { chat, subs } = session(api, cache);
    await chat.open();
    await tick();
    subs[0].onEvent(parsed(151), wire(151));
    chat.close();
    await tick();
    expect(await cache.range(SID)).toEqual({ from: 51, to: 151 });
  });

  it('a delivery frame changes the event the device holds', async () => {
    const cache = memoryHistoryCache();
    const input = wire(101, { type: 'user.message', payload: { text: 'hi', artifact_ids: [] }, author: { kind: 'user', id: 'u' }, delivery: 'pending' });
    const { api } = hub([...range(1, 100), input]);
    const { chat, subs } = session(api, cache);
    await chat.open();
    await tick();
    const d: DeliveryChange = { session_id: SID, event_id: eventId(101), seq: 101, delivery: 'delivered', delivered_at: '2026-10-24T18:01:00Z', joined: true };
    subs[0].onDelivery(d);
    await tick();
    const [kept] = (await cache.read(SID, 101, 101)) as { delivery: string; delivered_at: string; payload: { joined: boolean; text: string } }[];
    expect([kept.delivery, kept.delivered_at, kept.payload.joined, kept.payload.text]).toEqual(['delivered', '2026-10-24T18:01:00Z', true, 'hi']);
    // Another event at that seq is left alone.
    subs[0].onDelivery({ ...d, event_id: eventId(101, 'f'), delivery: 'failed' });
    await tick();
    expect(((await cache.read(SID, 101, 101)) as { delivery: string }[])[0].delivery).toBe('delivered');
  });

  it('a session the hub does not know is forgotten by the device', async () => {
    const cache = memoryHistoryCache();
    await cache.put(SID, range(1, 10));
    const api = { eventsBefore: vi.fn(async () => Promise.reject(new ApiError(404, 'no session'))) };
    const { chat } = session(api, {
      ...cache,
      // Its copy cannot be read: the newest page is asked for, and answers 404.
      read: async () => undefined,
    });
    await expect(chat.open()).resolves.toBeUndefined();
    expect(await cache.range(SID)).toBeUndefined();
  });
});

describe('older pages that fail', () => {
  it('an automatic call waits for the user after a failed page; a success or an open clears it', async () => {
    const { api } = hub(range(1, 450));
    const page = api.eventsBefore.getMockImplementation()!;
    let down = true;
    api.eventsBefore.mockImplementation(async (...a: Parameters<typeof page>) => {
      if (a[1] !== undefined && down) throw new Error('hub down');
      return page(...a);
    });
    const { chat } = session(api, noHistoryCache);
    await chat.open();
    await chat.loadOlder(undefined, true);
    await chat.loadOlder(undefined, true);
    await chat.loadOlder(351, true);
    expect(api.eventsBefore).toHaveBeenCalledTimes(2);
    // The user's own call tries, and succeeds.
    down = false;
    await chat.loadOlder();
    expect(chat.timeline.olderFrom).toBe(151);
    // Failed again, then an open: automatic calls work.
    down = true;
    await chat.loadOlder();
    await chat.loadOlder(undefined, true);
    expect(api.eventsBefore).toHaveBeenCalledTimes(4);
    down = false;
    await chat.open();
    await chat.loadOlder(undefined, true);
    expect(chat.timeline.olderFrom).toBe(0);
  });
});

describe('a chat with a copy on the device', () => {
  /** The device holds events 301..450 (as the hub sent them), the hub `events`; its newest page
   *  is held until `release`. */
  async function held(events: unknown[], cache = memoryHistoryCache()) {
    await cache.put(SID, range(301, 450));
    const { api } = hub(events);
    const page = api.eventsBefore.getMockImplementation()!;
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    api.eventsBefore.mockImplementation(async (...a: Parameters<typeof page>) => (await gate, page(...a)));
    return { api, cache, release };
  }

  it('shows the newest 100 of it before the hub answers, and subscribes after the hub did', async () => {
    const { api, cache, release } = await held(range(1, 460));
    const { chat, subs } = session(api, cache);
    await chat.open();
    expect(chat.loaded).toBe(true);
    expect(seqs(chat)).toEqual(range(351, 450).map((e) => e.seq));
    expect(chat.timeline.olderFrom).toBe(351);
    expect(chat.timeline.lastSeq).toBe(450);
    expect(subs).toHaveLength(0);
    release();
    await tick(10);
    // The hub's page goes on from the copy: the new events join it.
    expect(seqs(chat).at(-1)).toBe(460);
    expect(seqs(chat)).toHaveLength(110);
    expect(subs).toHaveLength(1);
    expect(subs[0].afterSeq()).toBe(460);
    expect(await cache.range(SID)).toEqual({ from: 301, to: 460 });
  });

  it('shows an event of the copy that does not parse as an invalid one', async () => {
    const cache = memoryHistoryCache();
    await cache.put(SID, [...range(1, 99), wire(100, { payload: 'bad' })]);
    const { api } = hub(range(1, 100));
    api.eventsBefore.mockReturnValue(new Promise(() => {}));
    const { chat } = session(api, cache);
    await chat.open();
    expect(seqs(chat)).toHaveLength(99);
    expect(chat.timeline.items().filter((i) => i.kind === 'invalid')).toHaveLength(1);
    expect(chat.timeline.olderFrom).toBe(0);
  });

  it('a hub that is ahead by more than a page drops the copy from the screen and from the device', async () => {
    const { api, cache, release } = await held(range(1, 700));
    const { chat } = session(api, cache);
    await chat.open();
    chat.timeline.addPending('op', 'not stored yet');
    release();
    await tick(10);
    expect(seqs(chat)).toEqual(range(601, 700).map((e) => e.seq));
    expect(chat.timeline.olderFrom).toBe(601);
    expect(chat.timeline.items().some((i) => i.kind === 'pending')).toBe(true);
    expect(await cache.range(SID)).toEqual({ from: 601, to: 700 });
    expect(await cache.read(SID, 301, 450)).toBeUndefined();
  });

  it('another event at a seq both have drops the copy', async () => {
    const { api, cache, release } = await held([...range(1, 400), ...range(401, 450).map((e) => ({ ...e, event_id: eventId(e.seq, 'a') }))]);
    const { chat } = session(api, cache);
    await chat.open();
    release();
    await tick(10);
    expect(chat.timeline.events().map((e) => e.event_id)).toEqual(range(351, 450).map((e) => eventId(e.seq, e.seq > 400 ? 'a' : '')));
    expect(chat.timeline.olderFrom).toBe(351);
  });

  it('a hub with less than the copy drops it', async () => {
    const { api, cache, release } = await held(range(1, 320));
    const { chat } = session(api, cache);
    await chat.open();
    release();
    await tick(10);
    expect(seqs(chat)).toEqual(range(221, 320).map((e) => e.seq));
    expect(await cache.range(SID)).toEqual({ from: 221, to: 320 });
  });

  it('a copy it drops takes its task list along, and the hub\'s list at a lower seq shows', async () => {
    const tasks = (seq: number, subject: string) =>
      wire(seq, { type: 'tasks', payload: { items: [{ subject, status: 'in_progress' }] } });
    const { api, cache, release } = await held([...range(1, 309), tasks(310, 'Hub'), ...range(311, 320)]);
    await cache.put(SID, [tasks(440, 'Copy')]);
    const { chat } = session(api, cache);
    await chat.open();
    expect(chat.timeline.tasks.map((t) => t.subject)).toEqual(['Copy']);
    release();
    await tick(10);
    expect(chat.timeline.tasks.map((t) => t.subject)).toEqual(['Hub']);
  });

  it('a session the hub does not know shows nothing, and the device forgets it', async () => {
    const { api, cache, release } = await held(range(1, 450));
    const gated = api.eventsBefore.getMockImplementation()!;
    api.eventsBefore.mockImplementation(async (...a: Parameters<typeof gated>) => {
      await gated(...a).catch(() => {});
      throw new ApiError(404, 'no session');
    });
    const { chat } = session(api, cache);
    await chat.open();
    expect(seqs(chat)).toHaveLength(100);
    release();
    await tick(10);
    expect(seqs(chat)).toHaveLength(0);
    expect(await cache.range(SID)).toBeUndefined();
  });

  it('a copy that holds the whole history keeps olderFrom 0 when the hub\'s page is only the newest 100', async () => {
    const cache = memoryHistoryCache();
    await cache.put(SID, range(1, 100));
    const { api } = hub(range(1, 120));
    const { chat } = session(api, cache);
    await chat.open();
    expect(chat.timeline.olderFrom).toBe(0);
    await tick(10);
    expect(seqs(chat)).toHaveLength(120);
    expect(chat.timeline.olderFrom).toBe(0);
  });

  it('an event the hub sends again rewrites its copy inside the range and moves nothing else', async () => {
    const cache = memoryHistoryCache();
    const { api } = hub(range(1, 450));
    const { chat, subs } = session(api, cache);
    await chat.open();
    await tick();
    expect(await cache.range(SID)).toEqual({ from: 351, to: 450 });
    // Below the range: it would have replaced the range, and the copy with it.
    subs[0].onEvent(parsed(100), wire(100, { delivery: 'delivered' }));
    // Inside: its copy is rewritten.
    subs[0].onEvent(parsed(440), wire(440, { delivery: 'delivered' }));
    // Above lastSeq: new, the range grows.
    subs[0].onEvent(parsed(451), wire(451));
    await tick(350);
    expect(await cache.range(SID)).toEqual({ from: 351, to: 451 });
    expect(await cache.read(SID, 100, 100)).toBeUndefined();
    expect(((await cache.read(SID, 440, 440)) as { delivery: string }[])[0].delivery).toBe('delivered');
    expect(await cache.read(SID, 351, 451)).toHaveLength(101);
  });

  it('the older pages come from the device while it holds them, then from the hub', async () => {
    const cache = memoryHistoryCache();
    await cache.put(SID, range(101, 450));
    const { api } = hub(range(1, 450));
    const { chat } = session(api, cache);
    await chat.open();
    await tick(10);
    expect(api.eventsBefore).toHaveBeenCalledTimes(1);
    await chat.loadOlder();
    await chat.loadOlder();
    expect(api.eventsBefore).toHaveBeenCalledTimes(1);
    expect(chat.timeline.olderFrom).toBe(101);
    expect(seqs(chat)[0]).toBe(101);
    // Below the device's range: the hub, which the device then keeps.
    await chat.loadOlder();
    expect(api.eventsBefore.mock.calls[1]).toEqual([SID, 101, 200]);
    expect(chat.timeline.olderFrom).toBe(0);
    expect(await cache.range(SID)).toEqual({ from: 1, to: 450 });
  });

  it('an input the copy holds as pending is asked of the hub: it was delivered since', async () => {
    const input = (delivery: string) =>
      wire(200, { type: 'user.message', payload: { text: 'hi', artifact_ids: [] }, author: { kind: 'user', id: 'u' }, delivery });
    const cache = memoryHistoryCache();
    await cache.put(SID, [...range(101, 199), input('pending'), ...range(201, 450)]);
    const { api } = hub([...range(1, 199), input('delivered'), ...range(201, 460)]);
    const { chat } = session(api, cache);
    await chat.open();
    await tick(10);
    expect(chat.timeline.olderFrom).toBe(351);
    await chat.loadOlder();
    expect(api.eventsBefore.mock.calls[1]).toEqual([SID, 351, 200]);
    expect(chat.timeline.events().find((e) => e.seq === 200)).toMatchObject({ delivery: 'delivered' });
    // The hub's page rewrote the copy.
    expect(((await cache.read(SID, 200, 200)) as { delivery: string }[])[0].delivery).toBe('delivered');
  });

  it('a take-back while the copy is checked waits for the hub\'s page, and the copy keeps it', async () => {
    const input = wire(450, { type: 'user.message', payload: { text: 'hi', artifact_ids: [] }, author: { kind: 'user', id: 'u' }, delivery: 'pending' });
    // The hub's newest page, asked for before the take-back, still has the input waiting.
    const { api, cache, release } = await held([...range(1, 449), input]);
    await cache.put(SID, [input]);
    const withdrawInput = vi.fn(async () => {});
    Object.assign(api, { withdrawInput });
    const { chat } = session(api, cache);
    await chat.open();
    expect(seqs(chat)).toContain(450);
    const taking = chat.withdraw({ event_id: eventId(450), seq: 450 });
    await tick(10);
    expect(withdrawInput).not.toHaveBeenCalled();
    release();
    await taking;
    await tick(10);
    expect(withdrawInput).toHaveBeenCalledWith(SID, eventId(450));
    expect(seqs(chat)).not.toContain(450);
    expect(((await cache.read(SID, 450, 450)) as { delivery: string }[])[0].delivery).toBe('withdrawn');
  });

  it('a take-back whose frame never comes leaves the copy withdrawn', async () => {
    const cache = memoryHistoryCache();
    const input = wire(101, { type: 'user.message', payload: { text: 'hi', artifact_ids: [] }, author: { kind: 'user', id: 'u' }, delivery: 'pending' });
    const { api } = hub([...range(1, 100), input]);
    Object.assign(api, { withdrawInput: vi.fn(async () => {}) });
    const { chat } = session(api, cache);
    await chat.open();
    await chat.withdraw({ event_id: eventId(101), seq: 101 });
    await tick(10);
    expect(((await cache.read(SID, 101, 101)) as { delivery: string }[])[0].delivery).toBe('withdrawn');
  });

  it('an event the device does not hold sends the page to the hub', async () => {
    const cache = memoryHistoryCache();
    await cache.put(SID, range(1, 450));
    const { api } = hub(range(1, 450));
    // The copy claims events 1..450 and cannot give the older page.
    const lacking: HistoryCache = { ...cache, read: async (s, lo, hi) => (hi > 350 ? cache.read(s, lo, hi) : undefined) };
    const { chat } = session(api, lacking);
    await chat.open();
    await tick(10);
    await chat.loadOlder();
    expect(api.eventsBefore.mock.calls[1]).toEqual([SID, 351, 200]);
    expect(seqs(chat)[0]).toBe(151);
  });

  it('a chat kept in memory that opens again keeps its newest page on the device', async () => {
    const cache = memoryHistoryCache();
    const events = range(1, 150);
    const { api } = hub(events);
    const { chat } = session(api, cache);
    await chat.open();
    chat.close();
    events.push(...range(151, 160));
    await chat.open();
    await tick(10);
    expect(await cache.range(SID)).toEqual({ from: 51, to: 160 });
  });
});

describe('the events of a subscription before its `subscribed`', () => {
  /** An open chat with a listener on its timeline, and the delta of an answer's text. */
  async function opened() {
    const { api } = hub(range(1, 10));
    const { chat, subs } = session(api, noHistoryCache);
    await chat.open();
    await tick();
    const drawn = vi.fn();
    chat.timeline.onChange(drawn);
    return { chat, subs, drawn };
  }
  const feed = (from: number, to: number, sub: SessionSubscriber) => {
    for (let seq = from; seq <= to; seq++) sub.onEvent(parsed(seq), wire(seq));
  };

  it('are not drawn one by one: nothing before `subscribed`, once at it, then each one', async () => {
    const { chat, subs, drawn } = await opened();
    feed(11, 60, subs[0]);
    expect(drawn).not.toHaveBeenCalled();
    subs[0].onSubscribed!();
    expect(drawn).toHaveBeenCalledTimes(1);
    expect(seqs(chat)).toEqual(Array.from({ length: 60 }, (_, i) => i + 1));
    drawn.mockClear();
    feed(61, 62, subs[0]);
    expect(drawn).toHaveBeenCalledTimes(2);
    chat.close();
  });

  it('a backfill is drawn every 100 ms before `subscribed`', async () => {
    const { chat, subs, drawn } = await opened();
    feed(11, 20, subs[0]);
    expect(drawn).not.toHaveBeenCalled();
    await tick(150);
    expect(drawn).toHaveBeenCalledTimes(1);
    feed(21, 30, subs[0]);
    await tick(150);
    expect(drawn).toHaveBeenCalledTimes(2);
    subs[0].onSubscribed!();
    expect(drawn).toHaveBeenCalledTimes(2);
    chat.close();
  });

  it('a socket reset starts a catch-up again: the events wait for `subscribed`, and one draw shows them', async () => {
    const { chat, subs, drawn } = await opened();
    subs[0].onSubscribed!();
    subs[0].onReset();
    drawn.mockClear();
    feed(11, 60, subs[0]);
    expect(drawn).not.toHaveBeenCalled();
    subs[0].onSubscribed!();
    expect(drawn).toHaveBeenCalledTimes(1);
    chat.close();
  });

  it('a socket reset draws the events that waited for a draw', async () => {
    const { chat, subs, drawn } = await opened();
    feed(11, 13, subs[0]);
    subs[0].onReset();
    expect(drawn).toHaveBeenCalledTimes(1);
    await tick(150);
    expect(drawn).toHaveBeenCalledTimes(1);
    chat.close();
  });

  it('closing the chat in the middle of a catch-up draws what waited, and no timer draws after it', async () => {
    const { chat, subs, drawn } = await opened();
    feed(11, 20, subs[0]);
    chat.close();
    expect(drawn).toHaveBeenCalledTimes(1);
    await tick(150);
    expect(drawn).toHaveBeenCalledTimes(1);
  });

  it('a `subscribe` that was not sent is not counted: the first `subscribed` ends the catch-up', async () => {
    const sockets: { readyState: number; onopen?: () => void; onmessage?: (m: { data: string }) => void }[] = [];
    class FakeWs {
      readyState = 0;
      onopen?: () => void;
      onmessage?: (m: { data: string }) => void;
      constructor() {
        sockets.push(this);
      }
      send() {}
      close() {}
    }
    const socket = new HubSocket({ url: 'ws://x/api/ws', WebSocketImpl: FakeWs as never });
    const onSubscribed = vi.fn();
    socket.start();
    // Not open yet: the frame is not sent, `onopen` sends it.
    socket.subscribe(SID, { afterSeq: () => 0, onEvent() {}, onDelivery() {}, onInvalid() {}, onReset() {}, onSubscribed });
    sockets[0].readyState = 1;
    sockets[0].onopen!();
    sockets[0].onmessage!({ data: JSON.stringify({ type: 'subscribed', session_id: SID, last_seq: 0 }) });
    expect(onSubscribed).toHaveBeenCalledTimes(1);
    socket.stop();
  });

  it('a refused `subscribe` (an `error` naming the session, no `subscribed`) ends the catch-up', async () => {
    const sockets: { onopen?: () => void; onmessage?: (m: { data: string }) => void }[] = [];
    class FakeWs {
      readyState = 1;
      onopen?: () => void;
      onmessage?: (m: { data: string }) => void;
      constructor() {
        sockets.push(this);
      }
      send() {}
      close() {}
    }
    const socket = new HubSocket({ url: 'ws://x/api/ws', WebSocketImpl: FakeWs as never });
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const onSubscribed = vi.fn();
    socket.start();
    sockets[0].onopen!();
    socket.subscribe(SID, { afterSeq: () => 0, onEvent() {}, onDelivery() {}, onInvalid() {}, onReset() {}, onSubscribed });
    // An error that names no session answers no `subscribe`.
    sockets[0].onmessage!({ data: JSON.stringify({ type: 'error', code: 'invalid_frame', message: 'x' }) });
    expect(onSubscribed).not.toHaveBeenCalled();
    sockets[0].onmessage!({ data: JSON.stringify({ type: 'error', code: 'internal', message: 'internal error', session_id: SID }) });
    expect(onSubscribed).toHaveBeenCalledTimes(1);
    warn.mockRestore();
    socket.stop();
  });

  it('the `subscribed` of a subscription that was left does not end the catch-up of the next one', async () => {
    const sockets: { onopen?: () => void; onmessage?: (m: { data: string }) => void }[] = [];
    class FakeWs {
      readyState = 1;
      onopen?: () => void;
      onmessage?: (m: { data: string }) => void;
      constructor() {
        sockets.push(this);
      }
      send() {}
      close() {}
    }
    const socket = new HubSocket({ url: 'ws://x/api/ws', WebSocketImpl: FakeWs as never });
    socket.start();
    sockets[0].onopen!();
    const { api } = hub(range(1, 10));
    const chat = new ChatSession(api as unknown as Api, socket, SID, noHistoryCache);
    await chat.open();
    await tick();
    chat.close();
    await chat.open();
    await tick();
    const drawn = vi.fn();
    chat.timeline.onChange(drawn);
    const frame = (f: unknown) => sockets[0].onmessage!({ data: JSON.stringify(f) });
    // The hub answers the first `subscribe` after its backfill: not the end of the second's.
    frame({ type: 'subscribed', session_id: SID, last_seq: 10 });
    for (let seq = 11; seq <= 60; seq++) frame({ type: 'event', event: wire(seq) });
    expect(drawn).not.toHaveBeenCalled();
    frame({ type: 'subscribed', session_id: SID, last_seq: 60 });
    expect(drawn).toHaveBeenCalledTimes(1);
    expect(seqs(chat)).toEqual(Array.from({ length: 60 }, (_, i) => i + 1));
    chat.close();
    socket.stop();
  });
});

describe('what the device keeps', () => {
  it('a session deleted from this client is forgotten', async () => {
    const cache = memoryHistoryCache();
    await cache.put(SID, range(1, 5));
    const api = { deleteSession: vi.fn(async () => {}) };
    const app = new AppState(api as unknown as Api, { onState: () => () => {} } as unknown as HubSocket, cache);
    app.sessions.set(SID, info(SID));
    await app.deleteSession(SID);
    // Behind the deletion: it does not wait for the device.
    for (let i = 0; i < 100 && (await cache.range(SID)); i++) await tick(10);
    expect(await cache.range(SID)).toBeUndefined();
  });

  it('the socket gives the subscriber the event as the hub sent it', () => {
    const sockets: { onopen?: () => void; onmessage?: (m: { data: string }) => void; readyState: number }[] = [];
    class FakeWs {
      readyState = 1;
      onopen?: () => void;
      onmessage?: (m: { data: string }) => void;
      constructor() {
        sockets.push(this);
      }
      send() {}
      close() {}
    }
    const socket = new HubSocket({ url: 'ws://x/api/ws', WebSocketImpl: FakeWs as never });
    const got: unknown[] = [];
    socket.subscribe(SID, { afterSeq: () => 0, onEvent: (_ev, raw) => got.push(raw), onDelivery() {}, onInvalid() {}, onReset() {} });
    socket.start();
    const e = { ...wire(5), extra_field_of_a_newer_hub: 1 };
    sockets[0].onmessage!({ data: JSON.stringify({ type: 'event', event: e }) });
    expect(got).toEqual([e]);
    socket.stop();
  });
});

// The feed of a chat: happy-dom lays nothing out, so the feed is made taller than its window.
function info(id: string): SessionInfo {
  return {
    id,
    project_id: 'p1',
    workspace_id: 'w1',
    worker_id: 'wk1',
    cwd: '.',
    title: 'Long one',
    slug: id,
    state: 'idle',
    queue_len: 0,
    unread: 0,
    last_seq: 1,
    last_read_seq: 1,
    archived: false,
    updated_at: new Date().toISOString(),
    system: false,
  };
}

describe('the chat on screen', () => {
  let root: HTMLElement;
  beforeEach(() => {
    root = document.createElement('div');
    document.body.appendChild(root);
    const h = (px: number) =>
      function (this: HTMLElement) {
        return this.classList.contains('timeline') ? px : 0;
      };
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(h(5000));
    vi.spyOn(HTMLElement.prototype, 'clientHeight', 'get').mockImplementation(h(600));
  });
  afterEach(() => {
    render(null, root);
    document.body.innerHTML = '';
    vi.restoreAllMocks();
  });

  function open(events: unknown[], opts: { requests?: { request_id: string; seq: number; kind?: string }[]; room?: boolean } = {}) {
    const { api } = hub(events);
    // What the hub says of the session, also when the chat asks for it again.
    const self: SessionInfo = opts.room ? { ...info(SID), room_mode: 'conversation' } : info(SID);
    const full = {
      ...api,
      events: vi.fn(async () => eventPageOf([], 0)),
      readCursor: vi.fn(async () => {}),
      session: async () => self,
      sessionOrder: async () => ({ mode: 'state', session_ids: [] }),
      workspaces: async () => [],
    };
    const socket = { onState: () => () => {}, subscribe: () => () => {} } as unknown as HubSocket;
    const app = new AppState(full as unknown as Api, socket, noHistoryCache);
    app.me = { id: 'u', email: 'a@b.c', personal_project_id: 'p1', theme: 'warm', look: 'spacious', project_sort: 'custom', new_session_project_id: null };
    app.projects.set('p1', { id: 'p1', name: 'hqc', slug: 'hqc', description: '', is_personal: false, color: 'teal', quick_messages: [], created_at: '2026-10-01T00:00:00Z' });
    app.sessions.set(SID, self);
    app.requests = (opts.requests ?? []).map((r) => ({
      session_id: SID,
      request_id: r.request_id,
      kind: r.kind ?? 'approval',
      address: 'hqc/s1',
      title: 'Long one',
      system: false,
      waiting: [],
      event: {
        ...(eventPageOf([wire(r.seq, { type: 'approval.request', payload: { request_id: r.request_id, tool: 'Bash', description: 'ls', options: [] } })], 0).events[0] as object),
      },
    })) as never;
    return { app, api };
  }

  const mount = async (app: AppState, focusSeq?: number) => {
    await act(async () => {
      render(<Chat app={app} sessionId={SID} focusSeq={focusSeq} />, root);
      await tick(50);
    });
    // The passive effects of the render that showed the feed run with the next render here.
    await act(async () => app.setRequests(app.requests));
  };

  it('loads older history behind the newest page only while the header lacks its cost, down to the usage', async () => {
    const { app, api } = open([...range(1, 249), usage(250), ...range(251, 450)]);
    await mount(app);
    await until(() => app.chat(SID).timeline.olderFrom === 151);
    await tick(30);
    // Not in the newest 100: one more page of 200 holds it, and the rest stays unloaded.
    expect(api.eventsBefore.mock.calls).toEqual([
      [SID, undefined, 100],
      [SID, 351, 200],
    ]);
    expect(app.chat(SID).timeline.usageTotals().cost).toBe(0.5);
    expect(app.chat(SID).timeline.olderFrom).toBe(151);
  });

  it('a usage in the newest page needs no older one', async () => {
    const { app, api } = open([...range(1, 449), usage(450)]);
    await mount(app);
    expect(api.eventsBefore).toHaveBeenCalledTimes(1);
  });

  it('a newest page that fills the window asks for no older one while it is being drawn', async () => {
    // The feed is as tall as its blocks: empty, it is no taller than its window.
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? (this.querySelector('.feed-block') ? 5000 : 600) : 0;
    });
    const { app, api } = open([...range(1, 449), usage(450)]);
    // Rendered outside `act`, which would join the render of the page put into the timeline and
    // the one that draws it into one.
    await act(async () => render(<Chat app={app} sessionId={SID} />, root));
    await tick(50);
    await act(async () => app.setRequests(app.requests));
    await tick(30);
    expect(api.eventsBefore).toHaveBeenCalledTimes(1);
  });

  it('loads older history down to the event of an open request', async () => {
    const { app, api } = open([...range(1, 99), wire(100, { type: 'approval.request', payload: { request_id: 'r1', tool: 'Bash', description: 'ls', options: [] } }), ...range(101, 449), usage(450)], {
      requests: [{ request_id: 'r1', seq: 100 }],
    });
    await mount(app);
    await until(() => app.chat(SID).timeline.olderFrom === 0);
    // One page of 1000 down to the request's event, not pages of 200.
    expect(api.eventsBefore.mock.calls).toEqual([[SID, undefined, 100], [SID, 351, 1000]]);
    expect(app.chat(SID).timeline.olderFrom).toBe(0);
  });

  it('scrolling to the top asks for one older page, and its blocks show', async () => {
    const { app, api } = open([...range(1, 449), usage(450)]);
    const page = api.eventsBefore.getMockImplementation()!;
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    api.eventsBefore.mockImplementation(async (...a: Parameters<typeof page>) => (a[1] === 351 && (await gate), page(...a)));
    await mount(app);
    await until(() => !!root.querySelector('.feed-block'));
    await tick(30);
    const scroller = root.querySelector<HTMLElement>('.timeline')!;
    const first = () => Number(root.querySelector<HTMLElement>('.feed-block')!.dataset.seq);
    const toTop = () =>
      act(async () => {
        scroller.scrollTop = 100;
        scroller.dispatchEvent(new Event('scroll'));
      });
    expect(first()).toBeGreaterThanOrEqual(351);
    // First the blocks of the newest page that are not rendered yet; no request for them.
    await toTop();
    expect(first()).toBe(351);
    expect(api.eventsBefore).toHaveBeenCalledTimes(1);
    // Then the top of what is loaded: an older page.
    await toTop();
    expect(root.querySelector('.feed > .feed-older > .loading')!.textContent).toBe('Loading…');
    release();
    // The older blocks render above, as `showMore` does.
    await until(() => first() < 351);
    expect(api.eventsBefore.mock.calls.slice(1)).toEqual([[SID, 351, 200]]);
    expect(root.querySelector('.feed > .feed-older > .loading')).toBeNull();
    expect(app.chat(SID).timeline.olderFrom).toBe(151);
  });

  it('an older page that joins the top block to the calls before it keeps that block on screen', async () => {
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? 5000 : 0;
    });
    // Calls 341..360 are one group; the newest page starts inside it, at 351.
    const calls = Array.from({ length: 20 }, (_, i) => {
      const seq = 341 + i;
      const call_id = `toolu_${seq - (i % 2)}`;
      return i % 2
        ? wire(seq, { type: 'tool.result', payload: { call_id, result_preview: 'ok', error: false } })
        : wire(seq, { type: 'tool.call', payload: { call_id, tool: 'Bash', args_preview: '{"command":"ls"}' } });
    });
    const { app, api } = open([...range(1, 340), ...calls, ...range(361, 449), usage(450)]);
    const page = api.eventsBefore.getMockImplementation()!;
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    api.eventsBefore.mockImplementation(async (...a: Parameters<typeof page>) => (a[1] === 351 && (await gate), page(...a)));
    await mount(app);
    await until(() => !!root.querySelector('.feed-block'));
    await tick(30);
    const scroller = root.querySelector<HTMLElement>('.timeline')!;
    const first = () => Number(root.querySelector<HTMLElement>('.feed-block[data-seq]')!.dataset.seq);
    const scrollTo = (top: number) =>
      act(async () => {
        scroller.scrollTop = top;
        scroller.dispatchEvent(new Event('scroll'));
      });
    await scrollTo(100);
    expect(first()).toBe(351);
    await scrollTo(100);
    expect(api.eventsBefore.mock.calls.slice(1)).toEqual([[SID, 351, 200]]);
    // Away from the top when the page lands: no more blocks show, the same ones stay.
    await scrollTo(1000);
    release();
    await until(() => app.chat(SID).timeline.olderFrom === 151);
    await tick(30);
    expect(first()).toBe(341);
  });

  it('the user at the top, an older page that adds no block asks for the next one', async () => {
    vi.spyOn(HTMLElement.prototype, 'scrollHeight', 'get').mockImplementation(function (this: HTMLElement) {
      return this.classList.contains('timeline') ? 5000 : 0;
    });
    // Calls 291..350 are one group; the newest page starts at 351, in the group's own run, so the
    // page 151..350 is all calls: its events join the top group and no block is added.
    const calls = Array.from({ length: 200 }, (_, i) => {
      const seq = 151 + i;
      const call_id = `toolu_${seq - (i % 2)}`;
      return i % 2
        ? wire(seq, { type: 'tool.result', payload: { call_id, result_preview: 'ok', error: false } })
        : wire(seq, { type: 'tool.call', payload: { call_id, tool: 'Bash', args_preview: '{"command":"ls"}' } });
    });
    const tail = Array.from({ length: 10 }, (_, i) => {
      const seq = 351 + i;
      const call_id = `toolu_${seq - (i % 2)}`;
      return i % 2
        ? wire(seq, { type: 'tool.result', payload: { call_id, result_preview: 'ok', error: false } })
        : wire(seq, { type: 'tool.call', payload: { call_id, tool: 'Bash', args_preview: '{"command":"ls"}' } });
    });
    const { app, api } = open([...range(1, 150), ...calls, ...tail, ...range(361, 449), usage(450)]);
    await mount(app);
    await until(() => !!root.querySelector('.feed-block'));
    await tick(30);
    const scroller = root.querySelector<HTMLElement>('.timeline')!;
    const scrollTo = (top: number) =>
      act(async () => {
        scroller.scrollTop = top;
        scroller.dispatchEvent(new Event('scroll'));
      });
    // Up from the bottom: the feed stops following it.
    await scrollTo(4000);
    await scrollTo(100);
    // The blocks of the newest page not rendered yet first, then the top of what is loaded.
    await scrollTo(100);
    // The scroll asked for the page 151..350; with no block in it, nothing scrolls again, and the
    // next page is asked for all the same.
    await until(() => api.eventsBefore.mock.calls.length >= 3);
    expect(api.eventsBefore.mock.calls.slice(1)).toEqual([
      [SID, 351, 200],
      [SID, 151, 200],
    ]);
  });

  it('a search hit in the older history loads the pages down to it', async () => {
    const { app, api } = open([...range(1, 449), usage(450)]);
    await mount(app, 320);
    await until(() => !!root.querySelector('.feed-block.is-found'));
    expect(api.eventsBefore.mock.calls.slice(1)).toEqual([[SID, 351, 1000]]);
    expect(root.querySelector<HTMLElement>('.feed-block.is-found')!.dataset.seq).toBe('320');
  });
  it('a request of any kind has its event loaded: a worker approval too', async () => {
    const { app, api } = open([...range(1, 99), wire(100, { type: 'approval.request', payload: { request_id: 'r1', tool: 'Bash', description: 'ls', options: [] } }), ...range(101, 449), usage(450)], {
      requests: [{ request_id: 'r1', seq: 100, kind: 'worker_approval' }],
    });
    await mount(app);
    await until(() => app.chat(SID).timeline.olderFrom === 0);
    expect(api.eventsBefore.mock.calls.map((c) => c[1])).toEqual([undefined, 351]);
  });

  it('a room has no usage of its own: no older page is asked for it', async () => {
    const { app, api } = open(range(1, 450), { room: true });
    await mount(app);
    await tick(100);
    expect(api.eventsBefore.mock.calls).toEqual([[SID, undefined, 100]]);
  });

  it('a hub that fails the older pages is not asked again and again', async () => {
    const { app, api } = open(range(1, 450));
    const page = api.eventsBefore.getMockImplementation()!;
    api.eventsBefore.mockImplementation(async (...a: Parameters<typeof page>) => {
      if (a[1] !== undefined) throw new Error('hub down');
      return page(...a);
    });
    await mount(app);
    // Renders keep coming (the passive effects of one run with the next): an effect that asked
    // again after each failure would ask on each of them.
    for (let i = 0; i < 20; i++) await act(async () => {
        app.setRequests(app.requests);
        await tick(20);
      });
    // The first try, and at most one more that was already under way.
    expect(api.eventsBefore.mock.calls.filter((c) => c[1] !== undefined).length).toBeLessThanOrEqual(2);
    // The user's own scroll to the top tries again.
    const before = api.eventsBefore.mock.calls.length;
    const scroller = root.querySelector<HTMLElement>('.timeline')!;
    const toTop = () =>
      act(async () => {
        scroller.scrollTop = 100;
        scroller.dispatchEvent(new Event('scroll'));
      });
    await toTop();
    await toTop();
    await act(async () => void (await tick(20)));
    expect(api.eventsBefore.mock.calls.length).toBeGreaterThan(before);
  });

  it('a render the timeline did not cause builds no feed again, nor one of the answer text being written', async () => {
    const { app } = open([...range(1, 449), usage(450)]);
    await mount(app);
    const t = app.chat(SID).timeline;
    // The feed is built by the time the spy is set: it counts only what comes after.
    await until(() => !!root.querySelector('.feed-block'));
    const items = vi.spyOn(t, 'items');
    await act(async () => app.setRequests(app.requests));
    await act(async () => app.setRequests(app.requests));
    expect(items).not.toHaveBeenCalled();
    const delta = (text: string) =>
      parseEvent({ ...wire(0), seq: undefined, type: 'agent.delta', payload: { text } });
    for (const text of ['one ', 'two ', 'three'])
      await act(async () => t.apply(delta(text)));
    expect(t.streaming).toBe('one two three');
    expect(items).not.toHaveBeenCalled();
    await act(async () => t.apply(parsed(451)));
    expect(items).toHaveBeenCalled();
  });

  describe('a click on a quote of the owner\'s message', () => {
    const owner = (seq: number, text: string) => wire(seq, { type: 'user.message', payload: { text, artifact_ids: [] }, author: { kind: 'user', id: 'u' } });
    const agent = (seq: number, text: string) => wire(seq, { payload: { text, final: true } });
    const click = (el: Element) => act(async () => void el.dispatchEvent(new MouseEvent('click', { bubbles: true })));
    const quoteOf = (n = 0) => root.querySelectorAll('.msg-user .msg-quote')[n]!;
    /** The custom highlights the page makes: the ranges set under each name, and their removals. */
    function stubHighlights() {
      const set: Record<string, Range[]> = {};
      const removed: string[] = [];
      vi.stubGlobal('CSS', { highlights: { set: (n: string, h: { ranges: Range[] }) => (set[n] = h.ranges), delete: (n: string) => void removed.push(n) } });
      vi.stubGlobal('Highlight', class { ranges: Range[]; constructor(...r: Range[]) { this.ranges = r; } });
      return { set, removed };
    }
    afterEach(() => vi.unstubAllGlobals());

    const SOURCE = 'Intro line.\n\nSome **bold** words and a [link](https://x.test/a) in text. Tail.';

    it('highlights exactly the quoted fragment of its source and lets go of the bottom', async () => {
      const hl = stubHighlights();
      Element.prototype.scrollIntoView = () => {};
      const { app } = open([agent(1, SOURCE), owner(2, '> Some bold words and a link in text\n\nwhy?')]);
      await mount(app);
      await until(() => !!root.querySelector('.feed-block'));
      const scroller = root.querySelector<HTMLElement>('.timeline')!;
      await click(quoteOf());
      expect(hl.set['quote-jump']).toHaveLength(1);
      expect(hl.set['quote-jump'][0].toString()).toBe('Some bold words and a link in text');
      // Not pulled back to the bottom by a later render.
      scroller.scrollTop = 100;
      await act(async () => app.chat(SID).timeline.changed());
      expect(scroller.scrollTop).toBe(100);
    });

    it('marks the block of its source when the browser has no custom highlights', async () => {
      const scrolled: string[] = [];
      Element.prototype.scrollIntoView = function (this: Element) {
        scrolled.push((this as HTMLElement).dataset.seq ?? '');
      } as Element['scrollIntoView'];
      const { app } = open([agent(1, SOURCE), owner(2, '> Some bold words and a link in text\n\nwhy?')]);
      await mount(app);
      await until(() => !!root.querySelector('.feed-block'));
      await click(quoteOf());
      expect(root.querySelector<HTMLElement>('.feed-block.is-found')!.dataset.seq).toBe('1');
      expect(scrolled).toEqual(['1']);
    });

    it('a second click on the same quote jumps again', async () => {
      const hl = stubHighlights();
      Element.prototype.scrollIntoView = () => {};
      const { app } = open([agent(1, SOURCE), owner(2, '> Some bold words and a link in text\n\nwhy?')]);
      await mount(app);
      await until(() => !!root.querySelector('.feed-block'));
      await click(quoteOf());
      delete hl.set['quote-jump'];
      await click(quoteOf());
      expect(hl.set['quote-jump']).toHaveLength(1);
    });

    it('loads older pages until the source is found, then jumps', async () => {
      const hl = stubHighlights();
      Element.prototype.scrollIntoView = () => {};
      const { app, api } = open([...range(1, 19), agent(20, SOURCE), ...range(21, 448), owner(449, '> words and a link in text\n\nwhy?'), usage(450)]);
      await mount(app);
      await until(() => !!quoteOf());
      expect(app.chat(SID).timeline.olderFrom).toBeGreaterThan(20);
      await click(quoteOf());
      await until(() => !!hl.set['quote-jump']);
      expect(app.chat(SID).timeline.olderFrom).toBeLessThanOrEqual(20);
      expect(api.eventsBefore).toHaveBeenCalled();
      expect(hl.set['quote-jump'][0].toString()).toBe('words and a link in text');
    });

    it('an earlier message that quoted the same text is not the source', async () => {
      const hl = stubHighlights();
      Element.prototype.scrollIntoView = () => {};
      const { app } = open([agent(1, SOURCE), owner(2, '> words and a link in text\n\nfirst'), agent(3, 'ok'), owner(4, '> words and a link in text\n\nsecond')]);
      await mount(app);
      await until(() => root.querySelectorAll('.msg-user .msg-quote').length === 2);
      await click(quoteOf(1));
      expect(hl.set['quote-jump']).toHaveLength(1);
      // In the agent's message, not in the first quote.
      expect((hl.set['quote-jump'][0].startContainer.parentElement as HTMLElement).closest('.msg-user')).toBeNull();
    });

    const approvalOf = (seq: number) =>
      wire(seq, { type: 'approval.request', payload: { request_id: 'r1', tool: 'Bash', description: 'Run Bash', options: [] } });
    const callOf = (seq: number) =>
      wire(seq, { type: 'tool.call', payload: { call_id: 'c1', tool: 'Bash', args_preview: JSON.stringify({ command: 'echo quotedcommandtext' }) } });

    it('marks the card of a request when the quoted command is its call, not the block before it', async () => {
      Element.prototype.scrollIntoView = () => {};
      const { app } = open([agent(1, 'something else'), callOf(2), approvalOf(3), owner(4, '> echo quotedcommandtext\n\nwhy?')]);
      await mount(app);
      await until(() => !!quoteOf());
      expect([...root.querySelectorAll<HTMLElement>('.feed-block')].map((b) => b.dataset.seq)).toEqual(['1', '3', '4']);
      await click(quoteOf());
      expect(root.querySelector<HTMLElement>('.feed-block.is-found')!.dataset.seq).toBe('3');
    });

    it('marks the block of a side question when the quoted text is its answer', async () => {
      Element.prototype.scrollIntoView = () => {};
      const question = wire(2, { type: 'side.question', payload: { text: 'a side question?' } });
      const answer = wire(4, { type: 'side.answer', payload: { question: eventId(2), text: 'uniquesideanswer words here' } });
      const { app } = open([agent(1, 'first'), question, agent(3, 'later'), answer, owner(5, '> uniquesideanswer words here\n\nwhy?')]);
      await mount(app);
      await until(() => !!quoteOf());
      await click(quoteOf());
      expect(root.querySelector<HTMLElement>('.feed-block.is-found')!.dataset.seq).toBe('2');
    });

    it('counts a jump to an event no block draws as done, without growing the feed again', async () => {
      Element.prototype.scrollIntoView = () => {};
      // A status without an error is hidden: no block draws it.
      const hidden = wire(2, { type: 'status', payload: { state: 'idle' } });
      const { app } = open([agent(1, 'first'), hidden, agent(3, 'third')]);
      await mount(app, 2);
      await act(async () => app.chat(SID).timeline.changed());
      await act(async () => app.chat(SID).timeline.changed());
      expect(root.querySelector<HTMLElement>('.feed-block.is-found')!.dataset.seq).toBe('1');
    });

    it("highlights the earlier message's own text when that message quoted it too", async () => {
      const hl = stubHighlights();
      Element.prototype.scrollIntoView = () => {};
      const { app } = open([
        owner(1, '> some quoted words here\n\nand my own some quoted words here again'),
        agent(2, 'ok'),
        owner(3, '> some quoted words here\n\nsecond'),
      ]);
      await mount(app);
      await until(() => root.querySelectorAll('.msg-user .msg-quote').length === 2);
      await click(quoteOf(1));
      expect(hl.set['quote-jump']).toHaveLength(1);
      const range = hl.set['quote-jump'][0];
      expect(range.toString()).toBe('some quoted words here');
      expect((range.startContainer.parentElement as HTMLElement).closest('.msg-quote')).toBeNull();
      expect((range.startContainer.parentElement as HTMLElement).closest('.feed-block')!.getAttribute('data-seq')).toBe('1');
    });

    it('the quote is a button of the keyboard: Enter on it jumps', async () => {
      const hl = stubHighlights();
      Element.prototype.scrollIntoView = () => {};
      const { app } = open([agent(1, SOURCE), owner(2, '> Some bold words and a link in text\n\nwhy?')]);
      await mount(app);
      await until(() => !!quoteOf());
      expect(quoteOf().getAttribute('role')).toBe('button');
      expect(quoteOf().getAttribute('tabindex')).toBe('0');
      await act(async () => void quoteOf().dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })));
      expect(hl.set['quote-jump'][0].toString()).toBe('Some bold words and a link in text');
    });

    it('goes on to the second older page when the source is on it, in a history of over 1100 events', async () => {
      const hl = stubHighlights();
      Element.prototype.scrollIntoView = () => {};
      const { app, api } = open([...range(1, 19), agent(20, SOURCE), ...range(21, 1148), usage(1149), owner(1150, '> words and a link in text\n\nwhy?')]);
      await mount(app);
      await until(() => !!quoteOf());
      const before = api.eventsBefore.mock.calls.length;
      await click(quoteOf());
      await until(() => !!hl.set['quote-jump']);
      // The first older page (1000 events) does not hold it; the second does: two more calls.
      expect(api.eventsBefore.mock.calls.length - before).toBeGreaterThanOrEqual(2);
      expect(app.chat(SID).timeline.olderFrom).toBeLessThanOrEqual(20);
      expect(hl.set['quote-jump'][0].toString()).toBe('words and a link in text');
    });

    it('says so when an older page does not load', async () => {
      stubHighlights();
      const { app, api } = open([...range(1, 449), usage(450), owner(451, '> words that sit far back\n\nwhy?')]);
      await mount(app);
      await until(() => !!quoteOf());
      api.eventsBefore.mockImplementation(async () => {
        throw new Error('offline');
      });
      await click(quoteOf());
      await until(() => !!root.querySelector('.notice'));
      expect(root.querySelector('.notice')!.textContent).toBe('Older history did not load');
    });

    it('the range of a quote after an emoji and a ligature is exactly the quoted text', async () => {
      const hl = stubHighlights();
      Element.prototype.scrollIntoView = () => {};
      const { app } = open([agent(1, '🙂 The ﬁnal 🎉 plan: ﬁrst, the words here and there'), owner(2, '> the words here\n\nwhy?')]);
      await mount(app);
      await until(() => !!quoteOf());
      await click(quoteOf());
      expect(hl.set['quote-jump'][0].toString()).toBe('the words here');
    });

    it('says so when the text is nowhere in the chat', async () => {
      stubHighlights();
      const { app } = open([agent(1, 'something else'), owner(2, '> words that were never said\n\nwhy?')]);
      await mount(app);
      await until(() => !!quoteOf());
      await click(quoteOf());
      expect(root.querySelector('.notice')!.textContent).toBe('The quoted text is not in this chat');
    });

    it('a click that ends a selection does not jump', async () => {
      const hl = stubHighlights();
      Element.prototype.scrollIntoView = () => {};
      const { app } = open([agent(1, SOURCE), owner(2, '> Some bold words and a link in text\n\nwhy?')]);
      await mount(app);
      await until(() => !!quoteOf());
      const q = quoteOf();
      const r = document.createRange();
      r.selectNodeContents(q);
      document.getSelection()!.removeAllRanges();
      document.getSelection()!.addRange(r);
      await click(q);
      expect(hl.set['quote-jump']).toBeUndefined();
      expect(root.querySelector('.feed-block.is-found')).toBeNull();
      document.getSelection()!.removeAllRanges();
    });

    it('finds a quote of an answer stored after the message that quotes it', async () => {
      const hl = stubHighlights();
      Element.prototype.scrollIntoView = () => {};
      // Quoted while the answer was being written: its message came in after the quote's.
      const { app } = open([agent(1, 'something else'), owner(2, '> words and a link in text\n\nwhy?'), agent(3, SOURCE)]);
      await mount(app);
      await until(() => !!quoteOf());
      await click(quoteOf());
      expect(hl.set['quote-jump']).toHaveLength(1);
      expect((hl.set['quote-jump'][0].startContainer.parentElement as HTMLElement).closest('.feed-block')!.getAttribute('data-seq')).toBe('3');
    });
  });
});
