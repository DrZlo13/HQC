// One open chat: history over REST, live tail over /api/ws, actions over REST (SPEC §6).

import type { Api, EventPage } from './api';
import type { ClarifyAnswer } from './proto';
import { Timeline, type PendingAttachment } from './timeline';
import type { HubSocket } from './ws';

/** The newest events, shown before the rest of the history loads. */
const TAIL = 100;
/** Older history, loaded page by page behind the tail. */
const PAGE = 1000;

/** A file picked in the composer, with its preview. */
export interface OutgoingFile {
  file: Blob;
  name: string;
  /** `data:` URL for an image. */
  preview?: string;
}

interface Outgoing {
  text: string;
  files: OutgoingFile[];
  /** Artifact ids of the files uploaded so far, by position: a resend does not upload again. */
  ids: (string | undefined)[];
}

export class ChatSession {
  readonly timeline = new Timeline();
  private unsubscribe: (() => void) | null = null;
  private readonly outgoing = new Map<string, Outgoing>();
  private reportedRead = 0;
  private closed = false;

  /** Called with the text of each message once the hub stored it. */
  onStored?: (text: string) => void;

  constructor(
    private readonly api: Api,
    private readonly socket: HubSocket,
    readonly id: string,
  ) {}

  /** Loads the newest page of history and subscribes from it; resolves then, so a long chat
   *  shows at once. The older pages load after that, newest first (`Timeline.olderFrom`). */
  async open(): Promise<void> {
    const t = this.timeline;
    try {
      const page = await this.api.eventsBefore(this.id, undefined, TAIL);
      if (this.closed) return;
      this.applyPage(page);
      t.olderFrom = page.count < TAIL ? 0 : (page.minSeq ?? 0);
    } catch (e) {
      // History over REST is an optimisation: subscribe replays everything after lastSeq.
      console.warn('history load failed, relying on subscribe', e);
    }
    if (this.closed) return;
    this.unsubscribe = this.socket.subscribe(this.id, {
      afterSeq: () => t.resumeSeq(),
      onEvent: (ev) => t.apply(ev),
      onDelivery: (d) => t.applyDelivery(d),
      onInvalid: (raw, err) => t.addInvalid(raw, err),
      onReset: () => t.reset(),
    });
    void this.loadOlder();
  }

  private async loadOlder(): Promise<void> {
    const t = this.timeline;
    try {
      while (t.olderFrom > 1 && !this.closed) {
        const page = await this.api.eventsBefore(this.id, t.olderFrom, PAGE);
        if (this.closed) return;
        t.olderFrom = page.count < PAGE ? 0 : (page.minSeq ?? 0);
        this.applyPage(page);
      }
      t.olderFrom = 0;
    } catch (e) {
      console.warn('older history load failed', e);
    }
  }

  private applyPage(page: EventPage): void {
    const t = this.timeline;
    t.applyAll(page.events);
    for (const inv of page.invalid) t.addInvalid(inv.raw, inv.error);
    t.lastSeq = Math.max(t.lastSeq, page.maxSeq);
  }

  close(): void {
    this.closed = true;
    this.unsubscribe?.();
    this.unsubscribe = null;
  }

  /** Shows the message at once and reconciles it with the hub echo by `client_op_id`.
   *  Files are uploaded first (`POST /api/artifacts`), then the input carries their ids
   *  (SPEC §12). */
  send(text: string, files: OutgoingFile[] = []): string {
    const op = newOpId();
    this.outgoing.set(op, { text, files, ids: [] });
    this.timeline.addPending(op, text, pendingFiles(files));
    this.post(op);
    return op;
  }

  /** Same `client_op_id`: if the first POST was stored but its reply lost, the hub can
   *  dedupe instead of running the message twice. */
  resend(clientOpId: string): void {
    const p = this.timeline.dropPending(clientOpId);
    if (!p) return;
    const o = this.outgoing.get(clientOpId) ?? { text: p.text, files: [], ids: [] };
    this.outgoing.set(clientOpId, o);
    this.timeline.addPending(clientOpId, o.text, pendingFiles(o.files));
    this.post(clientOpId);
  }

  discard(clientOpId: string): void {
    this.outgoing.delete(clientOpId);
    this.timeline.dropPending(clientOpId);
  }

  /** The reply carries the stored event (also for a repeated `client_op_id`), so the
   *  message is reconciled even if the socket is down; the echo with the same `seq` is a
   *  no-op. */
  private post(op: string): void {
    const o = this.outgoing.get(op);
    if (!o) return;
    this.upload(o)
      .then((ids) => this.api.input(this.id, o.text, op, ids))
      .then(
        (ev) => {
          this.outgoing.delete(op);
          this.timeline.applyReply(ev);
          this.onStored?.(o.text);
        },
        (e: Error) => this.timeline.failPending(op, e.message),
      );
  }

  private async upload(o: Outgoing): Promise<string[]> {
    for (const [i, f] of o.files.entries()) {
      if (o.ids[i]) continue;
      try {
        o.ids[i] = (await this.api.uploadArtifact(f.file, f.name)).id;
      } catch (e) {
        throw new Error(`${f.name}: ${(e as Error).message}`);
      }
    }
    return o.ids as string[];
  }

  approve(requestId: string, optionId: string, message?: string): Promise<void> {
    return this.api.approve(this.id, requestId, optionId, message);
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

function pendingFiles(files: OutgoingFile[]): PendingAttachment[] {
  return files.map((f) => ({ name: f.name, size: f.file.size, preview: f.preview }));
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
