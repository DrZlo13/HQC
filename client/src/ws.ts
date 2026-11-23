// WebSocket /api/ws (SPEC §6): `subscribe {session_id, after_seq}` per open chat,
// `subscribe_summary` for the session list and the open requests. Frames are flat JSON (crates/hqc-proto
// client_frame). On every (re)connect the client repeats each subscription from the last
// `seq` it has seen. Where the browser can inflate, the connection asks for `?deflate=raw`: the
// hub then sends binary messages, one raw DEFLATE stream with a sync flush after each frame, each
// frame's JSON followed by "\n" (`Inflater`); a text message is a frame as it is.

import {
  parseServerFrame,
  type ClientFrame,
  type DeliveryChange,
  type HqcEvent,
  type OpenRequest,
  type SessionSummary,
} from './proto';

export type ConnState = 'connecting' | 'open' | 'closed';

export interface SessionSubscriber {
  /** The `seq` to resume after; read at each (re)subscribe. */
  afterSeq(): number;
  /** `raw` is the event object of the frame as the hub sent it, which the device keeps. */
  onEvent(ev: HqcEvent, raw?: unknown): void;
  /** `delivery` of a stored input changed (the event itself is not sent again). */
  onDelivery(d: DeliveryChange): void;
  onInvalid(raw: unknown, error: string): void;
  /** The socket dropped: transport-only state (agent.delta, subagents.live, schedules.live)
   *  is gone. */
  onReset(): void;
  /** The hub has sent the history after `afterSeq` (frame `subscribed`, sent after the backfill,
   *  the live text, `subagents.live` and `schedules.live`); the live stream follows. */
  onSubscribed?(): void;
}

type WsCtor = new (url: string) => WebSocket;

export interface HubSocketOptions {
  url: string;
  WebSocketImpl?: WsCtor;
  minDelayMs?: number;
  maxDelayMs?: number;
}

/** Whether this browser inflates raw DEFLATE (`DecompressionStream`). */
function canInflate(): boolean {
  try {
    new DecompressionStream('deflate-raw');
    return true;
  } catch {
    return false;
  }
}

/** One connection's compressed stream: the binary messages go in in order, each complete line
 *  of the inflated text comes out through `onLine`. A broken stream calls `onError`; after
 *  `close` nothing comes out. */
class Inflater {
  private readonly writer: WritableStreamDefaultWriter<BufferSource>;
  private readonly reader: ReadableStreamDefaultReader<string>;
  private closed = false;

  constructor(
    private readonly onLine: (line: string) => void,
    private readonly onError: () => void,
  ) {
    const inflate = new DecompressionStream('deflate-raw');
    this.writer = inflate.writable.getWriter();
    this.reader = inflate.readable.pipeThrough(new TextDecoderStream()).getReader();
    void this.read().catch(() => this.fail());
  }

  write(data: ArrayBuffer): void {
    this.writer.write(new Uint8Array(data)).catch(() => this.fail());
  }

  close(): void {
    this.closed = true;
    this.writer.abort().catch(() => {});
    this.reader.cancel().catch(() => {});
  }

  private fail(): void {
    if (!this.closed) this.onError();
  }

  private async read(): Promise<void> {
    let rest = '';
    for (;;) {
      const { done, value } = await this.reader.read();
      if (done) return;
      rest += value;
      for (let i = rest.indexOf('\n'); i >= 0; i = rest.indexOf('\n')) {
        const line = rest.slice(0, i);
        rest = rest.slice(i + 1);
        if (this.closed) return;
        this.onLine(line);
      }
    }
  }
}

export class HubSocket {
  private ws: WebSocket | null = null;
  private inflater: Inflater | null = null;
  private readonly subs = new Map<string, SessionSubscriber>();
  private readonly summaryListeners = new Set<(s: SessionSummary[], whole: boolean) => void>();
  private readonly requestListeners = new Set<(r: OpenRequest[]) => void>();
  private readonly stateListeners = new Set<(s: ConnState) => void>();
  /** Per session, the `subscribe` frames sent on this socket whose `subscribed` has not come:
   *  the hub answers each, also one of a subscription that was left meanwhile, so an old
   *  `subscribed` must not end the catch-up of the new one. */
  private readonly unanswered = new Map<string, number>();
  private attempt = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private stopped = true;
  state: ConnState = 'closed';

  constructor(private readonly opts: HubSocketOptions) {}

  start(): void {
    if (!this.stopped) return;
    this.stopped = false;
    this.open();
  }

  stop(): void {
    this.stopped = true;
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    this.ws?.close();
    this.ws = null;
    this.unanswered.clear();
    this.inflater?.close();
    this.inflater = null;
    this.setState('closed');
  }

  /** Reconnect now if the socket is down (tab became visible, network came back). */
  nudge(): void {
    if (this.stopped || this.state === 'open' || this.state === 'connecting') return;
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
    this.attempt = 0;
    this.open();
  }

  subscribe(sessionId: string, sub: SessionSubscriber): () => void {
    this.subs.set(sessionId, sub);
    this.sendSubscribe(sessionId, sub);
    return () => {
      if (this.subs.get(sessionId) !== sub) return;
      this.subs.delete(sessionId);
      this.send({ type: 'unsubscribe', session_id: sessionId });
    };
  }

  /** `summary` (every session, `whole`) and `session.summary` (one changed) both come as a list. */
  onSummary(fn: (s: SessionSummary[], whole: boolean) => void): () => void {
    this.summaryListeners.add(fn);
    if (this.summaryListeners.size === 1) this.send({ type: 'subscribe_summary' });
    return () => this.summaryListeners.delete(fn);
  }

  /** The user's open requests, whole each time (T37); they come with the summary
   *  subscription, so a listener also needs `onSummary`. */
  onRequests(fn: (r: OpenRequest[]) => void): () => void {
    this.requestListeners.add(fn);
    return () => this.requestListeners.delete(fn);
  }

  onState(fn: (s: ConnState) => void): () => void {
    this.stateListeners.add(fn);
    return () => this.stateListeners.delete(fn);
  }

  private send(frame: ClientFrame): boolean {
    if (this.ws?.readyState !== 1) return false;
    this.ws.send(JSON.stringify(frame));
    return true;
  }

  private sendSubscribe(id: string, sub: SessionSubscriber): void {
    if (this.send({ type: 'subscribe', session_id: id, after_seq: sub.afterSeq() }))
      this.unanswered.set(id, (this.unanswered.get(id) ?? 0) + 1);
  }

  private setState(s: ConnState): void {
    if (this.state === s) return;
    this.state = s;
    for (const fn of this.stateListeners) fn(s);
  }

  private open(): void {
    const Ctor = this.opts.WebSocketImpl ?? WebSocket;
    this.setState('connecting');
    const inflate = canInflate();
    const url = inflate ? `${this.opts.url}${this.opts.url.includes('?') ? '&' : '?'}deflate=raw` : this.opts.url;
    const ws = new Ctor(url);
    this.ws = ws;
    this.unanswered.clear();
    if (inflate) ws.binaryType = 'arraybuffer';
    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.attempt = 0;
      this.setState('open');
      if (this.summaryListeners.size) this.send({ type: 'subscribe_summary' });
      for (const [id, sub] of this.subs) this.sendSubscribe(id, sub);
    };
    ws.onmessage = (m) => {
      if (this.ws !== ws) return;
      if (typeof m.data === 'string') {
        this.dispatch(m.data);
        return;
      }
      // A frame comes out of the stream a moment later; `close` of the inflater, on every path
      // that replaces the socket, stops the ones still on their way.
      this.inflater ??= new Inflater((line) => this.dispatch(line), () => ws.close());
      this.inflater.write(m.data as ArrayBuffer);
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.unanswered.clear();
      this.inflater?.close();
      this.inflater = null;
      for (const sub of this.subs.values()) sub.onReset();
      this.setState('closed');
      this.schedule();
    };
    ws.onerror = () => ws.close();
  }

  private schedule(): void {
    if (this.stopped) return;
    const min = this.opts.minDelayMs ?? 500;
    const max = this.opts.maxDelayMs ?? 15_000;
    const delay = Math.min(max, min * 2 ** this.attempt) * (0.75 + Math.random() * 0.5);
    this.attempt++;
    this.timer = setTimeout(() => {
      this.timer = null;
      this.open();
    }, delay);
  }

  private dispatch(data: unknown): void {
    let json: unknown;
    try {
      json = JSON.parse(String(data));
    } catch {
      return;
    }
    let frame;
    try {
      frame = parseServerFrame(json);
    } catch {
      return;
    }
    switch (frame.type) {
      case 'event':
        this.subs.get(frame.event.session_id)?.onEvent(frame.event, (json as { event?: unknown }).event);
        break;
      case 'event.invalid': {
        const sid = (frame.raw as { session_id?: unknown })?.session_id;
        if (typeof sid === 'string') this.subs.get(sid)?.onInvalid(frame.raw, frame.error);
        break;
      }
      case 'delivery':
        this.subs.get(frame.session_id)?.onDelivery(frame);
        break;
      case 'summary':
        for (const fn of this.summaryListeners) fn(frame.sessions, true);
        break;
      case 'session.summary':
        for (const fn of this.summaryListeners) fn([frame.session], false);
        break;
      case 'requests':
        for (const fn of this.requestListeners) fn(frame.requests);
        break;
      case 'error':
        console.warn('hub ws error', frame.code, frame.message, frame.session_id ?? '');
        break;
      case 'subscribed': {
        const left = (this.unanswered.get(frame.session_id) ?? 1) - 1;
        if (left > 0) this.unanswered.set(frame.session_id, left);
        else {
          this.unanswered.delete(frame.session_id);
          this.subs.get(frame.session_id)?.onSubscribed?.();
        }
        break;
      }
      // `pong` and unknown frames need no action.
    }
  }
}

export function hubSocketUrl(loc: Location = location): string {
  return `${loc.protocol === 'https:' ? 'wss' : 'ws'}://${loc.host}/api/ws`;
}
