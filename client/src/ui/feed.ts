// What the feed draws, built from the timeline's items (T26): runs of tool calls fold into
// one group, and every agent message stays a message of its own (the agent narrates in its
// thinking, which goes into the group; what it writes is for the user); the `usage` after a
// final answer goes into the answer's meta line; a `tool.call` followed at once by the
// `approval.request` for it is shown by the request card, not as a row of its own. Messages
// between sessions (T51) are blocks of their own: the agent's `hqc_send` / `hqc_ask` call and
// the ask's result, and the final message answering a question with the attachments and sends
// of its turn. Pure logic, no DOM.

import type { EventOf, HqcEvent } from '../proto';
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
        if (e.type === 'tool.result') {
          const call = timeline.callFor(e.payload.call_id);
          if (call && hqcTool(call.payload.tool) === 'ask' && !e.payload.error) {
            close();
            out.push({ kind: 'reply-from', key: it.key, call, result: e });
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
        const g = group as Extract<Block, { kind: 'tools' }> | null;
        const last = g?.rows.at(-1);
        let call: EventOf<'tool.call'> | undefined;
        if (g && last?.kind === 'call' && last.call?.payload.tool === e.payload.tool) {
          call = last.call;
          g.rows.pop();
          if (!g.rows.some((r) => r.kind === 'call')) out.splice(out.indexOf(g), 1);
        }
        close();
        out.push({ kind: 'approval', key: it.key, event: e, call });
        continue;
      }
      case 'usage': {
        const prev = out.at(-1);
        if (((prev?.kind === 'agent' && prev.event.payload.final) || prev?.kind === 'reply-to') && !prev.usage) {
          prev.usage = e;
          continue;
        }
        break;
      }
      case 'agent.message': {
        close();
        const reply = rel.replies.get(e.seq!);
        out.push(reply ?? { kind: 'agent', key: it.key, event: e });
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
    return out;
  }
  return {};
}

const BRIEF_KEYS = ['file_path', 'notebook_path', 'path', 'pattern', 'url', 'query', 'command', 'description', 'prompt'];

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
