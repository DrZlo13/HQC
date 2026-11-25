// One open chat: history over REST, live tail over /api/ws, actions over REST (SPEC §6).

import { ApiError, eventPageOf, type Api, type EventPage } from './api';
import { deviceHistoryCache, type HistoryCache, type HistoryRange } from './historycache';
import type { ClarifyAction, ClarifyAnswer, HqcEvent } from './proto';
import { Timeline, type PendingAttachment } from './timeline';
import type { HubSocket } from './ws';

/** The newest events, shown before anything older loads. */
const TAIL = 100;
/** The older history one scroll to the top loads. */
const OLDER = 200;
/** The older history a jump to an event loads, page by page. */
const PAGE = 1000;
/** How long the events of the subscription wait before the device keeps them together. */
const KEEP_AFTER_MS = 300;
/** The longest the chat waits to draw the events of a subscription's backfill, which come in a
 *  burst: one draw per frame would render a long chat once for each of them. */
const CATCH_UP_DRAW_MS = 100;

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
  /** Counts `open` and `close`: what an earlier one started stops at its next step. */
  private run = 0;
  private loading: Promise<void> | null = null;
  /** The hub's newest page on its way behind the copy of the device that `load` showed. */
  private syncing: Promise<void> | null = null;
  private olderLoad: Promise<void> | null = null;
  /** The events of the subscription the device does not keep yet, new ones and ones the hub
   *  sent again, and the timer that keeps them. */
  private unkept: unknown[] = [];
  private resent: unknown[] = [];
  private keepTimer: ReturnType<typeof setTimeout> | undefined;
  /** From the subscribe until the hub's `subscribed` (and again after a socket reset, which
   *  subscribes with a backfill once more) the events are applied without drawing each. */
  private catchingUp = false;
  /** The draw that is due for the events applied quietly; set exactly while one is waiting.
   *  The hub answers every `subscribe` (`subscribed`, or an `error` for a refused one), but a
   *  long backfill shows as it arrives, not only at its end: a draw is due `CATCH_UP_DRAW_MS`
   *  after the first event nothing has drawn. */
  private drawTimer: ReturnType<typeof setTimeout> | undefined;
  /** The session is deleted (`forget`). */
  private forgotten = false;
  /** The newest page of history is in. */
  loaded = false;
  /** Between `open` and `close`. */
  isOpen = false;
  /** An older page is on its way (`loadOlder`). */
  loadingOlder = false;
  /** The last older page failed: what loads by itself waits for the user's scroll or the next
   *  `open` (`loadOlder`'s `auto`). */
  private olderFailed = false;

  /** Called with the text of each message once the hub stored it. */
  onStored?: (text: string) => void;
  /** Called with each event the subscription brings. */
  onEvent?: (ev: HqcEvent) => void;

  constructor(
    private readonly api: Api,
    private readonly socket: HubSocket,
    readonly id: string,
    private readonly cache: HistoryCache = deviceHistoryCache(),
  ) {}

  /** Loads the newest page of history, once: a later call waits for the same load, one after
   *  a failed load tries again. What the device holds of it shows first and resolves the load;
   *  the hub's page follows behind it (`syncTail`). */
  load(): Promise<void> {
    this.loading ??= this.loadTail().catch((e) => {
      this.loading = null;
      throw e;
    });
    return this.loading;
  }

  private async loadTail(): Promise<void> {
    const t = this.timeline;
    const stored = await this.cache.range(this.id);
    const shown = stored && (await this.showStored(stored));
    if (shown) {
      this.syncing = this.syncTail(stored, shown).finally(() => (this.syncing = null));
      return;
    }
    const page = await this.newestPage();
    this.applyPage(page);
    t.olderFrom = olderFrom(page, TAIL);
    this.loaded = true;
    this.store(page.raw);
  }

  /** Shows the newest events of the range the device holds; their event ids by `seq`, or
   *  undefined when they cannot be read. */
  private async showStored(stored: HistoryRange): Promise<Map<number, string> | undefined> {
    const lo = Math.max(stored.from, stored.to - TAIL + 1);
    const raws = await this.cache.read(this.id, lo, stored.to);
    if (!raws) return undefined;
    const page = eventPageOf(raws, 0);
    const ids = new Map<number, string>();
    for (const raw of raws) {
      const o = raw as { seq: number; event_id?: unknown };
      if (typeof o.event_id === 'string') ids.set(o.seq, o.event_id);
    }
    this.applyPage({ ...page, maxSeq: Math.max(page.maxSeq, stored.to) });
    this.timeline.olderFrom = lo <= 1 ? 0 : lo;
    this.loaded = true;
    return ids;
  }

  /** The hub's newest page after the copy of the device was shown: it goes on from it, or the
   *  copy does not continue the hub's history (the hub is ahead by more than a page, or has
   *  less, or another event stands at a `seq` both have) and is dropped from the screen. */
  private async syncTail(stored: HistoryRange, shown: Map<number, string>): Promise<void> {
    const t = this.timeline;
    try {
      const page = await this.newestPage();
      const another = page.raw.some((r) => {
        const o = r as { seq?: unknown; event_id?: unknown };
        return typeof o.seq === 'number' && shown.has(o.seq) && shown.get(o.seq) !== o.event_id;
      });
      const apart = (page.minSeq ?? Infinity) > stored.to + 1 || page.maxSeq < stored.to || another;
      if (apart) {
        // What the device holds goes too, or `put` would join the page to events the hub lacks;
        // nothing is awaited between clearing the screen and the page.
        await this.cache.drop(this.id);
        t.clear();
      }
      this.applyPage(page);
      t.olderFrom = apart ? olderFrom(page, TAIL) : Math.min(t.olderFrom, olderFrom(page, TAIL));
      this.store(page.raw);
    } catch (e) {
      // A session the hub does not know (deleted elsewhere, or another user's in this browser)
      // shows nothing, as it does without a copy.
      if (e instanceof ApiError && e.status === 404) t.clear();
      console.warn('history sync failed, relying on the copy and subscribe', e);
    }
  }

  /** The hub's newest page; a session it does not know is forgotten by the device. */
  private newestPage(): Promise<EventPage> {
    return this.api.eventsBefore(this.id, undefined, TAIL).catch((e) => {
      if (e instanceof ApiError && e.status === 404) void this.cache.drop(this.id);
      throw e;
    });
  }

  /** Loads the newest page of history (unless it is in) and subscribes after what is loaded;
   *  resolves then, so a long chat shows at once. A chat shown from the copy of the device
   *  subscribes once the hub's newest page is in, which may drop the copy. A chat whose page
   *  was in fetches it again for the fields changed meanwhile (`Timeline.refresh`). The older
   *  history loads on demand (`loadOlder`). */
  async open(): Promise<void> {
    const run = ++this.run;
    this.isOpen = true;
    this.olderFailed = false;
    void this.cache.touch(this.id);
    const kept = this.loaded;
    try {
      await this.load();
    } catch (e) {
      // History over REST is an optimisation: subscribe replays everything after lastSeq.
      console.warn('history load failed, relying on subscribe', e);
    }
    if (run !== this.run) return;
    const syncing = this.syncing;
    if (syncing) void syncing.then(() => run === this.run && this.subscribe());
    else this.subscribe();
    if (kept && !syncing) void this.refresh(run);
  }

  private subscribe(): void {
    const t = this.timeline;
    this.catchingUp = true;
    this.unsubscribe = this.socket.subscribe(this.id, {
      afterSeq: () => t.resumeSeq(),
      onEvent: (ev, raw) => {
        // The hub sends an old event again when it changes (a reply's delivery): not a new one.
        const fresh = ev.seq === undefined || ev.seq > t.lastSeq;
        if (this.catchingUp) {
          t.applyQuiet(ev);
          this.drawTimer ??= setTimeout(() => this.drawNow(), CATCH_UP_DRAW_MS);
        } else t.apply(ev);
        this.keep(raw, fresh);
        this.onEvent?.(ev);
      },
      onDelivery: (d) => {
        t.applyDelivery(d);
        // After the events the device is about to keep, so that the change finds its event.
        this.keepNow();
        void this.cache.patch(this.id, d);
      },
      onInvalid: (raw, err) => {
        const seq = (raw as { seq?: unknown } | undefined)?.seq;
        const fresh = typeof seq !== 'number' || seq > t.lastSeq;
        t.addInvalid(raw, err);
        this.keep(raw, fresh);
      },
      onSubscribed: () => {
        this.drawNow();
        this.catchingUp = false;
      },
      onReset: () => {
        // The events still waiting for a draw are drawn now: `reset` may have nothing to notify.
        this.drawNow();
        this.catchingUp = true;
        t.reset();
      },
    });
  }

  /** Draws the events applied quietly, if there are any. */
  private drawNow(): void {
    if (this.drawTimer === undefined) return;
    clearTimeout(this.drawTimer);
    this.drawTimer = undefined;
    this.timeline.changed();
  }

  /** The device keeps an event of the subscription with the ones that come within
   *  `KEEP_AFTER_MS`; one that is not new only rewrites its copy (`HistoryCache.update`). */
  private keep(raw: unknown, fresh: boolean): void {
    if (this.forgotten || typeof (raw as { seq?: unknown } | undefined)?.seq !== 'number') return;
    (fresh ? this.unkept : this.resent).push(raw);
    this.keepTimer ??= setTimeout(() => this.keepNow(), KEEP_AFTER_MS);
  }

  private keepNow(): void {
    clearTimeout(this.keepTimer);
    this.keepTimer = undefined;
    const fresh = this.unkept;
    const resent = this.resent;
    this.unkept = [];
    this.resent = [];
    // The new ones first: a copy that is sent again may be among them.
    if (fresh.length) this.store(fresh);
    if (resent.length) this.store(resent, true);
  }

  /** Hands events to the device's copy (`resent`: ones the hub sent again, which only rewrite
   *  theirs); nothing once the session is forgotten, or a late write would bring back a copy
   *  of a deleted session. */
  private store(raws: unknown[], resent = false): void {
    if (this.forgotten) return;
    void (resent ? this.cache.update(this.id, raws) : this.cache.put(this.id, raws));
  }

  /** The device's copies were deleted: what the subscription brought and is not written yet
   *  stays out of the new ones. The chat goes on keeping what comes after. */
  dropUnwritten(): void {
    clearTimeout(this.keepTimer);
    this.keepTimer = undefined;
    this.unkept = [];
    this.resent = [];
  }

  /** The session was deleted: nothing of it reaches the device from now on, including what
   *  the subscription brought within `KEEP_AFTER_MS` and is not written yet. */
  forget(): void {
    this.forgotten = true;
    clearTimeout(this.keepTimer);
    this.keepTimer = undefined;
    this.unkept = [];
    this.resent = [];
  }

  private async refresh(run: number): Promise<void> {
    try {
      const page = await this.api.eventsBefore(this.id, undefined, TAIL);
      this.store(page.raw);
      if (run === this.run) this.timeline.refresh(page.events);
    } catch (e) {
      console.warn('history refresh failed', e);
    }
  }

  /** Loads older history, one call at a time (a second one gets the same promise): one page
   *  before `Timeline.olderFrom`, or with `downTo` pages down to that `seq`. From the device
   *  while its copy reaches there, else from the hub. Stops on `close`. `auto`: a call nobody
   *  asked for (the feed needs more by itself) does nothing after a failed older page, until
   *  a call without it or the next `open`: no tight loop of requests while the hub is down. */
  loadOlder(downTo?: number, auto = false): Promise<void> {
    if (auto && this.olderFailed && !this.olderLoad) return Promise.resolve();
    this.olderLoad ??= this.pageOlder(this.run, downTo).finally(() => {
      this.olderLoad = null;
      this.loadingOlder = false;
      this.timeline.changed();
    });
    return this.olderLoad;
  }

  private async pageOlder(run: number, downTo?: number): Promise<void> {
    const t = this.timeline;
    this.loadingOlder = true;
    t.changed();
    try {
      // The copy shown from the device may still be dropped.
      await this.syncing;
      if (t.olderFrom <= 0) return;
      do {
        const size = downTo === undefined ? OLDER : PAGE;
        const { page, asked } = await this.olderPage(t.olderFrom, size);
        if (run !== this.run) return;
        t.olderFrom = olderFrom(page, asked);
        this.applyPage(page);
      } while (downTo !== undefined && t.olderFrom > 0 && t.olderFrom > downTo);
      this.olderFailed = false;
    } catch (e) {
      this.olderFailed = true;
      console.warn('older history load failed', e);
    }
  }

  /** The `size` events before `before`, from the device when its copy holds them, else from the
   *  hub (which the device then keeps); `asked` is how many the page was to have. */
  private async olderPage(before: number, size: number): Promise<{ page: EventPage; asked: number }> {
    const r = await this.cache.range(this.id);
    if (r && r.from < before && r.to >= before - 1) {
      const lo = Math.max(r.from, before - size);
      const raws = await this.cache.read(this.id, lo, before - 1);
      // An input stored as pending was delivered or taken back since the chat was closed, and the
      // subscription does not resend it (it resumes after the loaded events only): from the copy
      // it would stand at the bottom as queued. The hub's page says what became of it and
      // rewrites the copy.
      const pending = raws?.some((x) => (x as { delivery?: unknown }).delivery === 'pending');
      if (raws && !pending) return { page: eventPageOf(raws, 0), asked: before - lo };
    }
    const page = await this.api.eventsBefore(this.id, before, size);
    this.store(page.raw);
    return { page, asked: size };
  }

  private applyPage(page: EventPage): void {
    const t = this.timeline;
    t.applyAll(page.events);
    for (const inv of page.invalid) t.addInvalid(inv.raw, inv.error);
    t.lastSeq = Math.max(t.lastSeq, page.maxSeq);
  }

  /** Unsubscribes and stops loading the older history; what is loaded stays for the next
   *  `open`, but not a half-written answer: the hub sends it whole on the next subscribe. */
  close(): void {
    this.run++;
    this.isOpen = false;
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.drawNow();
    this.keepNow();
    this.timeline.reset();
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

  /** Asks the agent a side question (SPEC 0.232): the stored question is shown at once, its
   *  answer comes over the socket. A refusal is the caller's. */
  async sideQuestion(question: string): Promise<void> {
    this.timeline.applyReply(await this.api.sideQuestion(this.id, question));
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

  /** The card closes when the hub took the answer, not when its event comes over the socket. */
  async approve(requestId: string, optionId: string, message?: string): Promise<void> {
    if (!(await this.taken(requestId, () => this.api.approve(this.id, requestId, optionId, message)))) return;
    this.timeline.markAnswered(requestId, {
      request_id: requestId,
      option_id: optionId,
      ...(message !== undefined && { message }),
    });
  }

  async answer(requestId: string, answers: Record<string, ClarifyAnswer>, action?: ClarifyAction): Promise<void> {
    if (!(await this.taken(requestId, () => this.api.clarify(this.id, requestId, answers, action)))) return;
    this.timeline.markAnswered(requestId, { request_id: requestId, answers, ...(action && { action }) });
  }

  /** Whether the hub took the answer. A request closed first, from another device, is not an
   *  error here: the newest events are fetched and only the event that closed this request is
   *  applied, as a reply is (`lastSeq` stays, the late socket still brings the rest in order),
   *  so the card shows what became of it. Without that event among them the `409` stands. */
  private async taken(requestId: string, post: () => Promise<void>): Promise<boolean> {
    try {
      await post();
      return true;
    } catch (e) {
      if (!(e instanceof ApiError && e.code === 'request_closed')) throw e;
      const page = await this.api.eventsBefore(this.id, undefined, TAIL);
      const closing = page.events.filter(
        (ev) =>
          (ev.type === 'approval.response' ||
            ev.type === 'clarify.response' ||
            ev.type === 'approval.cancel' ||
            ev.type === 'clarify.cancel') &&
          ev.payload.request_id === requestId,
      );
      if (!closing.length) throw e;
      for (const ev of closing) this.timeline.applyReply(ev);
      return false;
    }
  }

  stop(): Promise<void> {
    return this.api.stop(this.id);
  }

  background(callId: string): Promise<void> {
    return this.api.background(this.id, callId);
  }

  /** Takes the message back: the hub drops it and the timeline stops showing it at once. */
  async withdraw(event: { event_id: string; seq?: number }): Promise<void> {
    // A chat shown from the device's copy is checked against the hub's newest page first: that
    // page, asked for before the take-back, would show the message waiting again.
    await this.syncing;
    await this.api.withdrawInput(this.id, event.event_id);
    if (event.seq === undefined) return;
    const d = { session_id: this.id, event_id: event.event_id, seq: event.seq, delivery: 'withdrawn' as const };
    this.timeline.applyDelivery(d);
    // The hub's `delivery` frame is lost while the socket is down: the copy must not keep the
    // message waiting. After the events the device is about to keep, as for that frame.
    this.keepNow();
    void this.cache.patch(this.id, d);
  }

  /** Sends the waiting message into the running turn at once; whether the agent had not taken
   *  it yet. */
  sendNow(eventId: string): Promise<boolean> {
    return this.api.inputNow(this.id, eventId);
  }

  stopTask(taskId: string): Promise<void> {
    return this.api.stopTask(this.id, taskId);
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

/** Where the history not loaded yet ends after `page`, which was to have `asked` events: 0 once
 *  it holds the first one (the hub's `seq` start at 1 and have no gaps). */
function olderFrom(page: EventPage, asked: number): number {
  return page.count < asked || (page.minSeq ?? 0) <= 1 ? 0 : page.minSeq!;
}

function pendingFiles(files: OutgoingFile[]): PendingAttachment[] {
  return files.map((f) => ({ name: f.name, size: f.file.size, preview: f.preview }));
}

/** The question of a message `/btw <question>` or `/side <question>` (SPEC 0.232); undefined for
 *  any other text, `/btw` alone too. */
export function sideQuestionOf(text: string): string | undefined {
  return /^\/(?:btw|side)\s+(\S[\s\S]*)$/.exec(text)?.[1].trimEnd();
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
