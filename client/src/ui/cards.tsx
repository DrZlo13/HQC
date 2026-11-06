// The feed of a chat (T26): agent messages as text without a bubble, the user's in a bubble
// on the right, runs of tool calls folded into groups, request cards, service lines. One
// component per event type of SPEC §4; unknown and invalid events render as a collapsed
// row and never throw.

import { useState } from 'preact/hooks';
import type { ClarifyAnswer, ClarifyQuestion, EventOf, HqcEvent, SessionState } from '../proto';
import type { InvalidEvent, Item, PendingMessage, Timeline } from '../timeline';
import type { ArtifactStore } from '../artifacts';
import { AttachmentView, PendingAttachments } from './attachments';
import {
  askTarget,
  buildFeed,
  callLine,
  callPeek,
  callRows,
  groupDuration,
  groupState,
  relativePaths,
  toolLabel,
  type Block,
  type ToolRow,
} from './feed';
import { useAction, useNow } from './hooks';
import { IconCheck, IconChevron, IconCross, IconWarn } from './icons';
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
import { CopyButton, Markdown } from './Markdown';
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
}

export function ChatItems({ items, actions }: { items: Item[]; actions: ChatActions }) {
  return <FeedBlocks blocks={buildFeed(items, actions.timeline)} actions={actions} />;
}

export function FeedBlocks({ blocks, actions }: { blocks: Block[]; actions: ChatActions }) {
  // `data-seq`: the first event of the block, for the jump to a search hit (Chat).
  return (
    <>
      {blocks.map((b) => (
        <div key={b.key} class="feed-block" data-seq={blockSeq(b)}>
          <BlockView block={b} actions={actions} />
        </div>
      ))}
    </>
  );
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

/** The last line of the feed while a turn is on or input waits (SPEC §17.1): the running
 *  command with its timer, or what the session is doing. */
export function ActivityLine({ state, timeline, now }: { state: SessionState; timeline: Timeline; now?: () => number }) {
  const call = state === 'running' ? timeline.runningCall() : undefined;
  const t = useNow(1000, !!call, now);
  if (call) {
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
    return (
      <div class="activity activity-running" role="status" aria-live="polite">
        <span class="pulse-dot" aria-hidden="true" />
        <span class="activity-label">Running</span>
        <span class="activity-what mono">{line.text ? (line.shell ? line.text : `${call.payload.tool} ${line.text}`) : call.payload.tool}</span>
        <span class="activity-timer">{formatTimer(t - Date.parse(call.ts))}</span>
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
  // The runtime's own line about a compaction is text as is.
  if (p.code === 'context_compacted') {
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

/** A background task's notification, folded as the tab folds it: "Background task" and the
 *  first line of its summary; open, the whole text. */
function TaskNotification({ text }: { text: string }) {
  const peek = text.split('\n').find((l) => l.trim()) ?? '';
  return (
    <details class="line-fold line-system-task_notification">
      <summary>
        <IconChevron size={12} />
        <span class="fold-label">Background task</span>
        <span class="fold-peek">{peek.trim()}</span>
      </summary>
      <div class="fold-text">{text}</div>
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

function UserMessage({ event, actions }: { event: EventOf<'user.message'>; actions: ChatActions }) {
  const d = event.delivery;
  const ids = event.payload.artifact_ids;
  return (
    <div class="msg-user">
      <div class="msg-bubble">
        {event.payload.text && <UserText text={event.payload.text} />}
        {ids.length > 0 && (
          <div class="attachments">
            {ids.map((id) => (
              <AttachmentView key={id} id={id} actions={actions} />
            ))}
          </div>
        )}
      </div>
      <div class="meta">
        {event.payload.text && <CopyButton text={event.payload.text} />}
        {formatTime(event.ts)}
        {event.payload.schedule_id && <span class="scheduled"> · scheduled</span>}
        {d === 'pending' && <span class="delivery"> · awaiting delivery</span>}
        {d === 'failed' && <span class="delivery delivery-failed"> · not delivered</span>}
      </div>
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
      <div class="msg-bubble">
        <UserText text={text} />
      </div>
      <div class="meta">
        <CopyButton text={text} />
        {formatTime(event.ts)} · instead of the request
      </div>
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
      <div class="meta">
        {pending.status === 'sending' ? (
          'sending…'
        ) : (
          <>
            not sent: {pending.error}{' '}
            <button type="button" class="btn btn-small" onClick={() => actions.resend(pending.client_op_id)}>
              Retry
            </button>{' '}
            <button type="button" class="btn btn-small btn-ghost" onClick={() => actions.discard(pending.client_op_id)}>
              Discard
            </button>
          </>
        )}
      </div>
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
      <Markdown class="text" text={event.payload.text} />
      <div class="meta">
        <CopyButton text={event.payload.text} />
        {formatTime(event.ts)}
        {usage && (
          <span class="usage">
            {' · '}
            <UsageParts usage={usage} />
          </span>
        )}
      </div>
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
      <pre class="pre">{event.payload.text}</pre>
    </details>
  );
}

/** A run of tool calls, collapsed: "N commands · time" and the last call's line, as the tab
 *  shows it; open, a row per call (T26). */
function ToolGroup({ block, actions }: { block: Extract<Block, { kind: 'tools' }>; actions: ChatActions }) {
  const [open, setOpen] = useState(false);
  const calls = callRows(block.rows);
  const state = groupState(block.rows);
  const ms = groupDuration(block.rows);
  const n = count(calls.length, 'command');
  const stats = ms !== undefined ? `${n} · ${formatDuration(ms)}` : n;
  const last = calls.filter((r) => r.call).at(-1)?.call;
  const peek = last && callPeek(last.payload.tool, last.payload.args_preview, actions.workspaceRoot);
  return (
    <div class={`tool-group tool-${state} ${open ? 'is-open' : ''}`}>
      <button type="button" class="tool-group-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span class="chevron">
          <IconChevron />
        </span>
        <StateMark state={state} />
        <span class="tool-stats">{stats}</span>
        {peek && <span class="tool-peek mono">{peek}</span>}
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
 *  session's folder relative to it; open, the arguments and the result. */
function ToolRowView({ row, actions }: { row: ToolRow; actions: ChatActions }) {
  if (row.kind === 'thinking') return <Thinking event={row.event} />;
  const { call } = row;
  const result = call ? actions.timeline.resultFor(call.payload.call_id) ?? row.result : row.result;
  const r = result?.payload;
  const state = !r ? 'running' : r.error ? 'error' : 'ok';
  const line = call ? callLine(call.payload.tool, call.payload.args_preview) : undefined;
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
        {call && (
          <>
            <div class="label">Arguments</div>
            <pre class="pre">{call.payload.args_preview}</pre>
          </>
        )}
        {r && (
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
        if (open) run(() => actions.answer(p.request_id, collect()));
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
          <button type="submit" class="btn btn-primary" disabled={busy}>
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
