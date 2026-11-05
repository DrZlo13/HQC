// @vitest-environment happy-dom
// T26: the chat screen — the header (state pill, worker, path, model and cost of the
// session, "Stop", "More"), the two handover banners in place of the composer, the
// request's hotkeys through the chat, and the phone's bottom sheet with the text answer.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Api, SessionPatch } from '../src/api';
import { AppState } from '../src/app';
import {
  parseEvent,
  type HeldBy,
  type HqcEvent,
  type PermissionMode,
  type SessionInfo,
  type SessionState,
  type SpawnedBy,
} from '../src/proto';
import { AUTO_CONFIRM } from '../src/ui/labels';
import type { HubSocket } from '../src/ws';
import { Chat, withQuote } from '../src/ui/Chat';

const WIRE = join(import.meta.dirname, '../../crates/hqc-proto/examples/wire');
const ev = (name: string, seq: number, patch: (e: any) => void = () => {}): HqcEvent => {
  const e = JSON.parse(readFileSync(join(WIRE, `${name}.json`), 'utf8'));
  e.seq = seq;
  e.session_id = 's1';
  patch(e);
  return parseEvent(e);
};

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
});

const HISTORY = [
  ev('event.user.message', 1, (e) => (e.payload.artifact_ids = [])),
  ev('event.agent.message', 2),
  ev('event.usage', 3, (e) => (e.payload = { input_tokens: 2, output_tokens: 555, cost_usd: 0.05, model: 'claude-opus-5-5' })),
  ev('event.user.message', 4, (e) => ((e.event_id = '0199f3a2-7c10-7000-8000-0000000001c4'), (e.payload.artifact_ids = []))),
  ev('event.tool.call', 5, (e) => (e.payload.args_preview = '{"command":"ls /"}')),
  ev('event.approval.request', 6, (e) => {
    e.ts = new Date(Date.now() - 42_000).toISOString();
    e.payload.description = 'List root directory\nls /';
    delete e.payload.expires_at;
  }),
  ev('event.usage', 7, (e) => (e.payload = { input_tokens: 1, output_tokens: 2, cost_usd: 0.02 })),
];

function setup(
  state: SessionState,
  opts: {
    held_by?: HeldBy;
    history?: HqcEvent[];
    commands?: string[];
    fetched?: string[];
    spawned_by?: SpawnedBy;
    permission_mode?: PermissionMode;
    effective_permission_mode?: PermissionMode;
    permission_capped_by?: string;
    subagents?: number;
    model?: string;
  } = {},
) {
  const history = opts.history ?? HISTORY;
  const api = {
    // GET /api/sessions/{id}: the session object with the commands the hub has by now.
    session: vi.fn(async () => ({ ...info, commands: opts.fetched })),
    events: vi.fn(async (_id: string, after: number) => {
      const events = history.filter((e) => e.seq! > after);
      return { events, invalid: [], maxSeq: Math.max(after, ...events.map((e) => e.seq!)), count: events.length };
    }),
    eventsBefore: vi.fn(async (_id: string, before = Number.MAX_SAFE_INTEGER, limit = 200) => {
      const found = history.filter((e) => e.seq! < before).slice(-limit);
      const seqs = found.map((e) => e.seq!);
      return { events: found, invalid: [], maxSeq: Math.max(0, ...seqs), minSeq: seqs[0], count: found.length };
    }),
    readCursor: vi.fn(async () => {}),
    approve: vi.fn(async () => {}),
    takeover: vi.fn(async () => ({})),
    // PATCH /api/sessions/{id}: the hub stores the mode, the worker confirms it at once; the
    // model is stored as sent (`null`: the runtime's default).
    patchSession: vi.fn(async (_id: string, patch: SessionPatch) => ({
      ...info,
      permission_mode: patch.permission_mode,
      effective_permission_mode: patch.permission_mode,
      model: patch.model === undefined ? info.model : (patch.model ?? undefined),
    })),
    sessions: vi.fn(async () => []),
    projects: vi.fn(async () => []),
    workspaces: vi.fn(async () => [
      {
        id: 'w1',
        project_id: 'p1',
        worker_id: 'wk1',
        path: '/Users/me/work/hqc',
        label: '',
        kind: 'dir',
        mirror_enabled: true,
        created_at: '2026-10-20T10:00:00Z',
      },
    ]),
  };
  const socket = { onState: () => () => {}, subscribe: () => () => {} } as unknown as HubSocket;
  const app = new AppState(api as unknown as Api, socket);
  app.conn = 'open';
  const info: SessionInfo = {
    id: 's1',
    project_id: 'p1',
    workspace_id: 'w1',
    worker_id: 'wk1',
    cwd: '.',
    title: 'test 12',
    slug: 'test-12',
    state,
    queue_len: 0,
    unread: 0,
    last_seq: 7,
    last_read_seq: 7,
    archived: false,
    updated_at: '2026-10-24T19:47:00Z',
    system: false,
    held_by: opts.held_by,
    commands: opts.commands,
    spawned_by: opts.spawned_by,
    permission_mode: opts.permission_mode,
    effective_permission_mode: opts.effective_permission_mode,
    permission_capped_by: opts.permission_capped_by,
    subagents: opts.subagents,
    model: opts.model,
  };
  app.sessions.set('s1', info);
  app.sessions.set('sys', { ...info, id: 'sys', title: 'drzlo-mac', system: true, workspace_id: undefined, held_by: undefined, state: 'idle' });
  return { api, app };
}

async function mount(app: AppState, phone = false) {
  root = document.createElement('div');
  document.body.appendChild(root);
  // History over REST and the workspaces load inside act, so the effects of the renders
  // they cause run before the test looks.
  await act(async () => {
    render(<Chat app={app} sessionId="s1" phone={phone} />, root);
    await new Promise((r) => setTimeout(r, 50));
  });
}

const $ = (sel: string) => root.querySelector<HTMLElement>(sel);
const button = (text: string) => [...root.querySelectorAll<HTMLButtonElement>('button')].find((b) => b.textContent?.trim() === text);

// T38: the session's permission mode in the header, and the switch in "More".
describe('permission mode', () => {
  const mark = () => ({ auto: $('.perm-auto')?.textContent, ask: $('.perm-ask')?.textContent });
  const menu = async () => {
    await act(async () => $('.btn-more')!.click());
    return [...root.querySelectorAll<HTMLButtonElement>('.menu-item')];
  };

  it('marks Auto, shows Ask, and what the worker runs over what was asked', async () => {
    let { app } = setup('idle', { permission_mode: 'auto', effective_permission_mode: 'auto' });
    await mount(app);
    expect(mark()).toEqual({ auto: 'Auto', ask: undefined });
    expect($('.perm-auto')!.getAttribute('title')).toContain('deny list');
    render(null, root);

    // Asked for auto, the worker runs ask (allow_auto = false, or not started again yet).
    ({ app } = setup('idle', { permission_mode: 'auto', effective_permission_mode: 'ask' }));
    await mount(app);
    expect(mark()).toEqual({ auto: undefined, ask: 'Ask (Auto asked)' });
    render(null, root);

    ({ app } = setup('idle', { effective_permission_mode: 'ask' }));
    await mount(app);
    expect(mark()).toEqual({ auto: undefined, ask: 'Ask' });
    render(null, root);

    // Nothing known yet: no mark.
    ({ app } = setup('idle'));
    await mount(app);
    expect(mark()).toEqual({ auto: undefined, ask: undefined });
  });

  it('switches to Auto after a one-line confirmation and back to Ask without one', async () => {
    const { app, api } = setup('idle', { effective_permission_mode: 'ask' });
    await mount(app);
    const confirm = vi.fn(() => false);
    vi.stubGlobal('confirm', confirm);
    try {
      let items = await menu();
      expect(items.map((b) => b.textContent)).toContain('Switch to Auto');
      await act(async () => items.find((b) => b.textContent === 'Switch to Auto')!.click());
      expect(confirm).toHaveBeenCalledWith(AUTO_CONFIRM);
      expect(AUTO_CONFIRM).toContain("deny list is the only limit");
      expect(AUTO_CONFIRM.includes('\n')).toBe(false);
      expect(api.patchSession).not.toHaveBeenCalled();

      confirm.mockReturnValue(true);
      items = await menu();
      await act(async () => {
        items.find((b) => b.textContent === 'Switch to Auto')!.click();
        await new Promise((r) => setTimeout(r, 10));
      });
      expect(api.patchSession).toHaveBeenCalledWith('s1', { permission_mode: 'auto' });
      expect(mark().auto).toBe('Auto');

      confirm.mockClear();
      items = await menu();
      expect(items.map((b) => b.textContent)).toContain('Switch to Ask');
      await act(async () => {
        items.find((b) => b.textContent === 'Switch to Ask')!.click();
        await new Promise((r) => setTimeout(r, 10));
      });
      expect(confirm).not.toHaveBeenCalled();
      expect(api.patchSession).toHaveBeenLastCalledWith('s1', { permission_mode: 'ask' });
      expect(mark()).toEqual({ auto: undefined, ask: 'Ask' });
    } finally {
      vi.unstubAllGlobals();
    }
  });

  // T39: a turn capped by a message from a session in Ask.
  it('shows a capped turn as Ask, who asked, and the own mode after it', async () => {
    const { app } = setup('running', {
      permission_mode: 'auto',
      effective_permission_mode: 'ask',
      permission_capped_by: 'personal/a',
    });
    await mount(app);
    expect(mark()).toEqual({ auto: undefined, ask: 'Ask (asked by personal/a) · Auto after this turn' });
    expect($('.perm-capped')!.getAttribute('title')).toContain('cannot raise permissions');

    // The turn ends: the summary drops the cap and the worker runs Auto again.
    const s = app.sessions.get('s1')!;
    await act(async () =>
      app.mergeSummary([
        {
          session_id: 's1',
          project_id: 'p1',
          title: s.title,
          state: 'idle',
          queue_len: 0,
          unread: 0,
          last_seq: 7,
          last_read_seq: 7,
          permission_mode: 'auto',
          effective_permission_mode: 'auto',
        },
      ]),
    );
    expect(mark()).toEqual({ auto: 'Auto', ask: undefined });
  });

  it('follows the mode a summary brings', async () => {
    const { app } = setup('idle', { permission_mode: 'auto', effective_permission_mode: 'ask' });
    await mount(app);
    const s = app.sessions.get('s1')!;
    await act(async () =>
      app.mergeSummary([
        {
          session_id: 's1',
          project_id: 'p1',
          title: s.title,
          state: 'idle',
          queue_len: 0,
          unread: 0,
          last_seq: 7,
          last_read_seq: 7,
          permission_mode: 'auto',
          effective_permission_mode: 'auto',
        },
      ]),
    );
    expect(mark().auto).toBe('Auto');
  });
});

// The session's model in "More": a choice PATCHes at once, a typed name when it is done; `null`
// goes back to the runtime's default.
describe('model', () => {
  const field = <E extends HTMLElement = HTMLSelectElement>(name: string) =>
    root.querySelector<E>(`.menu-settings [name=${name}]`);
  const set = (name: string, value: string) =>
    act(async () => {
      const el = field<HTMLInputElement>(name)!;
      el.value = value;
      el.dispatchEvent(new Event('input', { bubbles: true }));
      el.dispatchEvent(new Event('change', { bubbles: true }));
      await new Promise((r) => setTimeout(r, 10));
    });
  const open = () => act(async () => $('.btn-more')!.click());

  it('switches the model: an alias at once, another name once typed, the default as null', async () => {
    const { app, api } = setup('running');
    await mount(app);
    await open();
    expect(field('model')!.value).toBe('');
    expect([...field('model')!.options].map((o) => o.textContent)).toEqual([
      "Claude Code's default",
      'Opus',
      'Sonnet',
      'Haiku',
      'Another model…',
    ]);
    await set('model', 'sonnet');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: 'sonnet' });
    expect(app.sessions.get('s1')!.model).toBe('sonnet');
    expect(field('model')!.value).toBe('sonnet');

    await set('model', 'custom');
    expect(api.patchSession).toHaveBeenCalledTimes(1);
    await set('model_name', ' claude-opus-5-5 ');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: 'claude-opus-5-5' });

    await set('model', '');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: null });
    expect(field('model_name')).toBeNull();
  });

  it('a name that is no alias shows as typed', async () => {
    const { app } = setup('idle', { model: 'claude-sonnet-5-5' });
    await mount(app);
    await open();
    expect(field('model')!.value).toBe('custom');
    expect(field<HTMLInputElement>('model_name')!.value).toBe('claude-sonnet-5-5');
  });

  it('shows what the hub refused', async () => {
    const { app, api } = setup('idle');
    api.patchSession.mockRejectedValueOnce(new Error('model must be 1..64 characters of A-Z a-z 0-9 . _ : -'));
    await mount(app);
    await open();
    await set('model', 'custom');
    await set('model_name', 'a b');
    expect(api.patchSession).toHaveBeenLastCalledWith('s1', { model: 'a b' });
    expect($('.notice')!.textContent).toContain('Not changed: model must be');
  });
});

describe('header', () => {
  it('shows the subagents running next to the state, only while there are any', async () => {
    let { app } = setup('running', { subagents: 2 });
    await mount(app);
    expect($('.chat-agents')!.textContent).toBe('2 agents');
    expect($('.pill + .chat-agents')).not.toBeNull();
    const s = app.sessions.get('s1')!;
    const summary = (subagents?: number) =>
      app.mergeSummary([
        { session_id: 's1', project_id: 'p1', title: s.title, state: 'running', queue_len: 0, unread: 0, last_seq: 7, last_read_seq: 7, subagents },
      ]);
    await act(async () => summary(1));
    expect($('.chat-agents')!.textContent).toBe('1 agent');
    await act(async () => summary(undefined));
    expect($('.chat-agents')).toBeNull();
    render(null, root);

    ({ app } = setup('running', { subagents: 0 }));
    await mount(app);
    expect($('.chat-agents')).toBeNull();
  });

  it('shows the state, worker, path, model and cost of the session, "Stop" and "More"', async () => {
    const { app } = setup('waiting');
    await mount(app);
    expect($('.chat-title')!.textContent).toBe('test 12');
    expect($('.pill')!.textContent).toBe('Waiting for permission');
    expect($('.pill .pulse')).toBeNull();
    const meta = $('.chat-meta')!.textContent!;
    expect(meta).toContain('drzlo-mac');
    expect($('.chat-path .mono')!.textContent).toBe('~/work/hqc');
    expect($('.chat-path button')!.getAttribute('aria-label')).toBe('Copy path');
    // The model of the last usage that names one, the sum of cost_usd.
    expect($('.chat-model')!.textContent).toBe('Opus 5.5');
    expect($('.chat-cost')!.textContent).toBe('$0.07 this session');
    expect(button('Stop')).toBeDefined();
    await act(async () => $('.btn-more')!.click());
    // T50: archiving is on the list's rows, not in the header.
    expect([...root.querySelectorAll('.menu-item')].map((b) => b.textContent)).toEqual(['Schedules', 'Switch to Auto']);
  });

  it('shows an archived session read-only: the chip, the band with the time, a dimmed feed, no input (T50)', async () => {
    const { app } = setup('idle');
    const at = new Date();
    at.setHours(23, 45, 0, 0);
    app.sessions.set('s1', { ...app.sessions.get('s1')!, archived: true, state: 'archived', archived_at: at.toISOString() });
    await mount(app);
    expect($('.pill')!.textContent).toBe('Archived');
    expect($('.archived-band')!.textContent).toBe(
      'Archived today at 23:45 · read-only. Restore or delete it from the list on the left.',
    );
    expect($('.chat.chat-archived .feed')).not.toBeNull();
    expect($('.chat textarea')).toBeNull();
    await act(async () => $('.btn-more')!.click());
    expect([...root.querySelectorAll('.menu-item')].map((b) => b.textContent)).toEqual(['Schedules']);
    render(null, root);

    // Archived before the hub kept the time: no time.
    app.sessions.set('s1', { ...app.sessions.get('s1')!, archived_at: undefined });
    await mount(app, true);
    expect($('.archived-band')!.textContent).toBe('Archived · read-only. Restore or delete it from the archive.');
  });

  it('links to the session that started it with hqc_spawn (T34)', async () => {
    let { app } = setup('idle');
    await mount(app);
    expect($('.chat-spawned')).toBeNull();
    render(null, root);
    ({ app } = setup('idle', { spawned_by: { session_id: 's0', address: 'hqc/planner' } }));
    await mount(app);
    expect($('.chat-spawned')!.textContent).toBe('started by hqc/planner');
    expect($('.chat-spawned a')!.getAttribute('href')).toBe('#/s/s0');
  });

  it('pulses while a turn is on, no "Stop" when idle', async () => {
    let { app } = setup('running');
    await mount(app);
    expect($('.pill')!.textContent).toBe('Running');
    expect($('.pill .pulse')).not.toBeNull();
    render(null, root);
    ({ app } = setup('idle', { history: HISTORY.slice(0, 3) }));
    await mount(app);
    expect($('.pill')!.textContent).toBe('Idle');
    expect(button('Stop')).toBeUndefined();
  });
});

describe('the request card through the chat', () => {
  it('folds the tool call into the card, explains, and takes Enter', async () => {
    const { api, app } = setup('waiting');
    await mount(app);
    expect($('.tool-group')).toBeNull();
    expect($('.perm-what')!.textContent).toBe('$ ls /');
    expect($('.perm-note')!.textContent).toBe(
      'A worker rule allows Bash, but Claude Code asked anyway: the path / is outside the working folder ~/work/hqc.',
    );
    expect($('.perm-timer')!.textContent).toMatch(/^waiting 0:4\d$/);
    await act(async () => void document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })));
    expect(api.approve.mock.calls[0].slice(0, 3)).toEqual(['s1', 'req-1', 'allow']);
  });
});

describe('the focus when a request arrives', () => {
  it('leaves an empty field so Enter reaches the card, keeps a field with text', async () => {
    for (const [text, blurred] of [['', true], ['typing a reply', false]] as const) {
      const field = document.createElement('textarea');
      field.value = text;
      document.body.appendChild(field);
      field.focus();
      expect(document.activeElement).toBe(field);
      const { app } = setup('waiting');
      await mount(app);
      expect(document.activeElement === field).toBe(!blurred);
      render(null, root);
      field.remove();
    }
  });
});

describe('handover banners instead of the composer (SPEC §9.3)', () => {
  it('open in VS Code right now: close it there, no button', async () => {
    const { app } = setup('locked_vscode', { held_by: { entrypoint: 'claude-vscode', status: 'idle' }, history: HISTORY.slice(0, 3) });
    await mount(app);
    expect($('.handover.handover-held .handover-title')!.textContent).toBe('The session is open in VS Code on drzlo-mac.');
    expect($('.handover-sub')!.textContent).toBe('Close it there to write from here.');
    expect(button('Take over here')).toBeUndefined();
    expect($('.composer')).toBeNull();
    expect(root.textContent).not.toContain('read-only');
  });

  it('last driven by VS Code, nobody holds it: "Take over here"', async () => {
    const { api, app } = setup('locked_vscode', { history: HISTORY.slice(0, 3) });
    await mount(app);
    expect($('.handover')!.classList.contains('handover-held')).toBe(false);
    expect($('.handover-title')!.textContent).toBe('VS Code was the last to run this session on drzlo-mac.');
    expect($('.handover-sub')).toBeNull();
    expect($('.composer')).toBeNull();
    await act(async () => button('Take over here')!.click());
    expect(api.takeover).toHaveBeenCalledWith('s1');
  });

  it('a session hqc drives has the composer', async () => {
    const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
    await mount(app);
    expect($('.handover')).toBeNull();
    expect($('.composer textarea')).not.toBeNull();
  });
});

describe('phone (Mobile-Session)', () => {
  it('the open request is a bottom sheet in place of the composer, with the text answer', async () => {
    const { api, app } = setup('waiting');
    await mount(app, true);
    expect($('.feed .perm')).toBeNull();
    const sheet = $('.sheet-wrap .perm-sheet')!;
    expect(sheet.querySelector('.perm-title')!.textContent).toBe('Run the command?');
    expect($('.composer')).toBeNull();
    expect($('a[aria-label="Back to sessions"]')!.getAttribute('href')).toBe('#/p/p1');
    expect($('button[aria-label="Stop the agent"]')).not.toBeNull();
    // "More" on a phone also has the path, the model and the cost.
    await act(async () => $('.btn-more')!.click());
    expect($('.menu-info')!.textContent).toContain('~/work/hqc');
    expect($('.menu-info')!.textContent).toContain('Opus 5.5');
    await act(async () => $('.btn-more')!.click());

    await act(async () => button('Reply with text instead')!.click());
    const area = sheet.querySelector<HTMLTextAreaElement>('textarea')!;
    await act(() => {
      area.value = 'Leave the root alone';
      area.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await act(async () => button('Deny and send')!.click());
    expect(api.approve).toHaveBeenCalledWith('s1', 'req-1', 'deny', 'Leave the root alone');
  });
});

describe('cost (T27)', () => {
  it('shows the runtime’s session total in the header and each turn’s own cost under it', async () => {
    const history = [
      ev('event.user.message', 1, (e) => (e.payload.artifact_ids = [])),
      ev('event.agent.message', 2),
      ev('event.usage', 3, (e) => (e.payload = { input_tokens: 2, output_tokens: 5, cost_usd: 0.4, session_cost_usd: 44.0 })),
      ev('event.user.message', 4, (e) => ((e.event_id = '0199f3a2-7c10-7000-8000-0000000001c4'), (e.payload.artifact_ids = []))),
      ev('event.agent.message', 5, (e) => (e.event_id = '0199f3a2-7c10-7000-8000-0000000001c5')),
      ev('event.usage', 6, (e) => (e.payload = { input_tokens: 2, output_tokens: 5, cost_usd: 0.401, session_cost_usd: 44.401 })),
    ];
    const { app } = setup('idle', { history, commands: [] });
    await mount(app);
    expect($('.chat-cost')!.textContent).toBe('$44.40 this session');
    const lines = [...root.querySelectorAll('.usage')].map((l) => l.textContent!);
    expect(lines).toHaveLength(2);
    expect(lines[0]).toContain('$0.400');
    expect(lines[1]).toContain('$0.401');
  });

  it('sums cost_usd when no usage has the session total (older events)', async () => {
    const { app } = setup('idle', { commands: [] });
    await mount(app);
    expect($('.chat-cost')!.textContent).toBe('$0.07 this session');
  });
});

describe('slash command completion (T27)', () => {
  const COMMANDS = ['compact', 'context', 'code-review', 'review', 'anthropic-skills:pdf'];
  const area = () => $('.composer-input') as HTMLTextAreaElement;
  const type = (text: string) =>
    act(() => {
      area().value = text;
      area().dispatchEvent(new Event('input', { bubbles: true }));
    });
  const press = async (key: string, init: KeyboardEventInit = {}) => {
    const e = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...init });
    await act(() => void area().dispatchEvent(e));
    return e;
  };
  const options = () => [...root.querySelectorAll('.cmd-item')].map((o) => o.textContent);
  const active = () => root.querySelector('.cmd-item.is-active')?.textContent;

  it('lists the commands matching what follows «/», moves with the arrows, inserts with Enter', async () => {
    const { app } = setup('idle', { commands: COMMANDS });
    await mount(app);
    await type('/co');
    expect(options()).toEqual(['/compact', '/context', '/code-review']);
    expect(active()).toBe('/compact');
    expect(area().getAttribute('aria-expanded')).toBe('true');
    expect(area().getAttribute('aria-activedescendant')).toBe('composer-command-0');
    await press('ArrowDown');
    await press('ArrowDown');
    expect(active()).toBe('/code-review');
    await press('ArrowDown');
    expect(active(), 'wraps around').toBe('/compact');
    await press('ArrowUp');
    expect(active()).toBe('/code-review');
    const enter = await press('Enter');
    expect(enter.defaultPrevented).toBe(true);
    expect(area().value).toBe('/code-review ');
    expect(options(), 'closed once a command is chosen').toEqual([]);
  });

  it('Tab inserts, Esc closes until the text changes, other text has no popup', async () => {
    const { app } = setup('idle', { commands: COMMANDS });
    await mount(app);
    await type('/AN');
    expect(options(), 'case does not matter').toEqual(['/anthropic-skills:pdf']);
    await press('Tab');
    expect(area().value).toBe('/anthropic-skills:pdf ');

    await type('/re');
    expect(options()).toEqual(['/review']);
    await press('Escape');
    expect(options()).toEqual([]);
    await type('/rev');
    expect(options(), 'typing on reopens it').toEqual(['/review']);
    await type('/review');
    expect(options(), 'typed in full').toEqual([]);
    await type('please /review');
    expect(options()).toEqual([]);
    await type('/nothing');
    expect(options()).toEqual([]);
  });

  it('a click (a tap on a phone) inserts the command', async () => {
    // Without the open request of HISTORY: on a phone it would take the composer's place.
    const { app } = setup('idle', { commands: COMMANDS, history: HISTORY.slice(0, 3) });
    await mount(app, true);
    await type('/');
    expect(options()).toHaveLength(COMMANDS.length);
    const item = [...root.querySelectorAll<HTMLElement>('.cmd-item')].find((o) => o.textContent === '/review')!;
    await act(() => item.click());
    expect(area().value).toBe('/review ');
  });

  it('no list, no popup; an idle session without a list fetches the session again', async () => {
    const { api, app } = setup('idle', { fetched: ['compact'] });
    await mount(app);
    expect(api.session).toHaveBeenCalledTimes(1);
    await type('/c');
    expect(options()).toEqual(['/compact']);

    const empty = setup('running');
    await act(async () => render(null, root));
    await mount(empty.app);
    expect(empty.api.session).not.toHaveBeenCalled();
    await type('/c');
    expect(options()).toEqual([]);
    expect(area().hasAttribute('aria-expanded')).toBe(false);
  });
});

describe('history', () => {
  const long = Array.from({ length: 450 }, (_, i) =>
    ev('event.agent.message', i + 1, (e) => (e.payload.text = `answer ${i + 1}`)),
  );

  it('shows the newest page at once and loads the older history behind it', async () => {
    const { api, app } = setup('idle', { history: long });
    let older!: () => void;
    const tail = api.eventsBefore.getMockImplementation()!;
    api.eventsBefore.mockImplementationOnce(tail);
    api.eventsBefore.mockImplementationOnce(
      (id, before, limit) => new Promise((r) => (older = () => r(tail(id, before, limit)))),
    );
    await mount(app);
    expect(api.events).not.toHaveBeenCalled();
    expect(root.querySelector('.feed .empty')).toBeNull();
    expect(api.eventsBefore.mock.calls[1]?.[1]).toBe(351);
    // The last 60 blocks of the newest 100 events.
    const blocks = () => [...root.querySelectorAll<HTMLElement>('.feed-block')];
    expect(blocks().length).toBe(60);
    expect(blocks().at(-1)!.textContent).toContain('answer 450');
    await act(async () => {
      older();
      await new Promise((r) => setTimeout(r, 20));
    });
    // What is on screen stays; scrolling to the top renders more.
    expect(blocks().length).toBe(60);
    const scroller = root.querySelector<HTMLElement>('.timeline')!;
    await act(async () => {
      scroller.scrollTop = 0;
      scroller.dispatchEvent(new Event('scroll'));
    });
    expect(blocks().length).toBe(120);
    expect(blocks()[0].textContent).toContain('answer 331');
  });
});

describe('Quote', () => {
  const area = () => $('.composer-input') as HTMLTextAreaElement;
  /** The text node of the feed (or of `scope`) that reads `text`. */
  const textNode = (text: string, scope = '.feed') => {
    const w = document.createTreeWalker($(scope)!, NodeFilter.SHOW_TEXT);
    for (let n = w.nextNode(); n; n = w.nextNode()) if (n.textContent === text) return n;
    throw new Error(`no text «${text}»`);
  };
  const select = (node: Node | null) =>
    act(async () => {
      const sel = document.getSelection()!;
      sel.removeAllRanges();
      if (node) {
        const r = document.createRange();
        r.selectNodeContents(node);
        sel.addRange(r);
      }
      document.dispatchEvent(new Event('selectionchange'));
    });

  it('puts the text selected in the feed after the draft as «> » lines and focuses the field', async () => {
    const { app } = setup('idle');
    await mount(app);
    expect($('.btn-quote')).toBeNull();
    await act(() => {
      area().value = 'Look at this';
      area().dispatchEvent(new Event('input', { bubbles: true }));
    });
    await select(textNode('All tests pass.'));
    expect($('.btn-quote')!.textContent).toBe('Quote');
    await act(() => $('.btn-quote')!.click());
    expect(area().value).toBe('Look at this\n\n> All tests pass.\n\n');
    expect(document.activeElement).toBe(area());
    expect(area().selectionStart).toBe(area().value.length);
    expect(document.getSelection()!.rangeCount).toBe(0);
    expect($('.btn-quote')).toBeNull();
    // The next quote goes after the first.
    await select(textNode('All tests pass.'));
    await act(() => $('.btn-quote')!.click());
    expect(area().value).toBe('Look at this\n\n> All tests pass.\n\n> All tests pass.\n\n');
  });

  it('offers nothing for text outside the feed, or without a composer', async () => {
    const { app } = setup('idle');
    await mount(app);
    await select(textNode('test 12', '.chat-head'));
    expect($('.btn-quote')).toBeNull();
    await select(textNode('All tests pass.'));
    expect($('.btn-quote')).not.toBeNull();
    await select(null);
    expect($('.btn-quote'), 'gone with the selection').toBeNull();
    render(null, root);

    const locked = setup('locked_vscode');
    await mount(locked.app);
    await select(textNode('All tests pass.'));
    expect($('.btn-quote')).toBeNull();
  });

  it('on a phone a tap that clears the selection still quotes it', async () => {
    // Without the open request of HISTORY: on a phone it would take the composer's place.
    const { app } = setup('idle', { history: HISTORY.slice(0, 3) });
    await mount(app, true);
    await select(textNode('All tests pass.'));
    const btn = $('.btn-quote')!;
    await act(() => void btn.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true })));
    await select(null);
    expect($('.btn-quote')).toBe(btn);
    await act(() => btn.click());
    expect(area().value).toBe('> All tests pass.\n\n');
    expect($('.btn-quote')).toBeNull();
  });

  it('quotes every line, a blank one as a bare «>», after a blank line', () => {
    expect(withQuote('', 'one')).toBe('> one\n\n');
    expect(withQuote('  \n', 'one')).toBe('> one\n\n');
    expect(withQuote('draft\n\n', 'one\n\n  two\r\nthree')).toBe('draft\n\n> one\n>\n>   two\n> three\n\n');
  });
});
