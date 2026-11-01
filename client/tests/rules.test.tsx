// @vitest-environment happy-dom
// T48: "Allow in this folder" on the permission card (the worker's patterns, one per part,
// editable; saved before the call is allowed), the folder rules screen (its groups with
// sources, the check, adding and removing hqc rules, the worker offline) and the rule
// syntax the client checks as the user types.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ApiError, type Api } from '../src/api';
import { AppState } from '../src/app';
import { parseEvent, parseWorkspacePermissions, type WorkspacePermissions, type WorkspaceRule } from '../src/proto';
import { isToolName, ruleCommand, ruleError } from '../src/rules';
import { Timeline } from '../src/timeline';
import { ChatItems, type ChatActions } from '../src/ui/cards';
import { CHECK_DELAY, RulesScreen } from '../src/ui/Rules';
import type { HubSocket } from '../src/ws';

const WIRE = join(import.meta.dirname, '../../crates/hqc-proto/examples/wire');
const raw = (name: string): any => JSON.parse(readFileSync(join(WIRE, `${name}.json`), 'utf8'));

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
  vi.useRealTimers();
});

const $ = <E extends Element = HTMLElement>(sel: string) => root.querySelector<E>(sel);
const $$ = (sel: string) => [...root.querySelectorAll<HTMLElement>(sel)];
const buttonByText = (text: string) => $$('button').find((b) => b.textContent?.trim() === text) as HTMLButtonElement | undefined;
const flush = () => act(async () => void (await new Promise((r) => setTimeout(r, 0))));
const typeInto = (el: HTMLInputElement, value: string) =>
  act(() => {
    el.value = value;
    el.dispatchEvent(new Event('input', { bubbles: true }));
  });

describe('rule syntax', () => {
  it('matches the hub: Bash patterns, tool names, never a whole Bash', () => {
    for (const ok of ['Bash(python3 *)', 'Bash(git fetch:*)', 'Bash(npm test)', 'Edit', 'mcp__hqc', 'Bash(./tools/x.sh --out=a *)']) {
      expect(ruleError(ok), ok).toBeNull();
    }
    for (const bad of ['', 'Bash', 'Bash()', 'Bash(a && b)', 'Bash(a  b)', 'Bash(echo $(id))', 'Bash(rm * x)', 'Read(./src/**)', ' Edit', 'Bash(x *']) {
      expect(ruleError(bad), bad).not.toBeNull();
    }
    expect(ruleCommand('Bash(git push:*)')).toBe('git push');
    expect(ruleCommand('Edit')).toBeNull();
    expect(isToolName('Edit')).toBe(true);
    expect(isToolName('mcp__hqc__hqc_send')).toBe(true);
    expect(isToolName('git push')).toBe(false);
  });
});

describe('permission card: Allow in this folder', () => {
  const request = () =>
    parseEvent({
      ...raw('event.approval.request'),
      payload: {
        ...raw('event.approval.request').payload,
        description: 'python3 tools/conv.py x && git -C sub fetch',
        rule_suggestions: [
          { part: 'python3 tools/conv.py x', rule: 'Bash(python3 *)' },
          { part: 'git -C sub fetch', rule: 'Bash(git fetch:*)' },
          { part: 'FOO=1 make' },
        ],
      },
    });

  function show(over: Partial<ChatActions> = {}) {
    const t = new Timeline();
    t.apply(request());
    const actions: ChatActions = {
      timeline: t,
      approve: vi.fn(async () => {}),
      answer: vi.fn(async () => {}),
      resend: vi.fn(),
      discard: vi.fn(),
      openArtifact: vi.fn(),
      now: () => Date.parse('2026-10-22T10:30:00Z'),
      ...over,
    };
    root = document.createElement('div');
    document.body.appendChild(root);
    act(() => render(<ChatItems items={t.items()} actions={actions} />, root));
    return actions;
  }

  it('is offered only with a folder and the worker’s suggestions', () => {
    show();
    expect(buttonByText('Allow in this folder')).toBeUndefined();
  });

  it('shows a field per part with the suggested pattern, saves, then allows once', async () => {
    const saved: string[][] = [];
    const order: string[] = [];
    const folder = {
      path: '~/work/jai',
      rulesHref: '#/p/p1/rules/w1',
      save: vi.fn(async (r: string[]) => {
        saved.push(r);
        order.push('save');
      }),
    };
    const actions = show({ folder });
    (actions.approve as ReturnType<typeof vi.fn>).mockImplementation(async () => void order.push('approve'));
    act(() => buttonByText('Allow in this folder')!.click());
    const fields = $$('.perm-folder input') as HTMLInputElement[];
    expect(fields.map((f) => f.value)).toEqual(['Bash(python3 *)', 'Bash(git fetch:*)', '']);
    expect($$('.perm-folder-part').map((p) => p.textContent)).toEqual(['python3 tools/conv.py x', 'git -C sub fetch', 'FOO=1 make']);
    expect($('.perm-folder .perm-note')!.textContent).toContain('3 parts');
    expect($<HTMLAnchorElement>('.perm-folder a')!.getAttribute('href')).toBe('#/p/p1/rules/w1');

    // An invalid pattern blocks saving; an edited one is saved as edited.
    await typeInto(fields[0], 'Bash(python3 && x)');
    expect(buttonByText('Save and allow')!.disabled).toBe(true);
    await typeInto(fields[0], 'Bash(python3 tools/conv.py *)');
    act(() => buttonByText('Save and allow')!.click());
    await flush();
    expect(saved).toEqual([['Bash(python3 tools/conv.py *)', 'Bash(git fetch:*)']]);
    expect(actions.approve).toHaveBeenCalledWith('req-1', 'allow');
    expect(order).toEqual(['save', 'approve']);
  });

  it('does not allow when the rules were not saved', async () => {
    const folder = { path: '~/w', rulesHref: '#', save: vi.fn(async () => Promise.reject(new Error('too many rules'))) };
    const actions = show({ folder });
    act(() => buttonByText('Allow in this folder')!.click());
    act(() => buttonByText('Save and allow')!.click());
    await flush();
    expect(folder.save).toHaveBeenCalled();
    expect(actions.approve).not.toHaveBeenCalled();
    expect(root.textContent).toContain('too many rules');
  });
});

describe('the rules screen', () => {
  const PERMS: WorkspacePermissions = parseWorkspacePermissions(raw('api.workspace.permissions'));

  function setup(opts: { offline?: boolean; rules?: WorkspaceRule[]; phone?: boolean; environment?: string; perms?: WorkspacePermissions } = {}) {
    let rules: WorkspaceRule[] = opts.rules ?? [
      { id: 'r2', rule: 'Edit', created_at: '2026-10-30T12:00:00Z' },
      { id: 'r1', rule: 'Bash(python3 *)', created_at: '2026-10-28T09:30:00Z' },
    ];
    const checks: unknown[] = [];
    const api = {
      workspaces: vi.fn(async () => [
        {
          id: 'w1',
          project_id: 'p1',
          worker_id: 'wk1',
          path: '/Users/me/work/jai',
          label: '',
          environment: opts.environment ?? 'default',
          kind: 'dir',
          mirror_enabled: true,
          hqc_rules: rules.length,
          created_at: '2026-10-20T10:00:00Z',
        },
      ]),
      workers: vi.fn(async () => [{ id: 'wk1', name: 'drzlo-mac', status: opts.offline ? 'offline' : 'online' }]),
      workspaceRules: vi.fn(async () => rules),
      addWorkspaceRules: vi.fn(async (_id: string, add: string[]) => {
        rules = [...add.map((r, i) => ({ id: `n${i}`, rule: r, created_at: '2026-11-01T10:00:00Z' })), ...rules];
        return rules;
      }),
      deleteWorkspaceRule: vi.fn(async (_id: string, ruleId: string) => {
        rules = rules.filter((r) => r.id !== ruleId);
      }),
      workspacePermissions: vi.fn(async (_id: string, check?: { command?: string; tool?: string }) => {
        if (opts.offline) throw new ApiError(409, 'the worker is offline', 'worker_offline');
        checks.push(check);
        if (!check) return { ...(opts.perms ?? PERMS), check: undefined };
        if (check.command === 'git push origin' || check.command === 'ssh') {
          return { ...PERMS, check: { verdict: 'deny', rule: 'Bash(ssh:*)', source: 'worker.toml', parts: 1, uncovered: 1, suggestions: [] } };
        }
        if (check.command === 'npm install && npm test') {
          return {
            ...PERMS,
            check: {
              verdict: 'ask',
              parts: 2,
              uncovered: 2,
              suggestions: [
                { part: 'npm install', rule: 'Bash(npm *)' },
                { part: 'npm test', rule: 'Bash(npm *)' },
              ],
            },
          };
        }
        return { ...PERMS, check: { verdict: 'allow', rule: 'Edit', source: 'hqc', parts: 1, uncovered: 0, suggestions: [] } };
      }),
    };
    const socket = { onState: () => () => {}, subscribe: () => () => {} } as unknown as HubSocket;
    const app = new AppState(api as unknown as Api, socket);
    root = document.createElement('div');
    document.body.appendChild(root);
    act(() => render(<RulesScreen app={app} projectId="p1" workspaceId="w1" phone={opts.phone} />, root));
    return { api, checks };
  }

  const group = (name: string) => $(`section[aria-label="${name}"]`)!;
  const rows = (name: string) =>
    [...group(name).querySelectorAll('.rule-row')].map((r) => [
      r.querySelector('.rule-text')!.textContent,
      r.querySelector('.rule-source')!.textContent,
    ]);

  it('shows denied and allowed rules in the order they act, each with its source', async () => {
    setup();
    await flush();
    await flush();
    expect($('.rules-meta')!.textContent).toContain('drzlo-mac');
    expect($('.rules-meta')!.textContent).toContain('~/work/jai');
    expect($('.rules-meta')!.textContent).toContain('Environment default');
    expect($('.rules-mode')!.textContent).toBe('Ask by default');
    expect(rows('Denied')).toEqual([
      ['Bash(ssh:*)', 'worker.toml'],
      ['Bash(rm -rf /*)', 'worker.toml'],
      ['Bash(curl:*)', '~/.claude/settings.json'],
    ]);
    // hqc first, newest first, with the date and a cross; then the files.
    expect(rows('Allowed')).toEqual([
      ['Edit', 'HQC'],
      ['Bash(python3 *)', 'HQC'],
      ['Bash(ls *)', 'worker.toml'],
      ['Read', '~/.claude/settings.json'],
      ['mcp__hqc', '.claude/settings.local.json'],
    ]);
    expect(group('Allowed').querySelectorAll('.rule-remove').length).toBe(2);
    expect(group('Everything else').textContent).toContain('This folder runs in Ask');
  });

  it('shows the environment of the folder and its rules under their own source (T43)', async () => {
    const env = 'worker.toml [environments.infra]';
    const [worker, ...files] = PERMS.sources;
    setup({
      environment: 'infra',
      perms: {
        ...PERMS,
        sources: [
          { ...worker, deny: worker.deny.filter((r) => r !== 'Bash(ssh:*)') },
          { source: env, allow: ['Bash(ssh debian@192.168.1.20:*)'], deny: ['Bash(git push:*)'], ask: [] },
          ...files,
        ],
      },
    });
    await flush();
    await flush();
    expect($('.rules-env')!.textContent).toBe('infra');
    expect(rows('Denied')).toEqual([
      ['Bash(rm -rf /*)', 'worker.toml'],
      ['Bash(git push:*)', env],
      ['Bash(curl:*)', '~/.claude/settings.json'],
    ]);
    expect(rows('Allowed')).toContainEqual(['Bash(ssh debian@192.168.1.20:*)', env]);
  });

  it('checks a command on the worker and offers the suggested rule', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const { api } = setup();
    await flush();
    const field = $<HTMLInputElement>('#rules-check')!;
    await typeInto(field, 'npm install && npm test');
    await act(async () => void vi.advanceTimersByTime(CHECK_DELAY + 10));
    await flush();
    expect(api.workspacePermissions).toHaveBeenLastCalledWith('w1', { command: 'npm install && npm test' });
    expect($('.rules-result-label')!.textContent).toBe('Will ask');
    expect($('.rules-result')!.textContent).toContain('2 of 2 parts not covered');
    act(() => buttonByText('Allow…')!.click());
    expect($<HTMLInputElement>('#rules-new')!.value).toBe('Bash(npm *)');

    await typeInto(field, 'Edit');
    await act(async () => void vi.advanceTimersByTime(CHECK_DELAY + 10));
    await flush();
    expect(api.workspacePermissions).toHaveBeenLastCalledWith('w1', { tool: 'Edit' });
    expect($('.rules-result-label')!.textContent).toBe('Allowed');
    expect($('.rules-result')!.textContent).toContain('by rule Edit · HQC');
  });

  it('adds a rule with a live check, warns about a deny that wins, refuses a duplicate', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const { api } = setup();
    await flush();
    act(() => buttonByText('Allow in this folder')!.click());
    const field = $<HTMLInputElement>('#rules-new')!;
    await typeInto(field, 'Bash(a && b)');
    expect($('.rules-msg.is-error')).not.toBeNull();
    expect(buttonByText('Save')!.disabled).toBe(true);
    await typeInto(field, 'Edit');
    expect($('.rules-msg.is-error')!.textContent).toBe('This rule is already in the folder.');
    await typeInto(field, 'Bash(git push origin:*)');
    await act(async () => void vi.advanceTimersByTime(CHECK_DELAY + 10));
    await flush();
    expect($('.rules-msg.is-warn')!.textContent).toContain('Won’t take effect: Bash(ssh:*) in worker.toml');
    expect(buttonByText('Save')!.disabled).toBe(false);
    act(() => buttonByText('Save')!.click());
    await flush();
    expect(api.addWorkspaceRules).toHaveBeenCalledWith('w1', ['Bash(git push origin:*)']);
    expect(rows('Allowed')[0]).toEqual(['Bash(git push origin:*)', 'HQC']);
    expect($('#rules-new')).toBeNull();
  });

  it('removes an hqc rule after a confirmation in the row', async () => {
    const { api } = setup();
    await flush();
    await flush();
    act(() => $<HTMLButtonElement>('button[aria-label="Remove rule Edit"]')!.click());
    expect(group('Allowed').textContent).toContain('Takes effect in running sessions right away.');
    act(() => $<HTMLButtonElement>('.btn-confirm-remove')!.click());
    await flush();
    expect(api.deleteWorkspaceRule).toHaveBeenCalledWith('w1', 'r2');
    expect(rows('Allowed').map((r) => r[0])).not.toContain('Edit');
  });

  it('with the worker offline shows hqc rules only and no check', async () => {
    setup({ offline: true });
    await flush();
    await flush();
    expect(rows('Denied')).toEqual([]);
    expect(group('Denied').textContent).toContain('Worker unavailable');
    expect(rows('Allowed')).toEqual([
      ['Edit', 'HQC'],
      ['Bash(python3 *)', 'HQC'],
    ]);
    expect(group('Allowed').textContent).toContain('HQC rules still save and apply when it’s back');
    expect($<HTMLInputElement>('#rules-check')!.disabled).toBe(true);
    expect($('.rules-result-label')!.textContent).toBe('Unavailable');
    // Still editable.
    expect(buttonByText('Allow in this folder')).toBeDefined();
  });

  it('on a phone the add form is a full-screen page', async () => {
    setup({ phone: true });
    await flush();
    act(() => buttonByText('Allow in this folder')!.click());
    expect($('.rules-form-page .rules-form-title')!.textContent).toBe('New rule');
    expect(buttonByText('Cancel')).toBeDefined();
  });
});
