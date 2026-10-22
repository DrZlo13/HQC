// What one chat shows: stored events by `seq`, the live `agent.delta` stream, optimistic
// messages keyed by `client_op_id`, and the fate of approval/clarify requests.
// Pure logic, no DOM and no network.

import type {
  ApprovalResponse,
  ClarifyResponse,
  EventOf,
  HqcEvent,
  RequestCancel,
} from './proto';

export interface PendingMessage {
  client_op_id: string;
  text: string;
  /** `sending` until the hub echoes it back; `failed` if the POST failed. */
  status: 'sending' | 'failed';
  error?: string;
}

export interface InvalidEvent {
  key: string;
  seq?: number;
  error: string;
  raw: unknown;
}

export type Item =
  | { kind: 'event'; key: string; event: HqcEvent }
  | { kind: 'invalid'; key: string; invalid: InvalidEvent }
  | { kind: 'pending'; key: string; pending: PendingMessage };

export interface RequestFate {
  response?: ApprovalResponse | ClarifyResponse;
  cancel?: RequestCancel;
}

export class Timeline {
  private readonly bySeq = new Map<number, HqcEvent>();
  private readonly invalid: InvalidEvent[] = [];
  private readonly pending = new Map<string, PendingMessage>();
  private readonly echoed = new Set<string>();
  private readonly fates = new Map<string, RequestFate>();
  private readonly results = new Map<string, EventOf<'tool.result'>>();
  private readonly calls = new Set<string>();
  private readonly requests = new Map<string, EventOf<'approval.request' | 'clarify.request'>>();
  private readonly listeners = new Set<() => void>();
  private sorted: HqcEvent[] | null = [];
  lastSeq = 0;
  /** Text of `agent.delta` since the last `agent.message`. */
  streaming = '';
  version = 0;

  onChange(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private changed(): void {
    this.version++;
    for (const fn of this.listeners) fn();
  }

  apply(ev: HqcEvent): void {
    this.applyQuiet(ev);
    this.changed();
  }

  applyAll(evs: HqcEvent[]): void {
    for (const ev of evs) this.applyQuiet(ev);
    this.changed();
  }

  private applyQuiet(ev: HqcEvent): void {
    if (ev.type === 'agent.delta') {
      this.streaming += ev.payload.text;
      return;
    }
    // Clients see only stored events (SPEC §5.2); anything else without seq is dropped.
    if (ev.seq === undefined) return;
    const known = this.bySeq.get(ev.seq);
    // The same seq again replaces the stored copy (e.g. a later `delivery`); an older copy
    // never overrides a newer one.
    if (known && known.event_id !== ev.event_id) return;
    this.bySeq.set(ev.seq, ev);
    this.sorted = null;
    this.lastSeq = Math.max(this.lastSeq, ev.seq);

    switch (ev.type) {
      case 'user.message':
        if (ev.client_op_id) {
          this.pending.delete(ev.client_op_id);
          this.echoed.add(ev.client_op_id);
        }
        break;
      case 'agent.message':
        this.streaming = '';
        break;
      case 'status':
        if (ev.payload.state !== 'running') this.streaming = '';
        break;
      case 'tool.call':
        this.calls.add(ev.payload.call_id);
        break;
      case 'tool.result':
        this.results.set(ev.payload.call_id, ev);
        break;
      case 'approval.request':
      case 'clarify.request':
        this.requests.set(ev.payload.request_id, ev);
        break;
      case 'approval.response':
      case 'clarify.response':
        this.fate(ev.payload.request_id).response = ev.payload;
        break;
      case 'approval.cancel':
      case 'clarify.cancel':
        this.fate(ev.payload.request_id).cancel = ev.payload;
        break;
    }
  }

  addInvalid(raw: unknown, error: string): void {
    const seq = (raw as { seq?: unknown })?.seq;
    const s = typeof seq === 'number' ? seq : undefined;
    if (s !== undefined && this.invalid.some((i) => i.seq === s)) return;
    if (s !== undefined) this.lastSeq = Math.max(this.lastSeq, s);
    this.invalid.push({ key: `inv-${s ?? this.invalid.length}-${this.version}`, seq: s, error, raw });
    this.changed();
  }

  private fate(requestId: string): RequestFate {
    let f = this.fates.get(requestId);
    if (!f) this.fates.set(requestId, (f = {}));
    return f;
  }

  requestFate(requestId: string): RequestFate {
    return this.fates.get(requestId) ?? {};
  }

  request(requestId: string): EventOf<'approval.request' | 'clarify.request'> | undefined {
    return this.requests.get(requestId);
  }

  resultFor(callId: string): EventOf<'tool.result'> | undefined {
    return this.results.get(callId);
  }

  /** Results whose call is in the timeline are shown inside the call's card. */
  hasCall(callId: string): boolean {
    return this.calls.has(callId);
  }

  addPending(clientOpId: string, text: string): void {
    if (this.echoed.has(clientOpId)) return;
    this.pending.set(clientOpId, { client_op_id: clientOpId, text, status: 'sending' });
    this.changed();
  }

  failPending(clientOpId: string, error: string): void {
    const p = this.pending.get(clientOpId);
    if (!p) return;
    this.pending.set(clientOpId, { ...p, status: 'failed', error });
    this.changed();
  }

  dropPending(clientOpId: string): PendingMessage | undefined {
    const p = this.pending.get(clientOpId);
    this.pending.delete(clientOpId);
    this.changed();
    return p;
  }

  /** The socket dropped: deltas after the gap would not continue the buffered text. */
  reset(): void {
    if (!this.streaming) return;
    this.streaming = '';
    this.changed();
  }

  events(): HqcEvent[] {
    if (!this.sorted) this.sorted = [...this.bySeq.values()].sort((a, b) => a.seq! - b.seq!);
    return this.sorted;
  }

  items(): Item[] {
    const out: Item[] = this.events().map((e) => ({ kind: 'event', key: `e${e.seq}`, event: e }));
    for (const inv of this.invalid) {
      const item: Item = { kind: 'invalid', key: inv.key, invalid: inv };
      const at = inv.seq === undefined ? -1 : out.findIndex((i) => seqOf(i) > inv.seq!);
      if (at < 0) out.push(item);
      else out.splice(at, 0, item);
    }
    for (const p of this.pending.values()) {
      out.push({ kind: 'pending', key: `p-${p.client_op_id}`, pending: p });
    }
    return out;
  }

  /** True if some user message ended with `delivery: failed` (SPEC §5.2): offer retry. */
  hasFailedDelivery(): boolean {
    return this.events().some((e) => e.type === 'user.message' && e.delivery === 'failed');
  }
}

function seqOf(i: Item): number {
  if (i.kind === 'event') return i.event.seq ?? Infinity;
  if (i.kind === 'invalid') return i.invalid.seq ?? Infinity;
  return Infinity;
}
