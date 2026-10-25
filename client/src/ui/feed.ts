// What the feed draws, built from the timeline's items (T26): runs of tool calls fold into
// one group, captioned by the interim agent message right before them; the `usage` after a
// final answer goes into the answer's meta line; a `tool.call` followed at once by the
// `approval.request` for it is shown by the request card, not as a row of its own.
// Pure logic, no DOM.

import type { EventOf, HqcEvent } from '../proto';
import type { Item, Timeline } from '../timeline';
import { oneLine } from './labels';

export type ToolRow =
  | { kind: 'call'; key: string; call?: EventOf<'tool.call'>; result?: EventOf<'tool.result'> }
  | { kind: 'thinking'; key: string; event: EventOf<'agent.thinking'> };

export type Block =
  | { kind: 'item'; key: string; item: Item }
  | { kind: 'agent'; key: string; event: EventOf<'agent.message'>; usage?: EventOf<'usage'> }
  | { kind: 'tools'; key: string; caption?: EventOf<'agent.message'>; rows: ToolRow[] }
  | { kind: 'approval'; key: string; event: EventOf<'approval.request'>; call?: EventOf<'tool.call'> };

/** Events the feed does not draw at all: `status` but an error with a reason (SPEC §17.1). */
function hidden(e: HqcEvent): boolean {
  return e.type === 'status' && (e.payload.state !== 'error' || !e.payload.detail);
}

export function buildFeed(items: Item[], timeline: Timeline): Block[] {
  const out: Block[] = [];
  let group: Extract<Block, { kind: 'tools' }> | null = null;
  const close = () => (group = null);

  for (const it of items) {
    if (it.kind !== 'event') {
      close();
      out.push({ kind: 'item', key: it.key, item: it });
      continue;
    }
    const e = it.event;
    if (hidden(e)) continue;
    switch (e.type) {
      case 'tool.call':
      case 'tool.result': {
        // A result is drawn in its call's row.
        if (e.type === 'tool.result' && timeline.hasCall(e.payload.call_id)) continue;
        if (!group) {
          group = { kind: 'tools', key: it.key, rows: [] };
          const prev = out.at(-1);
          if (prev?.kind === 'agent' && !prev.event.payload.final && !prev.usage) {
            out.pop();
            group.caption = prev.event;
          }
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
          if (!g.rows.some((r) => r.kind === 'call')) {
            // Nothing left but the caption: the message is drawn as itself again.
            out.splice(out.indexOf(g), 1);
            if (g.caption) out.push({ kind: 'agent', key: `e${g.caption.seq}`, event: g.caption });
          }
        }
        close();
        out.push({ kind: 'approval', key: it.key, event: e, call });
        continue;
      }
      case 'usage': {
        const prev = out.at(-1);
        if (prev?.kind === 'agent' && prev.event.payload.final && !prev.usage) {
          prev.usage = e;
          continue;
        }
        break;
      }
      case 'agent.message':
        close();
        out.push({ kind: 'agent', key: it.key, event: e });
        continue;
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

/** `running` while a call has no result, `error` if any failed, else `ok`. */
export function groupState(rows: ToolRow[]): 'running' | 'error' | 'ok' {
  const calls = callRows(rows);
  if (calls.some((r) => !r.result)) return 'running';
  return calls.some((r) => r.result!.payload.error) ? 'error' : 'ok';
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
export function captionText(m: EventOf<'agent.message'>, max = 140): string {
  return oneLine(m.payload.text.replace(/[#*_`>]+/g, ' '), max);
}

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
export function callLine(tool: string, argsPreview: string): { shell: boolean; text: string } {
  const args = callArgs(argsPreview);
  if (tool === 'Bash' && typeof args.command === 'string') return { shell: true, text: oneLine(args.command, 400) };
  const key = BRIEF_KEYS.find((k) => typeof args[k] === 'string' && args[k]);
  return { shell: false, text: key ? oneLine(String(args[key]), 400) : '' };
}

/** For a call of the hqc tool `hqc_ask` (`mcp__hqc__hqc_ask` in Claude Code, SPEC §7.1):
 *  the session it asked, '' if the arguments do not say; undefined for any other tool. */
export function askTarget(tool: string, argsPreview: string): string | undefined {
  if (tool !== 'hqc_ask' && !tool.endsWith('__hqc_ask')) return undefined;
  const s = callArgs(argsPreview).session;
  return typeof s === 'string' ? oneLine(s, 200) : '';
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
