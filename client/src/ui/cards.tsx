// The feed of a chat (T26): agent messages as text without a bubble, the user's in a bubble
// on the right (or, in the Compact look, a plate from the left edge and a mark in a
// gutter before every other item), runs of tool calls folded into groups, request cards,
// service lines. A message's time sits at the right end of its first line, its copy and fork
// buttons at the other edge on hover. One component per event type of SPEC §4; unknown and
// invalid events render as a collapsed row and never throw.

import type { ComponentChildren } from 'preact';
import { useMemo, useState } from 'preact/hooks';
import type { ClarifyAnswer, ClarifyQuestion, EventOf, HqcEvent, SessionState } from '../proto';
import type { InvalidEvent, Item, PendingMessage, Timeline } from '../timeline';
import type { ArtifactStore } from '../artifacts';
import { AttachmentView, PendingAttachments, type ViewedArtifact } from './attachments';
import {
  askTarget,
  buildFeed,
  callLine,
  callPeek,
  callRows,
  fileChange,
  groupDuration,
  groupState,
  relativePaths,
  toolKind,
  toolLabel,
  type Block,
  type ToolRow,
} from './feed';
import { useAction, useIsPhone, useNow } from './hooks';
import {
  IconArrowIn,
  IconArrowOut,
  IconAttach,
  IconCheck,
  IconChevron,
  IconCross,
  IconFork,
  IconQuestion,
  IconShield,
  IconWarn,
} from './icons';
import {
  CANCEL_LABELS,
  LOG_LEVEL_LABELS,
  STATE_LABELS,
  SYSTEM_CODE_LABELS,
  SYSTEM_WARNING_CODES,
  formatClock,
  formatCost,
  formatDay,
  formatDuration,
  formatTime,
  formatTimer,
  formatTokens,
  oneLine,
  count,
} from './labels';
import { CodeBlock, CopyButton, Markdown, markdownLine } from './Markdown';
import { PermissionBlock } from './permission';
import { PeerMessageBlock, ReplyFromBlock, ReplyToBlock, SentBlock } from './relay';

/** What cards need from the chat: lookups and actions. */
export interface FolderActions {
  /** The folder, as shown (`~/work/x`). */
  path: string;
  /** The folder's rules screen. */
  rulesHref: string;
  /** Saves hqc rules of the folder; rejects when the hub did not store them. */
  save(rules: string[]): Promise<void>;
}

export interface ChatActions {
  timeline: Timeline;
  approve(requestId: string, optionId: string, message?: string): Promise<void>;
  answer(requestId: string, answers: Record<string, ClarifyAnswer>): Promise<void>;
  resend(clientOpId: string): void;
  discard(clientOpId: string): void;
  /** Opens the artifact in a new tab; `download`: as an attachment. */
  openArtifact(id: string, download?: boolean): void;
  /** Shows an image or a Markdown text in the viewer over the chat; absent (the requests
   *  screen): they open as `openArtifact`. */
  viewArtifact?(a: ViewedArtifact): void;
  /** Metadata and signed links of artifacts; without it attachments are plain cards. */
  artifacts?: ArtifactStore;
  /** Wall clock, injectable for tests. */
  now?: () => number;
  /** The session's folder on the worker (absolute): a request touching a path outside it
   *  says so. */
  workspaceRoot?: string;
  /** The worker's service session (SPEC §10.2): its requests are the worker's own. */
  system?: boolean;
  /** The session's workspace (T48): "Allow in this folder" saves hqc rules into it; absent
   *  without a workspace, and where the card is shown away from its session. */
  folder?: FolderActions;
  /** The request whose card takes Enter and Esc. */
  hotkeyRequest?: string;
  /** The request shown as the bottom sheet on a phone: not drawn in the feed. */
  sheetRequest?: string;
  /** A session of the user by id or address, for the other side of a message between
   *  sessions (T51); undefined for a deleted or unknown one. */
  findSession?(ref: string): { id: string; address: string; title?: string } | undefined;
  /** The event at `seq` of session `sessionId`: a message this session sent there (T51). */
  peerMessage?(sessionId: string, seq: number): Promise<HqcEvent | undefined>;
  /** A new session with the conversation before this message of the user, the message in
   *  its input (SPEC §6 fork); absent where a session cannot fork. */
  fork?(event: EventOf<'user.message'>): void;
}

export function ChatItems({ items, actions }: { items: Item[]; actions: ChatActions }) {
  return <FeedBlocks blocks={buildFeed(items, actions.timeline)} actions={actions} />;
}

export function FeedBlocks({ blocks, actions }: { blocks: Block[]; actions: ChatActions }) {
  // `data-seq`: the first event of the block, for the jump to a search hit (Chat).
  return (
    <>
      {blocks.map((b) => {
        // The request on a phone's bottom sheet is not drawn here, nor its mark.
        const g: Gutter = b.kind === 'approval' && b.event.payload.request_id === actions.sheetRequest ? {} : gutter(b);
        return (
          <div key={b.key} class={g.full ? 'feed-block feed-full' : 'feed-block'} data-seq={blockSeq(b)}>
            <BlockView block={b} actions={actions} />
            {/* After the block, so a fold's open state turns its chevron (styles.css). */}
            {g.mark}
          </div>
        );
      })}
    </>
  );
}

interface Gutter {
  mark?: ComponentChildren;
  full?: boolean;
}

const mark = (kind: string, icon: ComponentChildren) => (
  <span class={`feed-mark feed-mark-${kind}`} aria-hidden="true">
    {icon}
  </span>
);

/** The block's mark in the gutter of the one-column feed (the Compact look,
 *  shown by styles.css only there): a grey dot for the agent's text, the state of a run of
 *  tools, an arrow for a message between sessions, a shield for a permission request, a
 *  question for the agent's questions, a clip for a file, a chevron for what folds. The user's
 *  messages and the service lines between hairlines have none and span the feed (`full`). */
function gutter(b: Block): Gutter {
  switch (b.kind) {
    case 'agent':
      return { mark: mark('dot', <span class="feed-dot" />) };
    case 'tools': {
      const state = groupState(b.rows);
      if (state === 'running') return { mark: mark('running', <span class="pulse-dot" />) };
      return { mark: mark(state, state === 'ok' ? <IconCheck size={13} /> : <IconCross size={13} />) };
    }
    case 'sent':
    case 'reply-to':
      return { mark: mark('relay', <IconArrowOut />) };
    case 'reply-from':
      return { mark: mark('relay', <IconArrowIn />) };
    case 'approval':
      return { mark: mark('perm', <IconShield size={14} />) };
    case 'item': {
      const it = b.item;
      if (it.kind === 'pending') return { full: true };
      if (it.kind === 'invalid') return {};
      const e = it.event;
      switch (e.type) {
        case 'user.message':
          return { full: true };
        case 'agent.message':
          return e.payload.runtime ? { full: true } : { mark: mark('dot', <span class="feed-dot" />) };
        case 'agent.delta':
          return { mark: mark('dot', <span class="feed-dot" />) };
        case 'agent.thinking':
          return { mark: mark('fold', <IconChevron size={12} />) };
        case 'approval.request':
          return { mark: mark('perm', <IconShield size={14} />) };
        case 'clarify.request':
          return { mark: mark('ask', <IconQuestion />) };
        case 'attachment':
          return { mark: mark('file', <IconAttach size={14} />) };
        case 'peer.message':
          return { mark: mark('relay', <IconArrowIn />) };
        case 'system':
          if (e.payload.code === 'task_notification') return { mark: mark('fold', <IconChevron size={12} />) };
          return { full: true };
        case 'unknown':
          return {};
        default:
          // The answer to a request and the other service lines.
          return { full: true };
      }
    }
  }
}

export function blockSeq(b: Block): number | undefined {
  switch (b.kind) {
    case 'agent':
    case 'approval':
    case 'reply-to':
      return b.event.seq;
    case 'sent':
    case 'reply-from':
      return (b.kind === 'sent' ? b.call : b.result).seq;
    case 'tools': {
      const first = b.rows[0];
      const row = first?.kind === 'call' ? (first.call ?? first.result) : first?.event;
      return row?.seq;
    }
    case 'item':
      return b.item.kind === 'event' ? b.item.event.seq : undefined;
  }
}

function BlockView({ block, actions }: { block: Block; actions: ChatActions }) {
  switch (block.kind) {
    case 'agent':
      return <AgentMessage event={block.event} usage={block.usage} />;
    case 'tools':
      return <ToolGroup block={block} actions={actions} />;
    case 'sent':
      return <SentBlock block={block} actions={actions} />;
    case 'reply-from':
      return <ReplyFromBlock block={block} actions={actions} />;
    case 'reply-to':
      return <ReplyToBlock block={block} actions={actions} />;
    case 'approval':
      if (block.event.payload.request_id === actions.sheetRequest) return null;
      return <PermissionBlock event={block.event} call={block.call} actions={actions} />;
    case 'item': {
      const it = block.item;
      switch (it.kind) {
        case 'event':
          return <EventView event={it.event} actions={actions} />;
        case 'invalid':
          return <InvalidCard invalid={it.invalid} />;
        case 'pending':
          return <PendingBubble pending={it.pending} actions={actions} />;
      }
    }
  }
}

/** The "requests only" filter of the service session (SPEC §10.2): the request cards,
 *  which show their own answer or cancellation. */
export function onlyRequests(items: Item[]): Item[] {
  return items.filter(
    (it) => it.kind === 'event' && (it.event.type === 'approval.request' || it.event.type === 'clarify.request'),
  );
}

/** How long a call runs (`Timeline.runSince`) before the activity line names it (SPEC §17.1):
 *  the agent's quick calls (a Read, a grep) would swap the line for a moment each. */
const CALL_SHOWN_AFTER_MS = 2000;

/** The last line of the feed while a turn is on or input waits (SPEC §17.1): the running
 *  command with its timer, or what the session is doing. A command or subagent the runtime
 *  runs in the foreground (`foreground`, the session's `foreground_calls`) gets "Run in
 *  background", which calls `onBackground` with its `call_id`. */
export function ActivityLine({
  state,
  timeline,
  now,
  foreground,
  onBackground,
}: {
  state: SessionState;
  timeline: Timeline;
  now?: () => number;
  foreground?: string[];
  onBackground?: (callId: string) => void;
}) {
  const call = state === 'running' ? timeline.runningCall() : undefined;
  const t = useNow(1000, !!call, now);
  if (call && t - timeline.runSince(call) >= CALL_SHOWN_AFTER_MS) {
    const asked = askTarget(call.payload.tool, call.payload.args_preview);
    if (asked !== undefined) {
      // `hqc_ask` waits for another session's final answer (SPEC §7.1).
      return (
        <div class="activity activity-running activity-ask" role="status" aria-live="polite">
          <span class="pulse-dot" aria-hidden="true" />
          <span class="activity-label">Waiting for a reply</span>
          <span class="activity-what mono">{asked ? `from ${asked}` : ''}</span>
          <span class="activity-timer">{formatTimer(t - Date.parse(call.ts))}</span>
        </div>
      );
    }
    const line = callLine(call.payload.tool, call.payload.args_preview);
    const callId = call.payload.call_id;
    return (
      <div class="activity activity-running" role="status" aria-live="polite">
        <span class="pulse-dot" aria-hidden="true" />
        <span class="activity-label">Running</span>
        <span class="activity-what mono">{line.text ? (line.shell ? line.text : `${call.payload.tool} ${line.text}`) : call.payload.tool}</span>
        <span class="activity-timer">{formatTimer(t - Date.parse(call.ts))}</span>
        {onBackground && foreground?.includes(callId) && (
          <button type="button" class="btn-link activity-background" onClick={() => onBackground(callId)}>
            Run in background
          </button>
        )}
      </div>
    );
  }
  let label: string | null = null;
  if (state === 'running') label = 'Agent is working';
  else if (state === 'queued') label = 'Input queued';
  else if (state === 'waiting' && timeline.openRequests().length === 0) label = 'Waiting for approval';
  if (!label) return null;
  return (
    <div class={`activity activity-${state}`} role="status" aria-live="polite">
      {state === 'running' && <span class="pulse-dot" aria-hidden="true" />}
      <span class="activity-label">{label}</span>
    </div>
  );
}

export function EventView({ event, actions }: { event: HqcEvent; actions: ChatActions }) {
  switch (event.type) {
    case 'user.message':
      return <UserMessage event={event} actions={actions} />;
    case 'agent.delta':
      return <StreamingMessage text={event.payload.text} />;
    case 'subagents.live':
      // Above the input (LiveSubagents), never in the feed.
      return null;
    case 'agent.message':
      // A reply Claude Code wrote itself (an API error, "Credit balance is too low"): a
      // service line, not the agent's text.
      if (event.payload.runtime) return <Line kind="system line-runtime">{event.payload.text}</Line>;
      return <AgentMessage event={event} />;
    case 'agent.thinking':
      return <Thinking event={event} />;
    case 'tool.call':
    case 'tool.result':
      // Drawn in tool groups (feed.ts).
      return null;
    case 'approval.request':
      return <PermissionBlock event={event} actions={actions} />;
    case 'approval.response': {
      const p = event.payload;
      // A text answer is the user's message (SPEC §10.1).
      if (p.message) return <TextAnswer event={event} />;
      // Otherwise the request's row says it; alone (its request not loaded) it is a line.
      if (actions.timeline.request(p.request_id)) return null;
      return <Line kind="approval">Answer to the request: {p.option_id}</Line>;
    }
    case 'approval.cancel':
    case 'clarify.cancel':
      if (actions.timeline.request(event.payload.request_id)) return null;
      return (
        <Line kind="cancel">
          {event.type === 'approval.cancel' ? 'Permission request' : 'Question'} cancelled:{' '}
          {CANCEL_LABELS[event.payload.reason]}
        </Line>
      );
    case 'clarify.request':
      return <ClarifyCard event={event} actions={actions} />;
    case 'clarify.response':
      if (actions.timeline.request(event.payload.request_id)) return null;
      return <Line kind="approval">Answers sent</Line>;
    case 'attachment':
      return (
        <AttachmentView
          id={event.payload.artifact_id}
          kind={event.payload.kind}
          name={event.payload.name}
          caption={event.payload.caption}
          actions={actions}
        />
      );
    case 'peer.message':
      return <PeerMessageBlock event={event} actions={actions} />;
    case 'usage':
      return (
        <div class="line line-usage">
          <UsageParts usage={event} />
        </div>
      );
    case 'status':
      // The state is in the header and the activity line; only an error with a reason
      // is worth a line of its own.
      if (event.payload.state !== 'error' || !event.payload.detail) return null;
      return (
        <Line kind="status state-error">
          {STATE_LABELS.error} · {event.payload.detail}
        </Line>
      );
    case 'system':
      return <SystemLine event={event} now={actions.now} />;
    case 'unknown':
      return (
        <details class="card card-unknown">
          <summary>Unknown event · {event.wire_type}</summary>
          <pre class="pre">{JSON.stringify(event.payload, null, 2)}</pre>
        </details>
      );
  }
}

/** A service line between hairlines; the creation of the session says when; a warning
 *  (a forked transcript, SPEC §9.3) gets an icon. */
function SystemLine({ event, now }: { event: EventOf<'system'>; now?: () => number }) {
  const p = event.payload;
  if (p.code === 'worker_log') return <WorkerLogLine event={event} />;
  if (p.code === 'task_notification') return <TaskNotification text={p.text} />;
  // A compaction with the runtime's summary folds it; one without (older events carry the
  // fixed text) is text as is.
  if (p.code === 'context_compacted') {
    if (p.text && p.text !== 'Context compacted') return <CompactionSummary text={p.text} />;
    return <Line kind={`system line-system-${p.code}`}>{p.text || 'Context compacted'}</Line>;
  }
  const caption = SYSTEM_CODE_LABELS[p.code];
  const warning = SYSTEM_WARNING_CODES.has(p.code);
  const kind = `system line-system-${p.code}${warning ? ' line-warning' : ''}`;
  if (p.code === 'session_created') {
    const when = formatDay(event.ts, now?.() ?? Date.now());
    return (
      <Line kind={kind}>
        {p.text === 'Session created' ? `Session started ${when}` : `${p.text} · ${when}`}
      </Line>
    );
  }
  return (
    <Line kind={caption ? kind : 'system'}>
      {warning && (
        <span class="warn-icon" role="img" aria-label="Warning">
          <IconWarn />
        </span>
      )}
      {caption && <b>{caption}</b>}
      <Markdown text={p.text} />
    </Line>
  );
}

/** A background task's notification, folded: "Background task ·" and its whole text on one
 *  line, without Markdown marks, cut where the line ends (a first line alone, often a heading
 *  like "### Summary", read as the whole message), dimmed in italics like the Thinking line's
 *  start of its text; open, the text as Markdown. */
function TaskNotification({ text }: { text: string }) {
  // The feed renders again on every streamed piece; the text is parsed once. A report runs to
  // tens of KB, and its first 2000 characters are wider than any window.
  const peek = useMemo(() => markdownLine(text.slice(0, 2000)), [text]);
  return (
    <details class="line-fold line-system-task_notification">
      <summary>
        <IconChevron size={12} />
        <span class="fold-label">Background task ·</span>
        <span class="fold-peek">{peek}</span>
      </summary>
      <Markdown class="fold-text" text={text} />
    </details>
  );
}

/** A compaction of the context, folded like a background task: "Context compacted"; open, the
 *  summary the runtime wrote, as Markdown. */
function CompactionSummary({ text }: { text: string }) {
  return (
    <details class="line-fold line-system-context_compacted">
      <summary>
        <IconChevron size={12} />
        <span class="fold-label">Context compacted</span>
      </summary>
      <Markdown class="fold-text" text={text} />
    </details>
  );
}

/** A warn/error line of the worker's own log in its service session (SPEC §10.2): time,
 *  level and the text as is (log text, not Markdown). */
function WorkerLogLine({ event }: { event: EventOf<'system'> }) {
  const level = event.payload.level ?? 'warn';
  return (
    <div class={`line-worker-log level-${level}`}>
      <time class="log-time" dateTime={event.ts}>
        {formatClock(event.ts)}
      </time>{' '}
      <span class="log-level">{LOG_LEVEL_LABELS[level] ?? level}</span>{' '}
      <span class="log-text">{event.payload.text}</span>
    </div>
  );
}

function Line({ kind, children }: { kind: string; children: preact.ComponentChildren }) {
  return (
    <div class={`line line-${kind}`}>
      <span class="line-text">{children}</span>
    </div>
  );
}

/** A message's time at the right end of its first line, as Telegram has it: floated, so the
 *  text wraps around it; `children` after the time ("· scheduled"). */
function MessageTime({ ts, before, children }: { ts: string; before?: ComponentChildren; children?: ComponentChildren }) {
  return (
    <span class="msg-time">
      {before}
      <time dateTime={ts}>{formatTime(ts)}</time>
      {children}
    </span>
  );
}

/** The user's bubble (a plate in one column) with its time and, when there is one, its note
 *  ("joined the turn"): in a bubble the first line with the time at its end, the text under
 *  them; left of the time on a plate (styles.css). */
function Bubble({
  ts,
  note,
  time,
  buttons,
  children,
}: {
  ts: string;
  note?: string;
  time?: ComponentChildren;
  /** Shown on hover left of the time (a computer's; a phone has them under the message). */
  buttons?: ComponentChildren;
  children: ComponentChildren;
}) {
  return (
    <div class={['msg-bubble', note && 'msg-noted', buttons && 'msg-bubble-acts'].filter(Boolean).join(' ')}>
      <MessageTime ts={ts} before={buttons && <span class="msg-time-actions">{buttons}</span>}>
        {time}
      </MessageTime>
      {note && <span class="msg-note">{note}</span>}
      {children}
    </div>
  );
}

/** A message's buttons, shown on hover at the edge opposite its time (styles.css): left of the
 *  feed, clear of the time and of the next block. The user's message has them in its bubble,
 *  left of the time, on a computer (`Bubble`). */
function MessageActions({ children }: { children: ComponentChildren }) {
  return <div class="msg-actions">{children}</div>;
}

function UserMessage({ event, actions }: { event: EventOf<'user.message'>; actions: ChatActions }) {
  const d = event.delivery;
  const ids = event.payload.artifact_ids;
  const fork = actions.fork && event.author.kind === 'user' && d !== 'pending' && d !== 'failed';
  const phone = useIsPhone();
  const buttons = (event.payload.text || fork) && [
    event.payload.text && <CopyButton key="copy" text={event.payload.text} icon />,
    fork && (
      <button
        key="fork"
        type="button"
        class="btn-copy btn-copy-idle btn-fork"
        aria-label="Fork from here"
        title="Fork from here: a new session with the conversation before this message"
        onClick={() => actions.fork!(event)}
      >
        <IconFork />
      </button>
    ),
  ];
  return (
    <div class="msg-user">
      <Bubble
        ts={event.ts}
        buttons={!phone && buttons}
        note={event.payload.joined ? 'joined the turn' : undefined}
        time={[
          event.payload.schedule_id && <span class="scheduled"> · scheduled</span>,
          d === 'pending' && <span class="delivery"> · awaiting delivery</span>,
          d === 'failed' && <span class="delivery delivery-failed"> · not delivered</span>,
        ]}
      >
        {event.payload.text && <UserText text={event.payload.text} />}
        {ids.length > 0 && (
          <div class="attachments">
            {ids.map((id) => (
              <AttachmentView key={id} id={id} actions={actions} />
            ))}
          </div>
        )}
      </Bubble>
      {phone && buttons && <MessageActions>{buttons}</MessageActions>}
    </div>
  );
}

/** What the user typed, as the tab shows it: the text as is with its line breaks, no
 *  Markdown. */
function UserText({ text }: { text: string }) {
  return <div class="text msg-text">{text}</div>;
}

/** "Reply with text instead": the user's text went to the agent with the denial. */
function TextAnswer({ event }: { event: EventOf<'approval.response'> }) {
  const text = event.payload.message!;
  return (
    <div class="msg-user msg-answer">
      <Bubble ts={event.ts} note="instead of the request">
        <UserText text={text} />
      </Bubble>
      <MessageActions>
        <CopyButton text={text} icon />
      </MessageActions>
    </div>
  );
}

function PendingBubble({ pending, actions }: { pending: PendingMessage; actions: ChatActions }) {
  return (
    <div class={`msg-user msg-pending ${pending.status === 'failed' ? 'msg-failed' : ''}`}>
      <div class="msg-bubble">
        {pending.text && <UserText text={pending.text} />}
        {pending.attachments && <PendingAttachments files={pending.attachments} />}
      </div>
      {pending.status === 'sending' ? (
        <div class="meta">sending…</div>
      ) : (
        // An alert of its own under the message: what happened on the left, the two ways out
        // on the right.
        <div class="send-alert" role="alert">
          <span class="send-alert-icon">
            <IconWarn size={15} />
          </span>
          <span class="send-alert-text">
            <b>Not sent</b>
            {pending.error && <span class="send-alert-reason">{pending.error}</span>}
          </span>
          <span class="send-alert-acts">
            <button type="button" class="btn btn-small btn-primary btn-resend" onClick={() => actions.resend(pending.client_op_id)}>
              Retry
            </button>
            <button type="button" class="btn btn-small btn-discard" onClick={() => actions.discard(pending.client_op_id)}>
              Discard
            </button>
          </span>
        </div>
      )}
    </div>
  );
}

/** Tokens, cache and cost of a turn: `2 → 555 tokens · cache 14.4k · $0.073`. */
export function UsageParts({ usage }: { usage: EventOf<'usage'> }) {
  const p = usage.payload;
  const parts = [`${formatTokens(p.input_tokens)} → ${formatTokens(p.output_tokens)} tokens`];
  if (p.cache_read) parts.push(`cache ${formatTokens(p.cache_read)}`);
  if (p.cost_usd !== undefined) parts.push(formatCost(p.cost_usd, true));
  return (
    <>
      {parts.map((s, i) => (
        <span key={i} class="usage-part">
          {i > 0 && ' · '}
          {s}
        </span>
      ))}
    </>
  );
}

function AgentMessage({ event, usage }: { event: EventOf<'agent.message'>; usage?: EventOf<'usage'> }) {
  return (
    <article class={`msg-agent ${event.payload.final ? 'msg-final' : 'msg-interim'}`}>
      <MessageTime ts={event.ts} />
      <Markdown class="text" text={event.payload.text} />
      {usage && (
        <div class="meta">
          <span class="usage">
            <UsageParts usage={usage} />
          </span>
        </div>
      )}
      <MessageActions>
        <CopyButton text={event.payload.text} icon />
      </MessageActions>
    </article>
  );
}

export function StreamingMessage({ text }: { text: string }) {
  return (
    <article class="msg-agent msg-streaming" aria-live="polite">
      {/* The cursor is drawn by CSS after the last block. */}
      <Markdown class="text" text={text} />
    </article>
  );
}

function Thinking({ event }: { event: EventOf<'agent.thinking'> }) {
  return (
    <details class="thinking">
      <summary>
        <IconChevron size={12} />
        Thinking · {oneLine(event.payload.text, 60)}
      </summary>
      {/* The feed's own font, no box: prose, as in the VS Code tab. */}
      <div class="thinking-text">{event.payload.text}</div>
    </details>
  );
}

/** A run of tool calls, collapsed: "N tool calls · time", then the call's line when it is
 *  one, else the tools it called, each once, in the order first called; open, a row per call
 *  (T26). */
function ToolGroup({ block, actions }: { block: Extract<Block, { kind: 'tools' }>; actions: ChatActions }) {
  const [open, setOpen] = useState(false);
  const calls = callRows(block.rows);
  const state = groupState(block.rows);
  const ms = groupDuration(block.rows);
  const n = count(calls.length, 'tool call');
  const stats = ms !== undefined ? `${n} · ${formatDuration(ms)}` : n;
  const one = calls.length === 1 ? calls[0].call : undefined;
  const peek = one && callPeek(one.payload.tool, one.payload.args_preview, actions.workspaceRoot);
  const kinds = [...new Set(calls.flatMap((r) => (r.call ? [toolKind(r.call.payload.tool)] : [])))];
  return (
    <div class={`tool-group tool-${state} ${open ? 'is-open' : ''}`}>
      <button type="button" class="tool-group-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span class="chevron">
          <IconChevron />
        </span>
        <StateMark state={state} />
        <span class="tool-stats">{stats}</span>
        {peek ? (
          <span class="tool-peek mono">{peek}</span>
        ) : (
          calls.length > 1 && kinds.length > 0 && <span class="tool-peek tool-kinds">({kinds.join(', ')})</span>
        )}
      </button>
      {open && (
        <div class="tool-group-body">
          {block.rows.map((r) => (
            <ToolRowView key={r.key} row={r} actions={actions} />
          ))}
        </div>
      )}
    </div>
  );
}

function StateMark({ state }: { state: 'running' | 'error' | 'ok' }) {
  if (state === 'running') return <span class="pulse-dot" role="img" aria-label="running" />;
  return (
    <span class={`mark mark-${state}`} role="img" aria-label={state === 'ok' ? 'done' : 'error'}>
      {state === 'ok' ? <IconCheck size={13} /> : <IconCross size={13} />}
    </span>
  );
}

/** One call: `$ command` for Bash, else the tool and its main argument, paths under the
 *  session's folder relative to it; open, the change of an Edit, a MultiEdit or a Write (its
 *  result only if it failed), else the arguments and the result. */
function ToolRowView({ row, actions }: { row: ToolRow; actions: ChatActions }) {
  if (row.kind === 'thinking') return <Thinking event={row.event} />;
  const { call } = row;
  const result = call ? actions.timeline.resultFor(call.payload.call_id) ?? row.result : row.result;
  const r = result?.payload;
  const state = !r ? 'running' : r.error ? 'error' : 'ok';
  const line = call ? callLine(call.payload.tool, call.payload.args_preview) : undefined;
  // An Edit, a MultiEdit or a Write shows its change, as its permission card does (SPEC §10.1).
  const change = call ? fileChange(call.payload.tool, call.payload.args_preview) : undefined;
  return (
    <details class={`tool-row tool-${state}`}>
      <summary>
        {line?.shell ? (
          <span class="tool-prompt">$</span>
        ) : (
          <span class="tool-name">{call ? toolLabel(call.payload.tool) : 'Tool result'}</span>
        )}
        <span class="tool-line mono">
          {line ? relativePaths(line.text, actions.workspaceRoot) : oneLine(r?.result_preview ?? '', 80)}
        </span>
        {line?.more && <span class="tool-more">+{line.more} {line.more === 1 ? 'line' : 'lines'}</span>}
        {r?.duration_ms !== undefined && <span class="tool-time">{formatDuration(r.duration_ms)}</span>}
        <StateMark state={state} />
      </summary>
      <div class="tool-row-body">
        {change ? (
          <div class="tool-change">
            {change.parts.map((c, i) => (
              <CodeBlock key={i} code={c.code} lang={c.diff ? 'diff' : ''} title={c.title} />
            ))}
            {change.cut && <div class="tool-cut muted">The preview is cut at 4 KB: the rest of the change is not shown.</div>}
          </div>
        ) : (
          call && (
            <>
              <div class="label">Arguments</div>
              <pre class="pre">{call.payload.args_preview}</pre>
            </>
          )
        )}
        {/* A change that went through says only that it did: its result is not shown. */}
        {r && (r.error || !change) && (
          <>
            <div class="label">{r.error ? 'Error' : 'Result'}</div>
            {r.error_text && <div class="error-text">{r.error_text}</div>}
            {r.result_preview && <pre class="pre">{r.result_preview}</pre>}
            {r.artifact_id && (
              <button type="button" class="btn btn-small" onClick={() => actions.openArtifact(r.artifact_id!)}>
                Full result
              </button>
            )}
          </>
        )}
      </div>
    </details>
  );
}

/** "Other" of a question with options: whether it is chosen, and its text. */
interface OtherAnswer {
  on: boolean;
  text: string;
}

/** The agent's questions (AskUserQuestion), as the tab asks them: a question's header, each
 *  option's description under its label, checkboxes for a multi question, and "Other" whose
 *  text is the answer (with multi, beside the checked labels). */
export function ClarifyCard({ event, actions }: { event: EventOf<'clarify.request'>; actions: ChatActions }) {
  const p = event.payload;
  const fate = actions.timeline.requestFate(p.request_id);
  const open = !fate.response && !fate.cancel;
  const [answers, setAnswers] = useState<Record<string, ClarifyAnswer>>({});
  const [others, setOthers] = useState<Record<string, OtherAnswer>>({});
  const { busy, error, run } = useAction();
  const given = fate.response && 'answers' in fate.response ? fate.response.answers : undefined;

  const set = (q: ClarifyQuestion, v: string, checked: boolean) => {
    setAnswers((a) => {
      if (!q.multi) return { ...a, [q.id]: v };
      const cur = Array.isArray(a[q.id]) ? (a[q.id] as string[]) : [];
      return { ...a, [q.id]: checked ? [...cur, v] : cur.filter((x) => x !== v) };
    });
    // A single answer: an option takes the place of Other.
    if (!q.multi && q.options?.length) setOthers((o) => (o[q.id]?.on ? { ...o, [q.id]: { ...o[q.id], on: false } } : o));
  };
  const setOther = (q: ClarifyQuestion, change: Partial<OtherAnswer>) => {
    setOthers((o) => ({ ...o, [q.id]: { ...(o[q.id] ?? { on: false, text: '' }), ...change } }));
    // A single answer: Other takes the place of the chosen option.
    if (!q.multi && change.on) {
      setAnswers((a) => {
        const rest = { ...a };
        delete rest[q.id];
        return rest;
      });
    }
  };
  // What goes to the agent: an Other chosen with nothing typed is "Other", as the tab sends it.
  const collect = (): Record<string, ClarifyAnswer> => {
    const out = { ...answers };
    for (const q of p.questions) {
      const o = others[q.id];
      if (!q.options?.length || !o?.on) continue;
      const typed = o.text.trim() || 'Other';
      const cur = out[q.id];
      out[q.id] = q.multi ? [...(Array.isArray(cur) ? cur : []), typed] : typed;
    }
    return out;
  };
  // Answer waits for an answer to every question, as in the VS Code tab: an option picked, a
  // text typed, or Other chosen.
  const toSend = collect();
  const complete = p.questions.every((q) => {
    const v = toSend[q.id];
    return Array.isArray(v) ? v.length > 0 : v !== undefined && v.trim() !== '';
  });
  // What a question shows: the options picked and Other; once answered, what was sent, an
  // answer that is not an option being the text of Other.
  const shown = (q: ClarifyQuestion): { picked: string[]; other: OtherAnswer } => {
    const opts = q.options ?? [];
    if (!given) {
      const cur = answers[q.id];
      return { picked: Array.isArray(cur) ? cur : cur === undefined ? [] : [cur], other: others[q.id] ?? { on: false, text: '' } };
    }
    const v = given[q.id];
    const vals = Array.isArray(v) ? v : v === undefined ? [] : [v];
    const typed = vals.filter((x) => !opts.includes(x));
    return { picked: vals.filter((x) => opts.includes(x)), other: { on: typed.length > 0, text: typed.join(', ') } };
  };

  return (
    <form
      class={`card card-clarify ${open ? 'is-open' : 'is-closed'}`}
      onSubmit={(e) => {
        e.preventDefault();
        if (open && complete) run(() => actions.answer(p.request_id, toSend));
      }}
    >
      <div class="card-title">Agent asks</div>
      {p.questions.map((q) => {
        const s = shown(q);
        const name = `${p.request_id}-${q.id}`;
        const other = {
          name,
          'aria-label': 'Other',
          checked: s.other.on,
          onChange: (e: Event) => setOther(q, { on: (e.currentTarget as HTMLInputElement).checked }),
        };
        return (
          <fieldset key={q.id} class="question" disabled={!open || busy}>
            <legend>
              {q.header && <span class="question-header">{q.header}</span>}
              <span class="question-text">{q.text}</span>
            </legend>
            {q.options?.map((o, i) => {
              const desc = q.option_descriptions?.[i];
              const input = {
                name,
                value: o,
                checked: s.picked.includes(o),
                onChange: (e: Event) => set(q, o, (e.currentTarget as HTMLInputElement).checked),
              };
              return (
                <label key={o} class="choice">
                  {q.multi ? <input type="checkbox" {...input} /> : <input type="radio" {...input} />}
                  <span class="choice-body">
                    <span class="choice-label">{o}</span>
                    {desc && <span class="choice-desc">{desc}</span>}
                  </span>
                </label>
              );
            })}
            {!!q.options?.length && (
              <div class="choice choice-other">
                {q.multi ? <input type="checkbox" {...other} /> : <input type="radio" {...other} />}
                {/* Typing into the field chooses Other, as in the tab. */}
                <input
                  class="input"
                  type="text"
                  placeholder="Other"
                  aria-label="Other answer"
                  value={s.other.text}
                  onInput={(e) => {
                    const text = (e.currentTarget as HTMLInputElement).value;
                    setOther(q, text.trim() ? { text, on: true } : { text });
                  }}
                />
              </div>
            )}
            {!q.options?.length && (
              <input
                class="input"
                type="text"
                value={String((given ?? answers)[q.id] ?? '')}
                onInput={(e) => set(q, (e.currentTarget as HTMLInputElement).value, true)}
              />
            )}
          </fieldset>
        );
      })}
      {open && (
        <div class="actions">
          <button type="submit" class="btn btn-primary" disabled={busy || !complete}>
            Answer
          </button>
        </div>
      )}
      {fate.response && <div class="outcome">Answers sent</div>}
      {fate.cancel && <div class="outcome">Cancelled: {CANCEL_LABELS[fate.cancel.reason]}</div>}
      {error && <div class="error-text">{error}</div>}
    </form>
  );
}

function InvalidCard({ invalid }: { invalid: InvalidEvent }) {
  const type = (invalid.raw as { type?: unknown })?.type;
  return (
    <details class="card card-unknown card-invalid">
      <summary>Malformed event · {typeof type === 'string' ? type : '?'}</summary>
      <div class="error-text">{invalid.error}</div>
      <pre class="pre">{JSON.stringify(invalid.raw, null, 2)}</pre>
    </details>
  );
}
