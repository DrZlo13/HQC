// One open chat: history over REST, live tail over /api/ws, actions over REST (SPEC §6).

import type { Api } from './api';
import type { ClarifyAnswer } from './proto';
import { Timeline } from './timeline';
import type { HubSocket } from './ws';

const PAGE = 200;

export class ChatSession {
  readonly timeline = new Timeline();
  private unsubscribe: (() => void) | null = null;
  private reportedRead = 0;
  private closed = false;

  constructor(
    private readonly api: Api,
    private readonly socket: HubSocket,
    readonly id: string,
  ) {}

  /** Loads history page by page, then subscribes from the last seen `seq`. */
  async open(): Promise<void> {
    try {
      for (;;) {
        const page = await this.api.events(this.id, this.timeline.lastSeq, PAGE);
        if (this.closed) return;
        this.timeline.applyAll(page.events);
        for (const inv of page.invalid) this.timeline.addInvalid(inv.raw, inv.error);
        this.timeline.lastSeq = Math.max(this.timeline.lastSeq, page.maxSeq);
        if (page.count < PAGE) break;
      }
    } catch (e) {
      // History over REST is an optimisation: subscribe replays everything after lastSeq.
      console.warn('history load failed, relying on subscribe', e);
    }
    if (this.closed) return;
    const t = this.timeline;
    this.unsubscribe = this.socket.subscribe(this.id, {
      afterSeq: () => t.lastSeq,
      onEvent: (ev) => t.apply(ev),
      onDelivery: (d) => t.applyDelivery(d),
      onInvalid: (raw, err) => t.addInvalid(raw, err),
      onReset: () => t.reset(),
    });
  }

  close(): void {
    this.closed = true;
    this.unsubscribe?.();
    this.unsubscribe = null;
  }

  /** Shows the message at once and reconciles it with the hub echo by `client_op_id`. */
  send(text: string): string {
    const op = newOpId();
    this.timeline.addPending(op, text);
    this.post(op, text);
    return op;
  }

  /** Same `client_op_id`: if the first POST was stored but its reply lost, the hub can
   *  dedupe instead of running the message twice. */
  resend(clientOpId: string): void {
    const p = this.timeline.dropPending(clientOpId);
    if (!p) return;
    this.timeline.addPending(clientOpId, p.text);
    this.post(clientOpId, p.text);
  }

  discard(clientOpId: string): void {
    this.timeline.dropPending(clientOpId);
  }

  /** The reply carries the stored event (also for a repeated `client_op_id`), so the
   *  message is reconciled even if the socket is down; the echo with the same `seq` is a
   *  no-op. */
  private post(op: string, text: string): void {
    this.api.input(this.id, text, op).then(
      (ev) => this.timeline.applyReply(ev),
      (e: Error) => this.timeline.failPending(op, e.message),
    );
  }

  approve(requestId: string, optionId: string): Promise<void> {
    return this.api.approve(this.id, requestId, optionId);
  }

  answer(requestId: string, answers: Record<string, ClarifyAnswer>): Promise<void> {
    return this.api.clarify(this.id, requestId, answers);
  }

  stop(): Promise<void> {
    return this.api.stop(this.id);
  }

  retry(): Promise<void> {
    return this.api.retry(this.id);
  }

  /** Moves the read cursor to the last seen `seq`; no-op if already there. */
  markRead(): void {
    const seq = this.timeline.lastSeq;
    if (seq <= this.reportedRead) return;
    this.reportedRead = seq;
    this.api.readCursor(this.id, seq).catch(() => {
      this.reportedRead = 0;
    });
  }
}

/** `crypto.randomUUID` needs a secure context; a phone on plain-HTTP LAN does not have one. */
export function newOpId(): string {
  if (typeof crypto.randomUUID === 'function' && globalThis.isSecureContext !== false) {
    return crypto.randomUUID();
  }
  const b = crypto.getRandomValues(new Uint8Array(16));
  b[6] = (b[6] & 0x0f) | 0x40;
  b[8] = (b[8] & 0x3f) | 0x80;
  const h = [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}
