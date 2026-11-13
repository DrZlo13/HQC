// What one chat shows: stored events by `seq`, the live `agent.delta` stream and
// `subagents.live` list, optimistic messages keyed by `client_op_id`, and the fate of
// approval/clarify requests.
// Pure logic, no DOM and no network.

import type {
  ApprovalResponse,
  ClarifyResponse,
  DeliveryChange,
  EventOf,
  HqcEvent,
  LiveSubagent,
  RequestCancel,
} from "./proto";

/** A file of a message not yet stored: shown by name, images by their local preview. */
export interface PendingAttachment {
  name: string;
  size: number;
  /** `data:` URL of an image (the CSP allows no `blob:`). */
  preview?: string;
}

export interface PendingMessage {
  client_op_id: string;
  text: string;
  attachments?: PendingAttachment[];
  /** `sending` until the hub echoes it back; `failed` if the POST failed. */
  status: "sending" | "failed";
  error?: string;
}

export interface InvalidEvent {
  key: string;
  seq?: number;
  error: string;
  raw: unknown;
}

export type Item =
  | { kind: "event"; key: string; event: HqcEvent }
  | { kind: "invalid"; key: string; invalid: InvalidEvent }
  | { kind: "pending"; key: string; pending: PendingMessage };

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
  private readonly results = new Map<string, EventOf<"tool.result">>();
  private readonly calls = new Map<string, EventOf<"tool.call">>();
  /** The latest `system task_notification` by its `call_id`, and by its `task_id`. */
  private readonly endsByCall = new Map<string, EventOf<"system">>();
  private readonly endsByTask = new Map<string, EventOf<"system">>();
  private readonly requests = new Map<
    string,
    EventOf<"approval.request" | "clarify.request">
  >();
  private readonly listeners = new Set<() => void>();
  private sorted: HqcEvent[] | null = [];
  lastSeq = 0;
  /** Events before this `seq` are not loaded yet (`ChatSession` loads the newest page first);
   *  0 once the history is in. */
  olderFrom = 0;
  /** Text of `agent.delta` since the last `agent.message`. */
  streaming = "";
  /** The subagents of the latest `subagents.live`. */
  liveAgents: LiveSubagent[] = [];
  /** When that list came (`Date.now()`): its `duration_ms` were counted then. */
  liveAgentsAt = 0;
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

  /** The stored input from the reply to `POST /input`. Shown at once, but `lastSeq` stays:
   *  events before it may still be on their way, and `subscribe` resumes from `lastSeq`. */
  applyReply(ev: HqcEvent): void {
    this.applyQuiet(ev, false);
    this.changed();
  }

  private applyQuiet(ev: HqcEvent, advance = true): void {
    if (ev.type === "agent.delta") {
      this.streaming += ev.payload.text;
      return;
    }
    if (ev.type === "subagents.live") {
      this.liveAgents = ev.payload.agents;
      this.liveAgentsAt = Date.now();
      return;
    }
    // Clients see only stored events (SPEC §5.2); anything else without seq is dropped.
    if (ev.seq === undefined) return;
    // Events are append-only: a seq seen before (REST page and subscribe backlog overlap,
    // the POST /input reply and its echo) is the same event. Only an input's `delivery`
    // changes (and with it a message's `payload.joined`), normally by a `delivery` frame
    // (`applyDelivery`); a frame sent while the socket
    // was down is lost, so a copy the hub sent again carries the current one. The reply to
    // `POST /input` (`advance: false`) may come after newer copies and never overrides them.
    // The hub also sends a reply to an ask again once it was delivered (`reply_delivered_at`,
    // SPEC §6), and a later copy keeps a time an earlier one had.
    const seen = this.bySeq.get(ev.seq);
    if (seen) {
      if (advance) {
        this.lastSeq = Math.max(this.lastSeq, ev.seq);
        const replyAt = ev.reply_delivered_at ?? seen.reply_delivered_at;
        if (
          seen.event_id === ev.event_id &&
          (seen.delivery !== ev.delivery ||
            seen.delivered_at !== ev.delivered_at ||
            seen.reply_delivered_at !== replyAt ||
            joined(seen) !== joined(ev))
        ) {
          this.bySeq.set(
            ev.seq,
            withJoined(
              { ...seen, delivery: ev.delivery, delivered_at: ev.delivered_at, reply_delivered_at: replyAt },
              joined(ev),
            ),
          );
          this.sorted = null;
        }
      }
      return;
    }
    this.bySeq.set(ev.seq, ev);
    this.sorted = null;
    // An older page loaded behind the live tail ends no stream.
    const fresh = ev.seq > this.lastSeq;
    if (advance) this.lastSeq = Math.max(this.lastSeq, ev.seq);

    switch (ev.type) {
      case "user.message":
        if (ev.client_op_id) {
          this.pending.delete(ev.client_op_id);
          this.echoed.add(ev.client_op_id);
        }
        break;
      case "agent.message":
        if (fresh) this.streaming = "";
        break;
      case "status":
        if (fresh && ev.payload.state !== "running") this.streaming = "";
        break;
      case "tool.call":
        this.calls.set(ev.payload.call_id, ev);
        break;
      case "tool.result":
        this.results.set(ev.payload.call_id, ev);
        break;
      case "system":
        if (ev.payload.code === "task_notification") {
          latest(this.endsByCall, ev.payload.call_id, ev);
          latest(this.endsByTask, ev.payload.task_id, ev);
        }
        break;
      case "approval.request":
      case "clarify.request":
        this.requests.set(ev.payload.request_id, ev);
        break;
      case "approval.response":
      case "clarify.response":
        this.fate(ev.payload.request_id).response = ev.payload;
        break;
      case "approval.cancel":
      case "clarify.cancel":
        this.fate(ev.payload.request_id).cancel = ev.payload;
        break;
    }
  }

  /** Where a (re)subscription resumes: before the first input still `pending`, so the hub
   *  sends it again with its current `delivery` (a `delivery` frame may have been lost while
   *  the socket was down), else `lastSeq`. */
  resumeSeq(): number {
    let from = this.lastSeq;
    for (const [seq, ev] of this.bySeq) {
      if (ev.delivery === "pending" && seq <= from) from = seq - 1;
    }
    return Math.max(0, from);
  }

  /** A `delivery` frame (SPEC §6): new `delivery` / `delivered_at` of a stored input, and
   *  `joined` when it went into the turn already running. A change for an event not loaded
   *  yet is dropped: the event arrives with current fields. */
  applyDelivery(d: DeliveryChange): void {
    const ev = this.bySeq.get(d.seq);
    if (!ev || ev.event_id !== d.event_id) return;
    this.bySeq.set(d.seq, withJoined({ ...ev, delivery: d.delivery, delivered_at: d.delivered_at }, d.joined === true));
    this.sorted = null;
    this.changed();
  }

  /** The hub's copies of events already loaded (the newest page fetched again when a kept
   *  chat opens): `delivery`, `delivered_at`, `reply_delivered_at` and `payload.joined` may
   *  have changed while nothing was subscribed, and the hub tells only subscriptions. Events
   *  not loaded, `lastSeq` and `olderFrom` stay: the subscription brings what came after
   *  `lastSeq`. */
  refresh(evs: HqcEvent[]): void {
    let changed = false;
    for (const ev of evs) {
      const seen = ev.seq === undefined ? undefined : this.bySeq.get(ev.seq);
      if (!seen || seen.event_id !== ev.event_id) continue;
      const replyAt = ev.reply_delivered_at ?? seen.reply_delivered_at;
      if (
        seen.delivery === ev.delivery &&
        seen.delivered_at === ev.delivered_at &&
        seen.reply_delivered_at === replyAt &&
        joined(seen) === joined(ev)
      )
        continue;
      this.bySeq.set(
        ev.seq!,
        withJoined({ ...seen, delivery: ev.delivery, delivered_at: ev.delivered_at, reply_delivered_at: replyAt }, joined(ev)),
      );
      changed = true;
    }
    if (!changed) return;
    this.sorted = null;
    this.changed();
  }

  addInvalid(raw: unknown, error: string): void {
    const seq = (raw as { seq?: unknown })?.seq;
    const s = typeof seq === "number" ? seq : undefined;
    if (s !== undefined && this.invalid.some((i) => i.seq === s)) return;
    if (s !== undefined) this.lastSeq = Math.max(this.lastSeq, s);
    this.invalid.push({
      key: `inv-${s ?? this.invalid.length}-${this.version}`,
      seq: s,
      error,
      raw,
    });
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

  request(
    requestId: string,
  ): EventOf<"approval.request" | "clarify.request"> | undefined {
    return this.requests.get(requestId);
  }

  resultFor(callId: string): EventOf<"tool.result"> | undefined {
    return this.results.get(callId);
  }

  /** The latest `system task_notification` of the task the call `callId` started: by its
   *  `call_id`, else by its `task_id` when `taskId` (the runtime's id of the task) is given. */
  taskEnd(callId: string, taskId?: string): EventOf<"system"> | undefined {
    return this.endsByCall.get(callId) ?? (taskId === undefined ? undefined : this.endsByTask.get(taskId));
  }

  /** The last `tool.call` while it has no `tool.result`. */
  runningCall(): EventOf<"tool.call"> | undefined {
    const evs = this.events();
    for (let i = evs.length - 1; i >= 0; i--) {
      const e = evs[i];
      if (e.type === "tool.call")
        return this.results.has(e.payload.call_id) ? undefined : e;
    }
    return undefined;
  }

  /** From when `call` counts as running, in ms: its `ts`, or the `ts` of the last answer to a
   *  request after it if later (a permission or a question it, or a subagent it runs, waited
   *  for). */
  runSince(call: EventOf<"tool.call">): number {
    const evs = this.events();
    const since = Date.parse(call.ts);
    for (let i = evs.length - 1; i >= 0 && evs[i].seq! > call.seq!; i--) {
      const e = evs[i];
      if (e.type === "approval.response" || e.type === "clarify.response")
        return Math.max(since, Date.parse(e.ts));
    }
    return since;
  }

  /** Requests with neither an answer nor a cancellation, oldest first. */
  openRequests(): EventOf<"approval.request" | "clarify.request">[] {
    return [...this.requests.values()]
      .filter((r) => {
        const f = this.fates.get(r.payload.request_id);
        return !f?.response && !f?.cancel;
      })
      .sort((a, b) => a.seq! - b.seq!);
  }

  /** Model of the last `usage` that names one, and the session's cost: the runtime's own
   *  running total of the last `usage` that has one (`session_cost_usd`, T27), else the sum
   *  of `cost_usd` over the loaded history. */
  usageTotals(): { model?: string; cost?: number } {
    let model: string | undefined;
    let sum: number | undefined;
    let total: number | undefined;
    for (const e of this.events()) {
      if (e.type !== "usage") continue;
      if (e.payload.model) model = e.payload.model;
      if (e.payload.cost_usd !== undefined)
        sum = (sum ?? 0) + e.payload.cost_usd;
      if (e.payload.session_cost_usd !== undefined)
        total = e.payload.session_cost_usd;
    }
    return { model, cost: total ?? sum };
  }

  /** The call of a result: results whose call is in the timeline are shown with it. */
  callFor(callId: string): EventOf<"tool.call"> | undefined {
    return this.calls.get(callId);
  }

  addPending(
    clientOpId: string,
    text: string,
    attachments?: PendingAttachment[],
  ): void {
    if (this.echoed.has(clientOpId)) return;
    const p: PendingMessage = {
      client_op_id: clientOpId,
      text,
      status: "sending",
    };
    if (attachments?.length) p.attachments = attachments;
    this.pending.set(clientOpId, p);
    this.changed();
  }

  failPending(clientOpId: string, error: string): void {
    const p = this.pending.get(clientOpId);
    if (!p) return;
    this.pending.set(clientOpId, { ...p, status: "failed", error });
    this.changed();
  }

  dropPending(clientOpId: string): PendingMessage | undefined {
    const p = this.pending.get(clientOpId);
    this.pending.delete(clientOpId);
    this.changed();
    return p;
  }

  /** The socket dropped: deltas after the gap would not continue the buffered text, and the
   *  subagents may have ended meanwhile (the next subscribe brings the hub's list). */
  reset(): void {
    if (!this.streaming && !this.liveAgents.length) return;
    this.streaming = "";
    this.liveAgents = [];
    this.changed();
  }

  events(): HqcEvent[] {
    if (!this.sorted)
      this.sorted = [...this.bySeq.values()].sort((a, b) => a.seq! - b.seq!);
    return this.sorted;
  }

  items(): Item[] {
    const out: Item[] = this.events().map((e) => ({
      kind: "event",
      key: `e${e.seq}`,
      event: e,
    }));
    for (const inv of this.invalid) {
      const item: Item = { kind: "invalid", key: inv.key, invalid: inv };
      const at =
        inv.seq === undefined ? -1 : out.findIndex((i) => seqOf(i) > inv.seq!);
      if (at < 0) out.push(item);
      else out.splice(at, 0, item);
    }
    // An input that came while the agent's text was being written stands under that text
    // (SPEC §6): the `agent.message` goes above the inputs with `during_text` right before it
    // in `seq` order, never above the input it answers. A final one takes the `usage` right
    // after it along, which the feed draws in its block.
    let floor = 0;
    for (let i = 0; i < out.length; i++) {
      const a = out[i];
      if (a.kind !== "event" || a.event.type !== "agent.message") continue;
      const next = out[i + 1];
      const n = a.event.payload.final && next?.kind === "event" && next.event.type === "usage" ? 2 : 1;
      const answers = a.event.payload.in_reply_to;
      let at = i;
      while (at > floor) {
        const prev = out[at - 1];
        if (prev.kind !== "event" || !duringText(prev) || prev.event.event_id === answers) break;
        at--;
      }
      if (at < i) out.splice(at, 0, ...out.splice(i, n));
      floor = i + n;
      i += n - 1;
    }
    for (const p of this.pending.values()) {
      out.push({ kind: "pending", key: `p-${p.client_op_id}`, pending: p });
    }
    return out;
  }

  /** True if some user message ended with `delivery: failed` (SPEC §5.2): offer retry. */
  hasFailedDelivery(): boolean {
    return this.events().some(
      (e) => e.type === "user.message" && e.delivery === "failed",
    );
  }
}

/** A stored input the hub marked as come while the agent's text was being written (SPEC §6). */
export function duringText(it: Item): boolean {
  return (
    it.kind === "event" &&
    (it.event.type === "user.message" || it.event.type === "peer.message") &&
    it.event.payload.during_text === true
  );
}

function seqOf(i: Item): number {
  if (i.kind === "event") return i.event.seq ?? Infinity;
  if (i.kind === "invalid") return i.invalid.seq ?? Infinity;
  return Infinity;
}

/** `payload.joined` of the user's message: the hub sets it when the runtime took the message
 *  into the turn already running (SPEC §17.2), the one change of a stored payload. */
function joined(e: HqcEvent): boolean {
  return e.type === "user.message" && e.payload.joined === true;
}

/** `e` with `payload.joined` set when `on`; it is never taken back. */
function withJoined(e: HqcEvent, on: boolean): HqcEvent {
  return on && e.type === "user.message" && !e.payload.joined
    ? { ...e, payload: { ...e.payload, joined: true } }
    : e;
}

/** `ev` under `key` in `m` unless a later one (by `seq`) is there: an older page may load
 *  after it. No key, no entry. */
function latest(m: Map<string, EventOf<"system">>, key: string | undefined, ev: EventOf<"system">): void {
  if (key === undefined) return;
  const had = m.get(key);
  if (!had || had.seq! < ev.seq!) m.set(key, ev);
}
