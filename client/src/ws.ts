// WebSocket /api/ws (SPEC §6): `subscribe {session_id, after_seq}` per open chat,
// `subscribe_summary {}` for the session list. On every (re)connect the client repeats
// each subscription from the last `seq` it has seen.

import { parseServerFrame, type ClientFrame, type HqcEvent, type SessionSummary } from './proto';

export type ConnState = 'connecting' | 'open' | 'closed';

export interface SessionSubscriber {
  /** Last `seq` applied; read at each (re)subscribe. */
  afterSeq(): number;
  onEvent(ev: HqcEvent): void;
  onInvalid(raw: unknown, error: string): void;
  /** The socket dropped: transport-only state (agent.delta) is gone. */
  onReset(): void;
}

type WsCtor = new (url: string) => WebSocket;

export interface HubSocketOptions {
  url: string;
  WebSocketImpl?: WsCtor;
  minDelayMs?: number;
  maxDelayMs?: number;
}

export class HubSocket {
  private ws: WebSocket | null = null;
  private readonly subs = new Map<string, SessionSubscriber>();
  private readonly summaryListeners = new Set<(s: SessionSummary[]) => void>();
  private readonly stateListeners = new Set<(s: ConnState) => void>();
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
    this.send({ type: 'subscribe', payload: { session_id: sessionId, after_seq: sub.afterSeq() } });
    return () => {
      if (this.subs.get(sessionId) !== sub) return;
      this.subs.delete(sessionId);
      this.send({ type: 'unsubscribe', payload: { session_id: sessionId } });
    };
  }

  onSummary(fn: (s: SessionSummary[]) => void): () => void {
    this.summaryListeners.add(fn);
    if (this.summaryListeners.size === 1) this.send({ type: 'subscribe_summary', payload: {} });
    return () => this.summaryListeners.delete(fn);
  }

  onState(fn: (s: ConnState) => void): () => void {
    this.stateListeners.add(fn);
    return () => this.stateListeners.delete(fn);
  }

  private send(frame: ClientFrame): void {
    if (this.ws?.readyState === 1) this.ws.send(JSON.stringify(frame));
  }

  private setState(s: ConnState): void {
    if (this.state === s) return;
    this.state = s;
    for (const fn of this.stateListeners) fn(s);
  }

  private open(): void {
    const Ctor = this.opts.WebSocketImpl ?? WebSocket;
    this.setState('connecting');
    const ws = new Ctor(this.opts.url);
    this.ws = ws;
    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.attempt = 0;
      this.setState('open');
      if (this.summaryListeners.size) this.send({ type: 'subscribe_summary', payload: {} });
      for (const [id, sub] of this.subs) {
        this.send({ type: 'subscribe', payload: { session_id: id, after_seq: sub.afterSeq() } });
      }
    };
    ws.onmessage = (m) => {
      if (this.ws !== ws) return;
      this.dispatch(m.data);
    };
    ws.onclose = () => {
      if (this.ws !== ws) return;
      this.ws = null;
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
        this.subs.get(frame.event.session_id)?.onEvent(frame.event);
        break;
      case 'event.invalid': {
        const sid = (frame.raw as { session_id?: unknown })?.session_id;
        if (typeof sid === 'string') this.subs.get(sid)?.onInvalid(frame.raw, frame.error);
        break;
      }
      case 'summary':
        for (const fn of this.summaryListeners) fn(frame.sessions);
        break;
      case 'error':
        console.warn('hub ws error', frame.code, frame.message);
        break;
    }
  }
}

export function hubSocketUrl(loc: Location = location): string {
  return `${loc.protocol === 'https:' ? 'wss' : 'ws'}://${loc.host}/api/ws`;
}
