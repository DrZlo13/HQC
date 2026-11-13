// What the feed draws, built from the timeline's items (T26): runs of tool calls fold into
// one group, and every agent message stays a message of its own (the agent narrates in its
// thinking, which goes into the group; what it writes is for the user); the `usage` after a
// final answer goes into the answer's meta line; a `tool.call` followed at once by the
// `approval.request` for it is shown by the request card, not as a row of its own. Messages
// between sessions (T51) are blocks of their own: the agent's `hqc_send` / `hqc_ask` call and
// the ask's result, and the final message answering a question with the attachments and sends
// of its turn. A subagent the agent starts (its `Agent` call) is a block of its own too, with
// its result and its state, and so is its end: its notification, or its result when it ran in
// the foreground. Pure logic, no DOM.

import type { EventOf, HqcEvent, LiveSubagent } from '../proto';
import type { Item, Timeline } from '../timeline';
import { oneLine } from './labels';

export type ToolRow =
  | { kind: 'call'; key: string; call?: EventOf<'tool.call'>; result?: EventOf<'tool.result'> }
  | { kind: 'thinking'; key: string; event: EventOf<'agent.thinking'> };

/** An `hqc_send` / `hqc_ask` call of this session's agent and its result (T51). */
export interface PeerCall {
  call: EventOf<'tool.call'>;
  result?: EventOf<'tool.result'>;
}

export type Block =
  | { kind: 'item'; key: string; item: Item }
  | { kind: 'agent'; key: string; event: EventOf<'agent.message'>; usage?: EventOf<'usage'> }
  | { kind: 'tools'; key: string; rows: ToolRow[] }
  | { kind: 'approval'; key: string; event: EventOf<'approval.request'>; call?: EventOf<'tool.call'> }
  /** A subagent: a call of `Agent` (`Task` in older Claude Code) and its result. */
  | { kind: 'subagent'; key: string; call: EventOf<'tool.call'>; result?: EventOf<'tool.result'> }
  /** Where a subagent ended: its `task_notification`, or the result of one that ran in the
   *  foreground (`subagentEnd`); `first`: the call's first end, the only one that shows the run
   *  time (a later one, a SendMessage that continued the subagent, would count from the call). */
  | {
      kind: 'subagent-end';
      key: string;
      call: EventOf<'tool.call'>;
      end: EventOf<'system'> | EventOf<'tool.result'>;
      first: boolean;
    }
  /** "↗ Sent to X": a call of `hqc_send` / `hqc_ask`. */
  | ({ kind: 'sent'; key: string } & PeerCall)
  /** "↙ Reply from X": the result of an `hqc_ask`. */
  | { kind: 'reply-from'; key: string; call: EventOf<'tool.call'>; result: EventOf<'tool.result'> }
  /** "↗ Reply to X": the final message answering a question, with the turn's attachments and
   *  the sends that went with it (T42). */
  | {
      kind: 'reply-to';
      key: string;
      event: EventOf<'agent.message'>;
      question: EventOf<'peer.message'>;
      files: EventOf<'attachment'>[];
      sends: PeerCall[];
      usage?: EventOf<'usage'>;
    };

/** Events the feed does not draw at all: `status` but an error with a reason (SPEC §17.1). */
function hidden(e: HqcEvent): boolean {
  return e.type === 'status' && (e.payload.state !== 'error' || !e.payload.detail);
}

/** What the exchanges between sessions take out of the feed's order (T51). */
interface Relays {
  /** Final messages that answer a question of this session, by their `seq`. */
  replies: Map<number, Extract<Block, { kind: 'reply-to' }>>;
  /** Events drawn inside a reply: its attachments and the sends that went with it. */
  inside: Set<number>;
  /** Messages in the asker that went with a reply (T42) and are drawn inside it. */
  folded: Set<number>;
}

function relays(items: Item[], timeline: Timeline): Relays {
  const evs = items.flatMap((it) => (it.kind === 'event' ? [it.event] : []));
  // Questions by `event_id`, with their place in `evs` (sorted by `seq`).
  const questions = new Map<string, number>();
  evs.forEach((e, i) => e.type === 'peer.message' && e.payload.mode === 'ask' && questions.set(e.event_id, i));
  const r: Relays = { replies: new Map(), inside: new Set(), folded: new Set() };
  evs.forEach((e, i) => {
    if (e.type !== 'agent.message' || !e.payload.final || !e.payload.in_reply_to) return;
    const qi = questions.get(e.payload.in_reply_to);
    const question = qi === undefined ? undefined : evs[qi];
    if (question?.type !== 'peer.message' || qi! > i) return;
    const block: Extract<Block, { kind: 'reply-to' }> = { kind: 'reply-to', key: `e${e.seq}`, event: e, question, files: [], sends: [] };
    // What the hub gives the asker with the reply (SPEC §7.1): the turn's attachments and its
    // sends to the asker that went `with_reply`.
    for (const x of evs.slice(qi! + 1, i)) {
      if (x.type === 'attachment') {
        block.files.push(x);
        r.inside.add(x.seq!);
      } else if (x.type === 'tool.call' && hqcTool(x.payload.tool) === 'send') {
        const result = timeline.resultFor(x.payload.call_id);
        if (sendResult(result)?.delivery !== 'with_reply') continue;
        block.sends.push({ call: x, result });
        r.inside.add(x.seq!);
      }
    }
    r.replies.set(e.seq!, block);
  });
  // In the asker, a send that went with a reply is also a `peer.message` without `delivery`:
  // the reply's block shows its text after the separator.
  const joined = new Set<string>();
  for (const e of evs) {
    if (e.type !== 'tool.result' || e.payload.error) continue;
    const call = timeline.callFor(e.payload.call_id);
    if (!call || hqcTool(call.payload.tool) !== 'ask') continue;
    const res = askResult(e);
    for (const part of splitReply(res.reply).slice(1)) joined.add(`${res.from}\n${part}`);
  }
  if (joined.size) {
    for (const e of evs) {
      if (e.type === 'peer.message' && e.payload.mode === 'send' && !e.delivery && joined.has(`${e.payload.from}\n${e.payload.text}`))
        r.folded.add(e.seq!);
    }
  }
  return r;
}

export function buildFeed(items: Item[], timeline: Timeline): Block[] {
  const out: Block[] = [];
  let group: Extract<Block, { kind: 'tools' }> | null = null;
  const close = () => (group = null);
  const rel = relays(items, timeline);
  // The `Agent` calls of background launches by the `agentId` their results name.
  const launches = new Map<string, EventOf<'tool.call'>>();
  for (const it of items) {
    if (it.kind !== 'event' || it.event.type !== 'tool.result') continue;
    const call = timeline.callFor(it.event.payload.call_id);
    const agentId = launchedAgent(it.event);
    if (call && subagentTool(call.payload.tool) && agentId) launches.set(agentId, call);
  }
  // The calls whose subagent has an end card already.
  const ended = new Set<string>();
  const end = (key: string, call: EventOf<'tool.call'>, e: EventOf<'system'> | EventOf<'tool.result'>) => {
    close();
    out.push({ kind: 'subagent-end', key, call, end: e, first: !ended.has(call.payload.call_id) });
    ended.add(call.payload.call_id);
  };

  for (const it of items) {
    if (it.kind !== 'event') {
      close();
      out.push({ kind: 'item', key: it.key, item: it });
      continue;
    }
    const e = it.event;
    if (hidden(e) || rel.inside.has(e.seq!) || rel.folded.has(e.seq!)) continue;
    switch (e.type) {
      case 'tool.call':
      case 'tool.result': {
        if (e.type === 'tool.call' && hqcTool(e.payload.tool)) {
          close();
          out.push({ kind: 'sent', key: it.key, call: e, result: timeline.resultFor(e.payload.call_id) });
          continue;
        }
        if (e.type === 'tool.call' && subagentTool(e.payload.tool)) {
          close();
          out.push({ kind: 'subagent', key: it.key, call: e, result: timeline.resultFor(e.payload.call_id) });
          continue;
        }
        if (e.type === 'tool.result') {
          const call = timeline.callFor(e.payload.call_id);
          if (call && hqcTool(call.payload.tool) === 'ask' && !e.payload.error) {
            close();
            out.push({ kind: 'reply-from', key: it.key, call, result: e });
            continue;
          }
          // A subagent that ran in the foreground ends here; a background launch's ends with
          // its notification.
          if (call && subagentTool(call.payload.tool) && !e.payload.result_preview.startsWith(LAUNCHED)) {
            end(it.key, call, e);
            continue;
          }
          // A result is drawn in its call's row.
          if (call) continue;
        }
        if (!group) {
          group = { kind: 'tools', key: it.key, rows: [] };
          out.push(group);
        }
        group.rows.push(
          e.type === 'tool.call'
            ? { kind: 'call', key: it.key, call: e, result: timeline.resultFor(e.payload.call_id) }
            : { kind: 'call', key: it.key, result: e },
        );
        continue;
      }
      case 'agent.thinking':
        if (group) {
          group.rows.push({ kind: 'thinking', key: it.key, event: e });
          continue;
        }
        break;
      case 'approval.request': {
        let g = group as Extract<Block, { kind: 'tools' }> | null;
        // The calls of one batch run together: a subagent's card or the card of a subagent's end
        // may stand between a call and its request; the group before them.
        for (let i = out.length - 1; !g && i >= 0; i--) {
          const b = out[i];
          if (b.kind === 'tools') g = b;
          else if (b.kind !== 'subagent' && b.kind !== 'subagent-end') break;
        }
        const last = g?.rows.at(-1);
        let call: EventOf<'tool.call'> | undefined;
        // Not a call whose result came before the request: the request is another's, such as
        // a subagent's (its calls are not in the feed, its requests are).
        const fits =
          last?.kind === 'call' &&
          last.call?.payload.tool === e.payload.tool &&
          !(last.result && last.result.seq! < e.seq!);
        if (g && last?.kind === 'call' && fits) {
          call = last.call;
          g.rows.pop();
          if (!g.rows.some((r) => r.kind === 'call')) out.splice(out.indexOf(g), 1);
        }
        close();
        out.push({ kind: 'approval', key: it.key, event: e, call });
        continue;
      }
      case 'usage': {
        // The session's total only (what background subagents spent between turns, SPEC
        // §8.1): the header counts it, the feed draws no `0 → 0 tokens` line for it.
        const u = e.payload;
        if (!u.input_tokens && !u.output_tokens && u.cost_usd === undefined) continue;
        const prev = out.at(-1);
        if (((prev?.kind === 'agent' && prev.event.payload.final) || prev?.kind === 'reply-to') && !prev.usage) {
          prev.usage = e;
          continue;
        }
        break;
      }
      case 'agent.message': {
        const reply = rel.replies.get(e.seq!);
        // A reply the runtime wrote itself is a service line, not the agent's message.
        if (e.payload.runtime && !reply) break;
        close();
        out.push(reply ?? { kind: 'agent', key: it.key, event: e });
        continue;
      }
      case 'system': {
        // A subagent's end, by its call or by the `agentId` of its background launch; any other
        // task's stays a "Background task" line.
        const p = e.payload;
        if (p.code !== 'task_notification' || !SUBAGENT_ENDS.has(p.status ?? '')) break;
        const byCall = p.call_id === undefined ? undefined : timeline.callFor(p.call_id);
        const call =
          byCall && subagentTool(byCall.payload.tool) ? byCall : p.task_id === undefined ? undefined : launches.get(p.task_id);
        if (!call) break;
        end(it.key, call, e);
        continue;
      }
    }
    close();
    out.push({ kind: 'item', key: it.key, item: it });
  }
  return out;
}

/** The tool call rows of a group. */
export function callRows(rows: ToolRow[]): Extract<ToolRow, { kind: 'call' }>[] {
  return rows.filter((r): r is Extract<ToolRow, { kind: 'call' }> => r.kind === 'call');
}

/** `running` while a call has no result, else `error` or `ok` by the last call. */
export function groupState(rows: ToolRow[]): 'running' | 'error' | 'ok' {
  const calls = callRows(rows);
  if (calls.some((r) => !r.result)) return 'running';
  return calls.at(-1)?.result!.payload.error ? 'error' : 'ok';
}

/** From the first call to the last result; the sum of `duration_ms` if the times say nothing. */
export function groupDuration(rows: ToolRow[]): number | undefined {
  const calls = callRows(rows);
  if (!calls.length || calls.some((r) => !r.result)) return undefined;
  const start = Math.min(...calls.map((r) => Date.parse((r.call ?? r.result!).ts)));
  const end = Math.max(...calls.map((r) => Date.parse(r.result!.ts)));
  if (Number.isFinite(start) && Number.isFinite(end) && end > start) return end - start;
  const sum = calls.reduce((n, r) => n + (r.result!.payload.duration_ms ?? 0), 0);
  return sum || undefined;
}

/** `Agent` (`Task` in older Claude Code): the tool that starts a subagent. */
export function subagentTool(tool: string): boolean {
  return tool === 'Agent' || tool === 'Task';
}

/** Where a subagent is: `running` (`live`: its row above the input, when the runtime lists
 *  it), `background` (sent or moved to the background, no end seen), `done`, `failed` or
 *  `stopped`; `ms`: how long it ran, when known. */
export interface SubagentState {
  state: 'running' | 'background' | 'done' | 'failed' | 'stopped';
  live?: LiveSubagent;
  ms?: number;
}

/** The `status` of a `task_notification` (SPEC §4) as the state it ends a subagent in; any
 *  other leaves the state to the call's result. */
const SUBAGENT_ENDS = new Map<string, SubagentState['state']>([
  ['completed', 'done'],
  ['failed', 'failed'],
  ['stopped', 'stopped'],
]);

/** The state of the subagent of `call`: running while `subagents.live` lists it; else as the
 *  first `system task_notification` of the call says it ended (one of the call by its
 *  `call_id`, else one with the `task_id` that the result of a background launch names as
 *  `agentId`: after a restart the CLI reports the subagent without the call; a later one, a
 *  SendMessage that continued the subagent, is not its end), after the call for as long as
 *  between them; else by its result: the CLI's answer for a subagent the user
 *  stopped ("[Request interrupted…") is stopped, another error failed, `run_in_background`
 *  or the CLI's answer for a subagent sent or moved to the background ("Async agent
 *  launched…") is in the background, any other is done, after the call for as long as between
 *  them (else the result's `duration_ms`); running without one. */
export function subagentState(
  call: EventOf<'tool.call'>,
  result: EventOf<'tool.result'> | undefined,
  timeline: Timeline,
): SubagentState {
  const id = call.payload.call_id;
  const live = timeline.liveAgents.find((a) => a.call_id === id);
  if (live) return { state: 'running', live };
  const end = timeline.taskEnd(id, launchedAgent(result));
  const ended = end && SUBAGENT_ENDS.get(end.payload.status ?? '');
  if (end && ended) return { state: ended, ms: between(call.ts, end.ts) };
  if (!result) return { state: 'running' };
  const r = result.payload;
  if (r.error) return { state: interrupted(r) ? 'stopped' : 'failed' };
  if (callArgs(call.payload.args_preview).run_in_background === true || r.result_preview.startsWith(LAUNCHED))
    return { state: 'background' };
  return { state: 'done', ms: between(call.ts, result.ts) ?? r.duration_ms };
}

/** The start of the CLI's answer for a subagent sent or moved to the background. */
const LAUNCHED = 'Async agent launched';

/** The `agentId` that the result of a background launch names. Only a launch's: the CLI names
 *  a finished foreground subagent too, and a later SendMessage that continues it ends with the
 *  same `task_id`. */
function launchedAgent(result: EventOf<'tool.result'> | undefined): string | undefined {
  const t = result?.payload.result_preview;
  return t?.startsWith(LAUNCHED) ? /^agentId: (\S+)/m.exec(t)?.[1] : undefined;
}

/** A finished foreground subagent's result without the trailer the CLI adds (2.1.312): from the
 *  last `agentId: <id> (use SendMessage …)` line that the `<usage>` block follows. */
function withoutTrailer(t: string): string {
  let at = -1;
  for (const m of t.matchAll(/(?:^|\n)agentId: [^\n]*\n<usage>/g)) at = m.index!;
  return at < 0 ? t : t.slice(0, at);
}

/** The CLI's answer for a subagent the user stopped. */
function interrupted(r: EventOf<'tool.result'>['payload']): boolean {
  return [r.error_text ?? '', r.result_preview].some((t) => t.startsWith('[Request interrupted'));
}

/** How a subagent ended at `end`: by its notification's `status`, or by its result (an error
 *  that says the request was interrupted is stopped, another failed, else done); `ms`: from
 *  the call to the end; `text`: the notification's text, or the result's (an error's
 *  `error_text`). */
export function subagentEnd(
  call: EventOf<'tool.call'>,
  end: EventOf<'system'> | EventOf<'tool.result'>,
): { state: 'done' | 'failed' | 'stopped'; ms?: number; text: string } {
  const ms = between(call.ts, end.ts);
  if (end.type === 'system') {
    const state = SUBAGENT_ENDS.get(end.payload.status ?? '');
    return { state: state === 'failed' || state === 'stopped' ? state : 'done', ms, text: end.payload.text };
  }
  const r = end.payload;
  if (!r.error) return { state: 'done', ms: ms ?? r.duration_ms, text: withoutTrailer(r.result_preview) };
  return { state: interrupted(r) ? 'stopped' : 'failed', ms: ms ?? r.duration_ms, text: r.error_text ?? r.result_preview };
}

/** From `from` to `to`; undefined when the times say nothing. */
function between(from: string, to: string): number | undefined {
  const ms = Date.parse(to) - Date.parse(from);
  return Number.isFinite(ms) && ms > 0 ? ms : undefined;
}

/** The interim message as a one-line caption. */
/** The arguments of a call: `args_preview` is the input as JSON, cut at 4 KB (SPEC §4), so a
 *  long one may not parse. */
export function callArgs(argsPreview: string): Record<string, unknown> {
  try {
    const v = JSON.parse(argsPreview);
    if (v && typeof v === 'object' && !Array.isArray(v)) return v as Record<string, unknown>;
  } catch {
    // Cut JSON: fish the string fields out.
    const out: Record<string, unknown> = {};
    for (const m of argsPreview.matchAll(/"(\w+)"\s*:\s*"((?:[^"\\]|\\.)*)"/g)) {
      try {
        out[m[1]] = JSON.parse(`"${m[2]}"`);
      } catch {
        out[m[1]] = m[2];
      }
    }
    // An `Agent` or `Task` call's `prompt` comes last (SPEC §8.1): the head before it, whole,
    // adds the fields that are not strings. In a string value a quote is escaped, so an
    // unescaped `,"prompt":` is the key.
    const at = argsPreview.indexOf(',"prompt":');
    if (at > 0)
      try {
        Object.assign(out, JSON.parse(argsPreview.slice(0, at) + '}'));
      } catch {
        // Not a whole head: the string fields only.
      }
    return out;
  }
  return {};
}

// `skill`: the Skill tool's skill name ("Skill worktree").
const BRIEF_KEYS = ['file_path', 'notebook_path', 'path', 'pattern', 'url', 'query', 'command', 'description', 'prompt', 'skill'];

/** One line for a call: `$ command` for Bash, else the tool and its main argument. */
export function callLine(tool: string, argsPreview: string): { shell: boolean; text: string; more?: number } {
  const args = callArgs(argsPreview);
  if (tool === 'Bash' && typeof args.command === 'string') {
    // A script (a heredoc, several commands) shows its first line and how many follow.
    const lines = args.command.trim().split('\n');
    const more = lines.slice(1).filter((l) => l.trim()).length;
    return { shell: true, text: oneLine(lines[0], 400), more: more || undefined };
  }
  const key = BRIEF_KEYS.find((k) => typeof args[k] === 'string' && args[k]);
  return { shell: false, text: key ? oneLine(String(args[key]), 400) : '' };
}

/** A tool as the Claude Code tab names it: an MCP tool `mcp__<server>__<tool>` as
 *  `<tool> (<server>)`, a plugin's server without its `plugin_<plugin>_` prefix
 *  (`mcp__plugin_chrome-devtools-mcp_chrome-devtools__take_screenshot` →
 *  `take_screenshot (chrome-devtools)`); any other tool as it is. */
export function toolLabel(tool: string): string {
  const m = MCP_TOOL.exec(tool);
  return m ? `${m[2]} (${m[1]})` : tool;
}

/** A tool in a group's list of the tools it called: an MCP tool as `<server>/<tool>`, any
 *  other as it is. */
export function toolKind(tool: string): string {
  const m = MCP_TOOL.exec(tool);
  return m ? `${m[1]}/${m[2]}` : tool;
}

const MCP_TOOL = /^mcp__(?:plugin_[^_]+_)?([^:\s]*)__([^:\s]+)$/;

/** `text` with the paths under `root` (the session's folder, absolute) relative to it: only
 *  where a path starts (at the start, after a space, a quote, `=` or `(`), not inside a
 *  longer one such as `/backup<root>/f`. */
export function relativePaths(text: string, root: string | undefined): string {
  const r = root?.replace(/\/+$/, '');
  if (!r) return text;
  const escaped = r.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return text.replace(new RegExp(`(^|[\\s"'\`=(])${escaped}/`, 'g'), '$1');
}

/** A call as one line of text, as the header of a group of one call shows it: `$ command`
 *  for Bash, else the tool and its main argument; paths under `root` relative to it. */
export function callPeek(tool: string, argsPreview: string, root: string | undefined): string {
  const line = callLine(tool, argsPreview);
  const text = relativePaths(line.text, root);
  return line.shell ? `$ ${text}` : [toolLabel(tool), text].filter(Boolean).join(' ');
}

/** A JSON text cut at some point, as `args_preview` is at 4 KB (SPEC §4), read as far as it
 *  goes: a string open at the cut ends there (a half escape dropped), a key without its value,
 *  a half number or literal and a trailing comma go, open arrays and objects close. `cut`: the
 *  text did not parse as it was; undefined if even the closed text does not parse. */
export function cutJson(s: string): { value: unknown; cut: boolean } | undefined {
  try {
    return { value: JSON.parse(s), cut: false };
  } catch {
    // Cut, or not JSON at all: closed below.
  }
  const closers: string[] = [];
  // Where the text can end: after the last whole value or opening bracket, with the brackets
  // open there; `tail` ends a string open at the cut.
  let safe = 0;
  let safeClosers: string[] = [];
  let tail = '';
  const mark = (end: number, quote = '') => {
    safe = end;
    safeClosers = [...closers];
    tail = quote;
  };
  // The last character outside strings that is not a space: says whether a string is a key.
  let prev = '';
  let i = 0;
  scan: while (i < s.length) {
    const c = s[i];
    if (c === '"') {
      const key = closers.at(-1) === '}' && (prev === '{' || prev === ',');
      let j = i + 1;
      while (s[j] !== '"') {
        // A string open at the cut: a value ends there, a key goes.
        if (j >= s.length) {
          if (!key) mark(j, '"');
          break scan;
        }
        if (s[j] !== '\\') {
          j++;
          continue;
        }
        const len = s[j + 1] === 'u' ? 6 : 2;
        if (j + len > s.length) {
          if (!key) mark(j, '"');
          break scan;
        }
        j += len;
      }
      i = j + 1;
      prev = '"';
      if (!key) mark(i);
    } else if (c === '{' || c === '[') {
      closers.push(c === '{' ? '}' : ']');
      prev = c;
      mark(++i);
    } else if (c === '}' || c === ']') {
      closers.pop();
      prev = c;
      mark(++i);
    } else if (c === ',' || c === ':') {
      prev = c;
      i++;
    } else if (/\s/.test(c)) {
      i++;
    } else {
      // A number or a literal is whole only if something follows it.
      const end = i + /^[^\s,:\]}]*/.exec(s.slice(i))![0].length;
      if (end >= s.length) break;
      prev = c;
      i = end;
      mark(i);
    }
  }
  try {
    return { value: JSON.parse(s.slice(0, safe) + tail + safeClosers.reverse().join('')), cut: true };
  } catch {
    return undefined;
  }
}

/** One edit as a diff: the lines both sides share at the start and at the end as context,
 *  the rest removed, then added. `cut`: a side may end mid-line, so nothing is matched. */
export function editDiff(before: string, after: string, cut = false): string {
  const a = before ? before.split('\n') : [];
  const b = after ? after.split('\n') : [];
  let head = 0;
  let foot = 0;
  if (!cut) {
    while (head < a.length && head < b.length && a[head] === b[head]) head++;
    while (foot < a.length - head && foot < b.length - head && a[a.length - 1 - foot] === b[b.length - 1 - foot]) foot++;
  }
  return [
    ...a.slice(0, head).map((l) => ` ${l}`),
    ...a.slice(head, a.length - foot).map((l) => `-${l}`),
    ...b.slice(head, b.length - foot).map((l) => `+${l}`),
    ...a.slice(a.length - foot).map((l) => ` ${l}`),
  ].join('\n');
}

/** A part of an Edit's, a MultiEdit's or a Write's change, in its permission card or its row
 *  of the feed: a diff of an edit, or a file's content; `title`, when set, names it in the
 *  code block's bar. */
export interface ChangePart {
  title?: string;
  code: string;
  diff: boolean;
}

const isRecord = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v);

/** What an Edit, a MultiEdit or a Write changes, from its call's `args_preview`: a diff
 *  per edit (`old_string` → `new_string`), the content of a Write. `cut`: the preview is cut,
 *  so the last part may end early and later ones are missing. Undefined for other tools and
 *  for arguments without a change. */
export function fileChange(tool: string, argsPreview: string): { parts: ChangePart[]; cut: boolean } | undefined {
  if (tool !== 'Edit' && tool !== 'MultiEdit' && tool !== 'Write') return undefined;
  const read = cutJson(argsPreview);
  if (!read || !isRecord(read.value)) return undefined;
  const args = read.value;
  const parts: ChangePart[] = [];
  if (tool === 'Write') {
    if (typeof args.content === 'string') parts.push({ title: 'Content', code: args.content, diff: false });
  } else {
    const edits = (tool === 'Edit' ? [args] : Array.isArray(args.edits) ? args.edits : [])
      .filter(isRecord)
      .filter((e) => typeof e.old_string === 'string' || typeof e.new_string === 'string');
    edits.forEach((e, i) => {
      // A cut preview may have lost later edits: their number is not known. An Edit with
      // `replace_all` changes every occurrence, which its one diff does not show by itself.
      const title =
        tool === 'Edit'
          ? e.replace_all === true
            ? 'All occurrences'
            : undefined
          : read.cut
            ? `Edit ${i + 1}`
            : `Edit ${i + 1} of ${edits.length}`;
      parts.push({ title, code: editDiff(str(e.old_string) ?? '', str(e.new_string) ?? '', read.cut), diff: true });
    });
  }
  return parts.length ? { parts, cut: read.cut } : undefined;
}

/** `send` / `ask` for the hqc tools `hqc_send` / `hqc_ask` (`mcp__hqc__hqc_ask` in Claude
 *  Code, SPEC §7), undefined for any other tool. */
export function hqcTool(tool: string): 'send' | 'ask' | undefined {
  for (const t of ['send', 'ask'] as const) if (tool === `hqc_${t}` || tool.endsWith(`__hqc_${t}`)) return t;
  return undefined;
}

/** For a call of `hqc_ask` (SPEC §7.1): the session it asked, '' if the arguments do not say;
 *  undefined for any other tool. */
export function askTarget(tool: string, argsPreview: string): string | undefined {
  if (hqcTool(tool) !== 'ask') return undefined;
  const s = callArgs(argsPreview).session;
  return typeof s === 'string' ? oneLine(s, 200) : '';
}

/** What a send or an ask took from the agent (SPEC §7): `text`, `session` and `files` (paths
 *  or artifact ids); a cut `args_preview` loses `files`. */
export function peerArgs(argsPreview: string): { text: string; session: string; files: string[] } {
  const a = callArgs(argsPreview);
  const files = Array.isArray(a.files) ? a.files.filter((f): f is string => typeof f === 'string') : [];
  return { text: typeof a.text === 'string' ? a.text : '', session: typeof a.session === 'string' ? a.session : '', files };
}

const str = (v: unknown) => (typeof v === 'string' ? v : undefined);
const num = (v: unknown) => (typeof v === 'number' ? v : undefined);

/** The result of `hqc_send` (`{to, delivery, message_seq}`, SPEC §7), undefined for none or an
 *  error. */
export function sendResult(r: EventOf<'tool.result'> | undefined): { to?: string; delivery?: string; message_seq?: number } | undefined {
  if (!r || r.payload.error) return undefined;
  const v = callArgs(r.payload.result_preview);
  return { to: str(v.to), delivery: str(v.delivery), message_seq: num(v.message_seq) };
}

/** A file of an ask's reply as the asking worker gives it (SPEC §7): the local `path` of the
 *  copy, or the `error` that stopped the download; `artifact_id` since T51. */
export interface ReplyFile {
  artifact_id?: string;
  name: string;
  kind?: string;
  path?: string;
  size?: number;
  error?: string;
}

/** The result of `hqc_ask` (`{from, reply, files?, message_seq}`, SPEC §7). A preview cut at
 *  4 KB keeps the whole strings before the cut; a reply cut itself is shown as the raw
 *  preview. */
export function askResult(r: EventOf<'tool.result'>): { from?: string; reply: string; files: ReplyFile[]; message_seq?: number } {
  const v = callArgs(r.payload.result_preview);
  const files = (Array.isArray(v.files) ? v.files : []).flatMap((f): ReplyFile[] => {
    if (!f || typeof f !== 'object') return [];
    const o = f as Record<string, unknown>;
    const name = str(o.name);
    if (!name) return [];
    return [{ artifact_id: str(o.artifact_id), name, kind: str(o.kind), path: str(o.path), size: num(o.size), error: str(o.error) }];
  });
  return { from: str(v.from), reply: str(v.reply) ?? r.payload.result_preview, files, message_seq: num(v.message_seq) };
}

/** Separates the final message of a reply from a send that went with it (T42, the hub's
 *  `peers::JOINED_SEPARATOR`). */
export const JOINED_SEPARATOR = '\n\n--- sent with hqc_send while answering ---\n';

/** The final message, then the texts of the sends that went with it. */
export function splitReply(reply: string): string[] {
  return reply.split(JOINED_SEPARATOR);
}

/** Paths a call touches: the path arguments and, for Bash, absolute or `~` words of the
 *  command. */
export function callPaths(tool: string, args: Record<string, unknown>): string[] {
  const out: string[] = [];
  for (const k of ['file_path', 'notebook_path', 'path']) if (typeof args[k] === 'string') out.push(args[k] as string);
  if (tool === 'Bash' && typeof args.command === 'string') {
    for (const w of args.command.split(/[\s;|&()<>'"=]+/)) if (/^(\/|~\/|~$)/.test(w)) out.push(w);
  }
  return out;
}

/** The first of `paths` outside `root` (an absolute folder), or undefined. `~` is taken for
 *  the home folder of `root` (`/Users/<name>` or `/home/<name>`). */
export function outsidePath(paths: string[], root: string | undefined): string | undefined {
  if (!root) return undefined;
  const home = /^\/(?:Users|home)\/[^/]+/.exec(root)?.[0];
  const norm = (p: string) => (home && (p === '~' || p.startsWith('~/')) ? home + p.slice(1) : p).replace(/\/+$/, '') || '/';
  const r = norm(root);
  return paths.find((p) => {
    const n = norm(p);
    if (!n.startsWith('/')) return false;
    return n !== r && !n.startsWith(`${r}/`);
  });
}
