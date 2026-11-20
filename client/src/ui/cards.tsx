// The feed of a chat (T26): agent messages as text without a bubble, the user's in a bubble
// on the right (or, in the Compact look, a plate from the left edge and a mark in a
// gutter before every other item), runs of tool calls folded into groups, request cards,
// service lines. A message's time sits at the right end of its first line, its copy and fork
// buttons at the other edge on hover. One component per event type of SPEC §4; unknown and
// invalid events render as a collapsed row and never throw.

import type { SecretFields } from '../api';
import type { ComponentChildren } from 'preact';
import { useMemo, useState } from 'preact/hooks';
import type { Activity, ClarifyAction, ClarifyAnswer, ClarifyQuestion, EventOf, HqcEvent, ModelProvider, SessionState } from '../proto';
import type { InvalidEvent, Item, PendingMessage, Timeline } from '../timeline';
import type { ArtifactStore } from '../artifacts';
import { ModelName, modelMenuItems, type ModelChoice } from './agentSettings';
import { AttachmentView, PendingAttachments, type ViewedArtifact } from './attachments';
import { ContextMenu } from './ContextMenu';
import {
  argsText,
  artifactPage,
  askTarget,
  between,
  buildFeed,
  callArgs,
  callLine,
  callPeek,
  callRows,
  fileChange,
  groupDuration,
  groupState,
  relativePaths,
  resultLine,
  subagentEnd,
  subagentState,
  toolKind,
  toolLabel,
  type Block,
  type ToolRow,
} from './feed';
import { useAction, useIsPhone, useNow } from './hooks';
import {
  IconAgent,
  IconArrowIn,
  IconArrowOut,
  IconAttach,
  IconCheck,
  IconChevron,
  IconCross,
  IconFork,
  IconJoined,
  IconQuestion,
  IconReply,
  IconShield,
  IconStop,
  IconWarn,
} from './icons';
import {
  CANCEL_LABELS,
  LOG_LEVEL_LABELS,
  STATE_LABELS,
  SYSTEM_CODE_LABELS,
  SYSTEM_WARNING_CODES,
  activityLabel,
  compactionNote,
  formatClock,
  formatCost,
  formatDay,
  formatDuration,
  formatTime,
  formatTimer,
  formatTokens,
  modelName,
  oneLine,
  count,
} from './labels';
import { elapsed, stats } from './LiveSubagents';
import { CodeBlock, CopyButton, Markdown, markdownLine, safeHref } from './Markdown';
import { asker, PermissionBlock } from './permission';
import { PeerMessageBlock, ReplyFromBlock, ReplyToBlock, SentBlock } from './relay';

/** The session's model, for the offer under the runtime's reply to a model refusal (SPEC
 *  0.168): the one it has now, the providers its worker allows, and `to`, which switches it as
 *  the chat's menu does and resolves to whether the hub took it. */
export interface ModelSwitch {
  model?: string;
  provider?: string;
  providers: ModelProvider[];
  to(choice: ModelChoice): Promise<boolean>;
}

/** What cards need from the chat: lookups and actions. */
export interface FolderActions {
  /** The folder, as shown (`~/work/x`). */
  path: string;
  /** The folder's rules screen. */
  rulesHref: string;
  /** Saves hqc rules of the folder; rejects when the hub did not store them. */
  save(rules: string[]): Promise<void>;
}

/** An agent of a room as its posts show it. */
export interface RoomAgent {
  /** Its place among the room's agents. */
  index: number;
  /** Its session's current title. */
  name?: string;
  model?: string;
}

export interface ChatActions {
  timeline: Timeline;
  approve(requestId: string, optionId: string, message?: string): Promise<void>;
  /** `action`: the person's choice on an MCP server's request. */
  answer(requestId: string, answers: Record<string, ClarifyAnswer>, action?: ClarifyAction): Promise<void>;
  /** Sends `text` to the session as the user's message; absent where the card is shown away
   *  from its session and where the session takes no input. */
  send?(text: string): void;
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
  /** In a room: the agent (its place among the room's agents, current title and model) whose
   *  session is `id`, whose post is shown as a chat message; absent outside a room. */
  roomAgent?(id: string): RoomAgent | undefined;
  /** The event at `seq` of session `sessionId`: a message this session sent there (T51). */
  peerMessage?(sessionId: string, seq: number): Promise<HqcEvent | undefined>;
  /** A new session with the conversation before this message of the user, the message in
   *  its input (SPEC §6 fork); absent where a session cannot fork. */
  fork?(event: EventOf<'user.message'>): void;
  /** A fork before this message of the user is under way: its time says so, and its fork
   *  button waits. */
  forking?(eventId: string): boolean;
  /** Gives the secret an agent's `hqc_secret` asks for (SPEC 0.147); absent where it cannot
   *  be given (the session's worker does not take secrets). */
  giveSecret?(requestId: string, f: SecretFields): Promise<void>;
  /** The offer of another model under a refusal of the model (SPEC 0.168): absent where the
   *  chat header has no model pill to switch (a room, the service session, an archived one),
   *  and while VS Code holds the session — its message cannot be sent again from here. */
  modelSwitch?: ModelSwitch;
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
        const g: Gutter =
          b.kind === 'approval' && b.event.payload.request_id === actions.sheetRequest ? {} : gutter(b, actions);
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
 *  tools or of a subagent (a grey dot for one in the background), an arrow for a message
 *  between sessions, a shield for a permission request, a question for the agent's questions,
 *  a clip for a file, a chevron for what folds. The user's messages and the service lines
 *  between hairlines have none and span the feed (`full`). */
function gutter(b: Block, actions: ChatActions): Gutter {
  const timeline = actions.timeline;
  switch (b.kind) {
    case 'agent':
      return { mark: mark('dot', <span class="feed-dot" />) };
    case 'tools': {
      const state = groupState(b.rows);
      if (state === 'running') return { mark: mark('running', <span class="pulse-dot" />) };
      // A call its turn left without a result is marked as a failed one, as the VS Code tab does.
      return { mark: state === 'ok' ? mark('ok', <IconCheck size={13} />) : mark('error', <IconCross size={13} />) };
    }
    case 'subagent': {
      const { state } = subagentState(b.call, b.result, timeline);
      if (state === 'running') return { mark: mark('running', <span class="pulse-dot" />) };
      if (state === 'background') return { mark: mark('dot', <span class="feed-dot" />) };
      if (state === 'done') return { mark: mark('ok', <IconCheck size={13} />) };
      return { mark: mark('error', <IconCross size={13} />) };
    }
    case 'subagent-end':
      return subagentEnd(b.call, b.end).state === 'done'
        ? { mark: mark('ok', <IconCheck size={13} />) }
        : { mark: mark('error', <IconCross size={13} />) };
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
          // An agent's post in its room is a message of the room, marked as an agent's text.
          return actions.roomAgent?.(e.payload.from_session_id) ? { mark: mark('dot', <span class="feed-dot" />) } : { mark: mark('relay', <IconArrowIn />) };
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
    case 'subagent':
      return b.call.seq;
    case 'subagent-end':
      return b.end.seq;
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
    case 'subagent':
      return <SubagentCard block={block} actions={actions} />;
    case 'subagent-end':
      return <SubagentEndCard block={block} />;
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
          return <EventView event={it.event} actions={actions} queued={it.queued} />;
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
 *  background", which calls `onBackground` with its `call_id`. With no command running, what
 *  the agent waits on (`activity`, the session's) stands instead of "Agent is working". In a
 *  room each working agent has a line of its own, led by its `name`. */
export function ActivityLine({
  state,
  timeline,
  name,
  now,
  root,
  foreground,
  activity,
  onBackground,
}: {
  state: SessionState;
  timeline: Timeline;
  name?: string;
  now?: () => number;
  /** The session's folder: paths under it are shown relative to it, as in the feed. */
  root?: string;
  foreground?: string[];
  activity?: Activity;
  onBackground?: (callId: string) => void;
}) {
  const call = state === 'running' ? timeline.runningCall() : undefined;
  const t = useNow(1000, !!call, now);
  const who = name && <span class="activity-name">{name} · </span>;
  if (call && t - timeline.runSince(call) >= CALL_SHOWN_AFTER_MS) {
    const asked = askTarget(call.payload.tool, call.payload.args_preview);
    if (asked !== undefined) {
      // `hqc_ask` waits for another session's final answer (SPEC §7.1).
      return (
        <div class="activity activity-running activity-ask" role="status" aria-live="polite">
          <span class="pulse-dot" aria-hidden="true" />
          {who}
          <span class="activity-label">Waiting for a reply</span>
          <span class="activity-what mono">{asked ? `from ${asked}` : ''}</span>
          <span class="activity-timer">{formatTimer(t - Date.parse(call.ts))}</span>
        </div>
      );
    }
    const line = callLine(call.payload.tool, call.payload.args_preview);
    const text = relativePaths(line.text, root);
    const callId = call.payload.call_id;
    return (
      <div class="activity activity-running" role="status" aria-live="polite">
        <span class="pulse-dot" aria-hidden="true" />
        {who}
        <span class="activity-label">Running</span>
        <span class="activity-what mono">{line.shell && text ? text : [toolLabel(call.payload.tool), text].filter(Boolean).join(' ')}</span>
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
  if (state === 'running') label = activity ? activityLabel(activity) : name ? `${name} is working` : 'Agent is working';
  else if (state === 'queued') label = 'Input queued';
  else if (state === 'waiting' && timeline.openRequests().length === 0) label = 'Waiting for approval';
  if (!label) return null;
  return (
    <div class={`activity activity-${state}`} role="status" aria-live="polite">
      {state === 'running' && <span class="pulse-dot" aria-hidden="true" />}
      {state !== 'running' && who}
      <span class="activity-label">{label}</span>
    </div>
  );
}

/** `queued`: an input waiting for a turn of its own (`Item.queued`). */
export function EventView({ event, actions, queued }: { event: HqcEvent; actions: ChatActions; queued?: boolean }) {
  switch (event.type) {
    case 'user.message':
      return <UserMessage event={event} actions={actions} queued={queued} />;
    case 'agent.delta':
      return <StreamingMessage text={event.payload.text} />;
    case 'subagents.live':
    case 'schedules.live':
      // Above the input (LiveSubagents), never in the feed.
      return null;
    case 'agent.message':
      // A reply Claude Code wrote itself (an API error, "Credit balance is too low"): a
      // service line, not the agent's text.
      if (event.payload.runtime && event.payload.refusal) return <RefusalLine event={event} actions={actions} />;
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
      return <PeerMessageBlock event={event} actions={actions} queued={queued} />;
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
  if (p.code === 'task_notification') return <TaskNotification note={p} />;
  // A compaction with the runtime's summary folds it; one without (older events carry the
  // fixed text) is text as is. Either says how it came and what it freed when the runtime did.
  if (p.code === 'context_compacted') {
    const note = compactionNote(p);
    if (p.text && p.text !== 'Context compacted') return <CompactionSummary text={p.text} note={note} />;
    return (
      <Line kind={`system line-system-${p.code}`}>
        {p.text || 'Context compacted'}
        {note}
      </Line>
    );
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
  // The worker's Claude Code is signed out (SPEC 0.185): its sign-in is in Settings → Workers.
  if (p.code === 'claude_signed_out') {
    return (
      <Line kind={kind}>
        <span class="warn-icon" role="img" aria-label="Warning">
          <IconWarn />
        </span>
        <Markdown text={p.text} />
        <a class="btn btn-small line-sign-in" href="#/settings">
          Sign in
        </a>
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

/** How a background task ended, in its line's label (SPEC §6, since 0.181): `status` as the
 *  runtime says it; a task that completed, or says nothing, is only "Background task". */
const TASK_END_LABELS = new Map([
  ['failed', 'Background task failed ·'],
  ['stopped', 'Background task stopped ·'],
]);

/** A background task's notification, folded: "Background task ·" ("Background task failed ·",
 *  "… stopped ·" by its `status`) and its whole text on one line, without Markdown marks, cut
 *  where the line ends (a first line alone, often a heading like "### Summary", read as the
 *  whole message), dimmed in italics like the Thinking line's start of its text, then what it
 *  spent, when the runtime says, as the subagents' row above the input has it; open, the text
 *  as Markdown. */
function TaskNotification({ note }: { note: EventOf<'system'>['payload'] }) {
  const { text, usage } = note;
  // The feed renders again on every streamed piece; the text is parsed once. A report runs to
  // tens of KB, and its first 2000 characters are wider than any window.
  const peek = useMemo(() => markdownLine(text.slice(0, 2000)), [text]);
  return (
    <details class="line-fold line-system-task_notification">
      <summary>
        <IconChevron size={12} />
        <span class="fold-label">{TASK_END_LABELS.get(note.status ?? '') ?? 'Background task ·'}</span>
        <span class="fold-peek">{peek}</span>
        {usage && <span class="fold-stats">· {stats(usage, usage.duration_ms)}</span>}
      </summary>
      <Markdown class="fold-text" text={text} />
    </details>
  );
}

/** A compaction of the context, folded like a background task: "Context compacted" and `note`
 *  (` · auto · 153k tokens freed`); open, the summary the runtime wrote, as Markdown. */
function CompactionSummary({ text, note }: { text: string; note: string }) {
  return (
    <details class="line-fold line-system-context_compacted">
      <summary>
        <IconChevron size={12} />
        <span class="fold-label">Context compacted{note}</span>
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

/** The runtime's reply to a model refusal (SPEC 0.168): the service line of any runtime reply,
 *  and, while no message of the user stands after it in the feed, "Switch model" under its
 *  text. It opens the model menu (`ChatActions.modelSwitch`), the session's model checked and
 *  without Claude Code's default: a running agent is not switched to it, it takes it at its
 *  next start. The checked entry itself (an alias, or the version in its family's submenu) only
 *  closes the menu; a family checked for one of its versions switches to its alias. Once another
 *  model went through, the line says so in the button's place. */
function RefusalLine({ event, actions }: { event: EventOf<'agent.message'>; actions: ChatActions }) {
  const ms = actions.modelSwitch;
  const phone = useIsPhone();
  const [open, setOpen] = useState<{ x: number; y: number; anchor: HTMLElement; custom?: boolean } | null>(null);
  // The model switched to, and the last `seq` in the feed when it was picked.
  const [switched, setSwitched] = useState<{ to: string; after: number } | null>(null);
  // By the place in the feed, not by `seq`: an input that waited through this turn stands under
  // the line though it came before it.
  const items = actions.timeline.items();
  const at = items.findIndex((i) => i.kind === 'event' && i.event.seq === event.seq);
  const answered = items.slice(at + 1).some((i) => i.kind === 'event' && i.event.type === 'user.message');
  // A switch the runtime then refused (SPEC 0.179: `system model_not_switched`) offers again.
  const shown =
    switched &&
    !items.some(
      (i) =>
        i.kind === 'event' &&
        i.event.type === 'system' &&
        i.event.payload.code === 'model_not_switched' &&
        (i.event.seq ?? 0) > switched.after,
    )
      ? switched.to
      : null;
  if (!ms || answered) return <Line kind="system line-runtime">{event.payload.text}</Line>;
  const pick = (c: ModelChoice) => {
    if ('provider' in c ? c.provider === ms.provider : !ms.provider && c.model === ms.model) return;
    const to = 'provider' in c ? ms.providers.find((p) => p.id === c.provider)!.label : modelName(c.model!);
    const after = items.reduce((m, i) => (i.kind === 'event' && (i.event.seq ?? 0) > m ? i.event.seq! : m), 0);
    void ms.to(c).then((ok) => ok && setSwitched({ to, after }));
  };
  let menu: ComponentChildren = null;
  if (open) {
    const at = { x: open.x, y: open.y, above: true, anchor: open.anchor, backdrop: phone, onClose: () => setOpen(null) };
    menu = open.custom ? (
      <ContextMenu {...at} label="Model">
        <ModelName
          value=""
          onKeyDown={(e) => {
            if (e.key !== 'Enter' || e.isComposing) return;
            e.preventDefault();
            const m = (e.currentTarget as HTMLInputElement).value.trim();
            if (!m) return;
            setOpen(null);
            pick({ model: m });
          }}
        />
      </ContextMenu>
    ) : (
      <ContextMenu
        {...at}
        label="Model"
        items={modelMenuItems({
          model: ms.model,
          provider: ms.provider,
          providers: ms.providers,
          withDefault: false,
          onPick: pick,
          onAnother: () => setOpen({ ...open, custom: true }),
        })}
      />
    );
  }
  return (
    <>
      <Line kind="system line-runtime line-refusal">
        {event.payload.text}
        <span class="refusal-offer" role="status">
          {shown ? (
            `Switched to ${shown}. Send the message again.`
          ) : (
            <button
              type="button"
              class="btn btn-small btn-outline btn-switch-model"
              aria-haspopup="menu"
              aria-expanded={!!open}
              onClick={(e) => {
                if (open) return setOpen(null);
                const el = e.currentTarget as HTMLElement;
                const r = el.getBoundingClientRect();
                setOpen({ x: r.left, y: r.top - 6, anchor: el });
              }}
            >
              Switch model
            </button>
          )}
        </span>
      </Line>
      {menu}
    </>
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

/** A bubble's note ("Joined the turn"): an icon with the words as its tooltip and its name. */
interface BubbleNote {
  label: string;
  icon: ComponentChildren;
}

const JOINED: BubbleNote = { label: 'Joined the turn', icon: <IconJoined /> };
const INSTEAD: BubbleNote = { label: 'Instead of the request', icon: <IconReply /> };

/** The user's bubble (a plate in one column) with its time and, when there is one, its note:
 *  an icon right before the time, in the time's line, so it takes an icon's room. */
function Bubble({
  ts,
  note,
  time,
  buttons,
  children,
}: {
  ts: string;
  note?: BubbleNote;
  time?: ComponentChildren;
  /** Shown on hover left of the time (a computer's; a phone has them under the message). */
  buttons?: ComponentChildren;
  children: ComponentChildren;
}) {
  return (
    <div class={['msg-bubble', buttons && 'msg-bubble-acts'].filter(Boolean).join(' ')}>
      <MessageTime
        ts={ts}
        before={[
          buttons && <span class="msg-time-actions">{buttons}</span>,
          note && (
            <span class="msg-note" role="img" aria-label={note.label} title={note.label}>
              {note.icon}
            </span>
          ),
        ]}
      >
        {time}
      </MessageTime>
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

function UserMessage({ event, actions, queued }: { event: EventOf<'user.message'>; actions: ChatActions; queued?: boolean }) {
  const d = event.delivery;
  const ids = event.payload.artifact_ids;
  const fork = actions.fork && event.author.kind === 'user' && d !== 'pending' && d !== 'failed';
  // The hub copies the history before it answers, seconds for a long session (SPEC §6).
  const forking = fork && !!actions.forking?.(event.event_id);
  const phone = useIsPhone();
  const buttons = (event.payload.text || fork) && [
    event.payload.text && <CopyButton key="copy" text={event.payload.text} icon />,
    fork && (
      <button
        key="fork"
        type="button"
        class="btn-copy btn-copy-idle btn-fork"
        disabled={forking}
        aria-label={forking ? 'Forking…' : 'Fork from here'}
        title={forking ? 'Forking…' : 'Fork from here: a new session with the conversation before this message'}
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
        note={event.payload.joined ? JOINED : undefined}
        time={[
          event.payload.schedule_id && <span class="scheduled"> · scheduled</span>,
          d === 'pending' && <span class="delivery"> · {queued ? 'queued' : 'awaiting delivery'}</span>,
          d === 'failed' && <span class="delivery delivery-failed"> · not delivered</span>,
          forking && (
            <span class="forking" role="status">
              {' · forking…'}
            </span>
          ),
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
      <Bubble ts={event.ts} note={INSTEAD}>
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
  // An interrupt cut it off: muted, marked after its time.
  const interrupted = event.payload.interrupted;
  return (
    <article class={`msg-agent ${event.payload.final ? 'msg-final' : 'msg-interim'}${interrupted ? ' msg-interrupted' : ''}`}>
      <MessageTime ts={event.ts}>{interrupted && <span class="interrupted"> · interrupted</span>}</MessageTime>
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
        {/* Cut where the line ends: 2000 characters are wider than any window. */}
        <span class="thinking-peek">Thinking · {oneLine(event.payload.text.slice(0, 2000), 2000)}</span>
      </summary>
      {/* The feed's own font, no box: prose, as in the VS Code tab. */}
      <div class="thinking-text">{event.payload.text}</div>
    </details>
  );
}

/** A run of tool calls, collapsed: "N tool calls · time", then the call's line and its result
 *  when it is one, else the tools it called, each once, in the order first called; open, a row
 *  per call (T26). Under it, a line with the page each of its `Artifact` calls published. */
function ToolGroup({ block, actions }: { block: Extract<Block, { kind: 'tools' }>; actions: ChatActions }) {
  const [open, setOpen] = useState(false);
  const calls = callRows(block.rows);
  const state = groupState(block.rows);
  const ms = groupDuration(block.rows);
  const n = count(calls.length, 'tool call');
  const stats = ms !== undefined ? `${n} · ${formatDuration(ms)}` : n;
  const one = calls.length === 1 ? calls[0].call : undefined;
  const peek = one && callPeek(one.payload.tool, one.payload.args_preview, actions.workspaceRoot);
  const peekResult =
    one &&
    resultLine(
      one.payload.tool,
      one.payload.args_preview,
      (actions.timeline.resultFor(one.payload.call_id) ?? calls[0].result)?.payload,
    );
  const kinds = [...new Set(calls.flatMap((r) => (r.call ? [toolKind(r.call.payload.tool)] : [])))];
  // The pages its `Artifact` calls published: a line each under the run, folded or not.
  const pages = calls.flatMap(({ call, result }) => {
    const r = call ? (actions.timeline.resultFor(call.payload.call_id) ?? result) : undefined;
    const page = call && artifactPage(call.payload.tool, r?.payload);
    const href = page && safeHref(page);
    return href ? [href] : [];
  });
  // One page opened twice in the run is one line.
  const unique = [...new Set(pages)];
  return (
    <>
      <div class={`tool-group tool-${state} ${open ? 'is-open' : ''}`}>
        <button type="button" class="tool-group-head" aria-expanded={open} onClick={() => setOpen(!open)}>
          <span class="chevron">
            <IconChevron />
          </span>
          <StateMark state={state} />
          <span class="tool-stats">{stats}</span>
          {peek ? (
            <>
              <span class="tool-peek mono">{peek}</span>
              {peekResult && <span class="tool-result mono">{relativePaths(peekResult, actions.workspaceRoot)}</span>}
            </>
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
      {unique.map((href) => (
        <div key={href} class="tool-artifact">
          <span class="tool-artifact-label">Artifact</span>{' '}
          <a href={href} target="_blank" rel="noopener noreferrer">
            {href}
          </a>
        </div>
      ))}
    </>
  );
}

function StateMark({ state }: { state: 'running' | 'unfinished' | 'error' | 'ok' }) {
  if (state === 'running') return <span class="pulse-dot" role="img" aria-label="running" />;
  if (state === 'unfinished')
    return (
      <span class="mark mark-error" role="img" aria-label="not finished">
        <IconCross size={13} />
      </span>
    );
  return (
    <span class={`mark mark-${state}`} role="img" aria-label={state === 'ok' ? 'done' : 'error'}>
      {state === 'ok' ? <IconCheck size={13} /> : <IconCross size={13} />}
    </span>
  );
}

/** One call: `$ command` for Bash, else the tool and its main argument, paths under the
 *  session's folder relative to it, then its result; open, the change of an Edit, a MultiEdit or
 *  a Write (its result only if it failed), else the arguments and the result. */
function ToolRowView({ row, actions }: { row: ToolRow; actions: ChatActions }) {
  if (row.kind === 'thinking') return <Thinking event={row.event} />;
  const { call } = row;
  const result = call ? actions.timeline.resultFor(call.payload.call_id) ?? row.result : row.result;
  const r = result?.payload;
  const state = !r ? (row.unfinished ? 'unfinished' : 'running') : r.error ? 'error' : 'ok';
  const line = call ? callLine(call.payload.tool, call.payload.args_preview) : undefined;
  const shown = call && resultLine(call.payload.tool, call.payload.args_preview, r);
  // Claude Code reports no time of a call, so the worker sends no `duration_ms`: from the call to
  // its result, as the group's time is counted.
  const ms = r?.duration_ms ?? (call && result ? between(call.ts, result.ts) : undefined);
  return (
    <details class={`tool-row tool-${state}`}>
      <summary>
        {line?.shell ? (
          <span class="tool-prompt">$</span>
        ) : (
          <span class="tool-name">{call ? toolLabel(call.payload.tool) : 'Tool result'}</span>
        )}
        <span class="tool-text">
          <span class="tool-line mono">
            {line ? relativePaths(line.text, actions.workspaceRoot) : oneLine(r?.result_preview ?? '', 80)}
          </span>
          {shown && <span class="tool-result mono">{relativePaths(shown, actions.workspaceRoot)}</span>}
        </span>
        {line?.more && (
          // "+N lines"; a cut preview counts only the lines it carries: "+N+ lines", at least that many.
          <span class="tool-more">
            +{line.more}
            {line.cut ? '+' : ''} {line.more === 1 && !line.cut ? 'line' : 'lines'}
          </span>
        )}
        {ms !== undefined && <span class="tool-time">{formatDuration(ms)}</span>}
        <StateMark state={state} />
      </summary>
      <ToolCallBody call={call} result={result} actions={actions} />
    </details>
  );
}

/** An opened call, in its row of a group or on a subagent's card: the change of an Edit, a
 *  MultiEdit or a Write (its result only if it failed), else the arguments and the result, its
 *  addresses as links and its images as thumbnails. */
function ToolCallBody({
  call,
  result,
  actions,
}: {
  call?: EventOf<'tool.call'>;
  result?: EventOf<'tool.result'>;
  actions: ChatActions;
}) {
  const r = result?.payload;
  // An Edit, a MultiEdit or a Write shows its change, as its permission card does (SPEC §10.1).
  const change = call ? fileChange(call.payload.tool, call.payload.args_preview) : undefined;
  const args = call && !change ? argsText(call.payload.args_preview) : undefined;
  return (
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
            <pre class="pre">{args?.text ?? call.payload.args_preview}</pre>
            {args?.cut && <div class="tool-cut muted">The preview is cut at 4 KB: the rest of the arguments are not shown.</div>}
          </>
        )
      )}
      {/* A change that went through says only that it did: its result is not shown. */}
      {r && (r.error || !change) && (
        <>
          <div class="label">{r.error ? 'Error' : 'Result'}</div>
          {r.error_text && <div class="error-text">{r.error_text}</div>}
          {r.result_preview && <pre class="pre">{withLinks(r.result_preview)}</pre>}
          {r.artifact_ids.length > 0 && (
            <div class="attachments tool-images">
              {r.artifact_ids.map((id) => (
                <AttachmentView key={id} id={id} kind="image" actions={actions} />
              ))}
            </div>
          )}
          {r.artifact_id && (
            <button type="button" class="btn btn-small" onClick={() => actions.openArtifact(r.artifact_id!)}>
              Full result
            </button>
          )}
        </>
      )}
    </div>
  );
}

/** An address found in text without what closes the sentence around it: trailing punctuation,
 *  and a closing bracket only when the address has no opening one for it, so
 *  `…/wiki/Rust_(programming_language)` keeps its `)` and `[x](https://a.b/c)` loses it. */
function trimUrl(found: string): string {
  const pairs: Record<string, string> = { ')': '(', ']': '[', '}': '{' };
  let url = found;
  for (;;) {
    const t = url.replace(/[.,;:!?]+$/, '');
    const close = t.at(-1) ?? '';
    const open = pairs[close];
    if (open && t.split(open).length < t.split(close).length) url = t.slice(0, -1);
    else return t;
  }
}

/** `text` with its http and https addresses as links that open in a new tab (SPEC §6). */
function withLinks(text: string): ComponentChildren {
  const out: ComponentChildren[] = [];
  let last = 0;
  for (const m of text.matchAll(/https?:\/\/[^\s<>"'`]+/g)) {
    const url = trimUrl(m[0]);
    const href = safeHref(url);
    if (!href) continue;
    out.push(
      text.slice(last, m.index),
      // Labelled with the address it opens: a look-alike host shows in its punycode form.
      <a key={m.index} href={href} target="_blank" rel="noopener noreferrer">
        {href}
      </a>,
    );
    last = m.index + url.length;
  }
  out.push(text.slice(last));
  return out;
}

/** A subagent the agent started (its `Agent` call): a card of its own in the project's colour,
 *  not a row of a tool group, two lines that never wrap, a long text cut. The head: the agent
 *  icon, "Agent started:", the task, the subagent's type (not on a screen 480 px wide or less)
 *  and whether it was sent to the background; under it the subagent's state: running ("Running", or what it does now) with its
 *  tokens, tool calls and run time (ticking), in the background, done with its run time, failed
 *  with the error, stopped. The head opens the call's arguments and result. */
function SubagentCard({ block, actions }: { block: Extract<Block, { kind: 'subagent' }>; actions: ChatActions }) {
  const [open, setOpen] = useState(false);
  const { call, result } = block;
  const s = subagentState(call, result, actions.timeline);
  const t = useNow(1000, s.state === 'running', actions.now);
  const args = callArgs(call.payload.args_preview);
  const type = typeof args.subagent_type === 'string' ? args.subagent_type : '';
  const task = typeof args.description === 'string' ? args.description : '';
  let status: ComponentChildren;
  switch (s.state) {
    case 'running':
      status = (
        <>
          <span class="pulse-dot" aria-hidden="true" />
          {/* What it does now says it runs: the CLI's often starts "Running …" itself. */}
          {s.live?.activity ? (
            <span class="subagent-activity">{s.live.activity}</span>
          ) : (
            <span class="subagent-state">Running</span>
          )}
          <span class="subagent-stats">
            {s.live
              ? stats(s.live, s.live.duration_ms + Math.max(0, t - actions.timeline.liveAgentsAt))
              : elapsed(t - Date.parse(call.ts))}
          </span>
        </>
      );
      break;
    case 'background':
      status = <span class="subagent-state">In background</span>;
      break;
    case 'done':
      status = (
        <>
          <StateMark state="ok" />
          <span class="subagent-state">{s.ms !== undefined ? `Done · ${elapsed(s.ms)}` : 'Done'}</span>
        </>
      );
      break;
    case 'failed':
      status = (
        <>
          <StateMark state="error" />
          <span class="subagent-state">Failed</span>
          {result?.payload.error_text && <span class="subagent-error">{result.payload.error_text}</span>}
        </>
      );
      break;
    case 'stopped':
      status = <span class="subagent-state">Stopped</span>;
      break;
  }
  return (
    <section class={`subagent subagent-${s.state} ${open ? 'is-open' : ''}`} aria-label="Subagent">
      <button type="button" class="subagent-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span class="subagent-icon">
          <IconAgent />
        </span>
        <span class="subagent-label">Agent started:</span>
        {task && <span class="subagent-task">{task}</span>}
        {type && <span class="subagent-type">{type}</span>}
        {args.run_in_background === true && <span class="subagent-chip">background</span>}
        <span class="chevron">
          <IconChevron />
        </span>
      </button>
      <div class="subagent-status">{status}</div>
      {open && <ToolCallBody call={call} result={result} actions={actions} />}
    </section>
  );
}

/** Where a subagent ended (its notification, or the result of one that ran in the
 *  foreground): a lighter card of the start card's family, two lines that never wrap. The
 *  head: the state's mark, "Agent finished:", the task, "done", "failed" or "stopped", the
 *  tokens and tool calls its notification's `usage` says (since 0.181) and, on the call's first
 *  end, the run time from the call; under it the end's text on one line, without Markdown
 *  marks; the head opens the whole text as Markdown. */
function SubagentEndCard({ block }: { block: Extract<Block, { kind: 'subagent-end' }> }) {
  const [open, setOpen] = useState(false);
  const e = subagentEnd(block.call, block.end);
  const spent = stats((block.end.type === 'system' && block.end.payload.usage) || {}, block.first ? e.ms : undefined);
  const args = callArgs(block.call.payload.args_preview);
  const task = typeof args.description === 'string' ? args.description : '';
  // A report runs to tens of KB; its first 2000 characters are wider than any window.
  const peek = useMemo(() => markdownLine(e.text.slice(0, 2000)), [e.text]);
  return (
    <section class={`subagent subagent-end subagent-${e.state} ${open ? 'is-open' : ''}`} aria-label="Subagent ended">
      <button type="button" class="subagent-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span class="subagent-icon">
          {e.state === 'stopped' ? (
            <span class="mark mark-no" role="img" aria-label="stopped">
              <IconStop size={10} />
            </span>
          ) : (
            <StateMark state={e.state === 'done' ? 'ok' : 'error'} />
          )}
        </span>
        <span class="subagent-label">Agent finished:</span>
        {task && <span class="subagent-task">{task}</span>}
        <span class="subagent-state">{e.state}</span>
        {spent && <span class="subagent-stats">{spent}</span>}
        <span class="chevron">
          <IconChevron />
        </span>
      </button>
      {open ? <Markdown class="subagent-end-text" text={e.text} /> : <div class="subagent-end-peek">{peek}</div>}
    </section>
  );
}

/** "Other" of a question with options: whether it is chosen, and its text. */
interface OtherAnswer {
  on: boolean;
  text: string;
}

/** The agent's questions (AskUserQuestion), as the tab asks them: a question's header, each
 *  option's description under its label, checkboxes for a multi question, and "Other" whose
 *  text is the answer (with multi, beside the checked labels). An MCP server's request
 *  (`server`) is headed by the server, has its text above the questions, its link with Done in
 *  place of questions, no "Other", its required fields marked, and Decline. */
export function ClarifyCard({ event, actions }: { event: EventOf<'clarify.request'>; actions: ChatActions }) {
  const p = event.payload;
  const fate = actions.timeline.requestFate(p.request_id);
  // Questions a worker restart or Stop cancelled are answered by a message, as the VS Code tab
  // offers an earlier question again (`web:161989`), until a message of the user stands after
  // the cancel: that message, when it is the answer (`web:162093`), says what was chosen; any
  // other closes the card. Any other cancel closes it at once.
  // An MCP server's request is not: a message goes to the agent, not to the server.
  const earlier =
    p.server === undefined &&
    !fate.response &&
    (fate.cancel?.reason === 'worker_restart' || fate.cancel?.reason === 'stopped');
  const after = earlier ? actions.timeline.userMessageAfter(fate.cancelSeq!) : undefined;
  const replayed = after && earlierAnswers(after.text.trim(), p.questions);
  // An answer whose message failed to be sent is not given yet.
  const answeredLater = !!replayed && !after!.failed;
  const later = earlier && !after && !!actions.send;
  const open = (!fate.response && !fate.cancel) || later;
  const [answers, setAnswers] = useState<Record<string, ClarifyAnswer>>({});
  const [others, setOthers] = useState<Record<string, OtherAnswer>>({});
  const { busy, error, run } = useAction();
  const given = fate.response && 'answers' in fate.response ? fate.response.answers : replayed;
  const declined = !!fate.response && 'action' in fate.response && fate.response.action === 'decline';
  const href = p.url ? safeHref(p.url) : null;

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
  // An MCP server's form waits for what its schema says, which a server on the MCP SDK checks
  // the answer against: every required field answered, an integer or a number that reads as one
  // to the worker too (a safe integer; a decimal number, not `0x10`).
  const fits = p.questions.every((q) => {
    const v = toSend[q.id];
    const t = (Array.isArray(v) ? v.join(', ') : (v ?? '')).trim();
    if (t === '') return !q.required;
    if (q.input === 'integer') return /^-?\d+$/.test(t) && Number.isSafeInteger(Number(t));
    if (q.input === 'number') return /^-?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/.test(t) && Number.isFinite(Number(t));
    return true;
  });
  const ready = p.server === undefined ? complete : fits;
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
        if (later) {
          const text = earlierAnswer(p.questions, collect());
          if (complete && text !== undefined) actions.send!(text);
        } else if (open && ready) {
          run(() => (p.server === undefined ? actions.answer(p.request_id, toSend) : actions.answer(p.request_id, toSend, 'accept')));
        }
      }}
    >
      <div class="card-title">{p.server === undefined ? `${asker(p.subagent, 'Agent')} asks` : `${p.server} asks`}</div>
      {p.message && <div class="clarify-message">{p.message}</div>}
      {p.url && (
        <div class="clarify-link">
          {href ? (
            <a href={href} target="_blank" rel="noopener noreferrer">
              {href}
            </a>
          ) : (
            p.url
          )}
        </div>
      )}
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
              {(q.header || q.required) && (
                <span class="question-header">{[q.header, q.required && 'required'].filter(Boolean).join(' · ')}</span>
              )}
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
            {!!q.options?.length && p.server === undefined && (
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
          <button
            type="submit"
            class="btn btn-primary"
            disabled={busy || (later && earlierAnswer(p.questions, collect()) === undefined) || !ready}
          >
            {p.url ? 'Done' : 'Answer'}
          </button>
          {p.server !== undefined && (
            <button type="button" class="btn btn-ghost" disabled={busy} onClick={() => run(() => actions.answer(p.request_id, {}, 'decline'))}>
              Decline
            </button>
          )}
        </div>
      )}
      {(fate.response || answeredLater) && <div class="outcome">{declined ? 'Declined' : p.url ? 'Done' : 'Answers sent'}</div>}
      {fate.cancel && !answeredLater && <div class="outcome">Cancelled: {CANCEL_LABELS[fate.cancel.reason]}</div>}
      {error && <div class="error-text">{error}</div>}
    </form>
  );
}

/** The message the VS Code tab sends for an earlier question it shows again (`web:162062`):
 *  the questions answered, in their order, the items of a multi answer joined by ", ";
 *  undefined when none is, as the tab then sends nothing. */
function earlierAnswer(questions: ClarifyQuestion[], answers: Record<string, ClarifyAnswer>): string | undefined {
  const given = questions.flatMap((q) => {
    const a = answers[q.id];
    const text = Array.isArray(a) ? a.join(', ') : a;
    return text ? [{ question: q.text, answer: text }] : [];
  });
  if (given.length === 0) return undefined;
  if (given.length === 1) return `${EARLIER_ONE}${given[0].question}": ${given[0].answer}`;
  return [EARLIER_SEVERAL, ...given.map((g) => `- "${g.question}": ${g.answer}`)].join('\n');
}

const EARLIER_ONE = 'Answering your earlier question "';
const EARLIER_SEVERAL = 'Answering your earlier questions:';

/** The answers a message in those words gives, read by the card's question texts as the tab
 *  reads them (`web:162093`), the items of a `multi` answer split at ", "; undefined when the
 *  message answers none of them. */
function earlierAnswers(text: string, questions: ClarifyQuestion[]): Record<string, ClarifyAnswer> | undefined {
  const texts = questions.map((q) => q.text);
  const found = new Map<string, string>();
  if (text.startsWith(EARLIER_ONE)) {
    // The longest question that fits: one may begin with another.
    let best: { question: string; marker: string } | undefined;
    for (const q of texts) {
      const marker = `${EARLIER_ONE}${q}": `;
      if (text.startsWith(marker) && (!best || marker.length > best.marker.length)) best = { question: q, marker };
    }
    if (best) found.set(best.question, text.slice(best.marker.length));
  } else if (text.startsWith(`${EARLIER_SEVERAL}\n`)) {
    const lines = text.slice(EARLIER_SEVERAL.length + 1);
    let at = 0;
    for (let i = 0; i < texts.length; i++) {
      const marker = `- "${texts[i]}": `;
      if (!lines.startsWith(marker, at)) continue;
      const from = at + marker.length;
      let to = lines.length;
      for (let j = i + 1; j < texts.length; j++) {
        const next = lines.indexOf(`\n- "${texts[j]}": `, from);
        if (next !== -1 && next < to) to = next;
      }
      found.set(texts[i], lines.slice(from, to));
      at = to + 1;
    }
  }
  const out: Record<string, ClarifyAnswer> = {};
  for (const q of questions) {
    const a = found.get(q.text);
    if (a !== undefined) out[q.id] = q.multi ? a.split(', ') : a;
  }
  return Object.keys(out).length ? out : undefined;
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
