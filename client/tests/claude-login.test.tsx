// @vitest-environment happy-dom
// SPEC 0.185: the sign-in of a worker's own Claude Code from Settings → Workers — the line with
// how it is signed in, the flow (the page, the code, cancel, a wrong code) and the feed's
// `claude_signed_out` line that leads there. The code is a one-time secret: it goes only into
// the request, and the field is empty after it.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { Api, ClaudeAuthStatus, WorkerInfo } from '../src/api';
import type { AppState } from '../src/app';
import { parseEvent } from '../src/proto';
import { Timeline } from '../src/timeline';
import { ChatItems, type ChatActions } from '../src/ui/cards';
import { claudeLine, signInHref } from '../src/ui/ClaudeSignIn';
import { Settings } from '../src/ui/Settings';
import { WEB_SEARCHES } from '../src/websearch';

const WIRE = join(import.meta.dirname, '../../crates/hqc-proto/examples/wire');
const CODE = 'abc123#state-XYZ';

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
  localStorage.clear();
  sessionStorage.clear();
});

const $ = <E extends Element = HTMLElement>(sel: string) => root.querySelector<E>(sel);
const buttonByText = (text: string) =>
  [...root.querySelectorAll<HTMLButtonElement>('button')].find((b) => b.textContent?.trim() === text);
const flush = () => act(async () => void (await new Promise((r) => setTimeout(r, 0))));
const typeInto = (el: HTMLInputElement, value: string) =>
  act(() => {
    el.value = value;
    el.dispatchEvent(new Event('input', { bubbles: true }));
  });

const signedIn: ClaudeAuthStatus = {
  logged_in: true,
  auth_method: 'claude.ai',
  email: 'owner@example.com',
  subscription_type: 'max',
  expires_at: '2026-12-12T10:00:00Z',
};
const withLogin: WorkerInfo = {
  id: 'w1',
  name: 'home-mac',
  os: 'macos',
  status: 'online',
  capabilities: { claude_code: { version: '2.1.322', login: true } },
};

function settingsApp(api: Partial<Api>) {
  return {
    api: { pushKey: vi.fn(async () => null), ...api },
    me: { email: 'me@example.com' },
    device: {},
    theme: () => 'warm',
    lookName: () => 'spacious',
    newSessionProject: () => undefined,
    sortedProjects: () => [],
    webSearch: WEB_SEARCHES[0],
    providers: [],
    reloadSessions: vi.fn(async () => {}),
  } as unknown as AppState;
}

async function mount(api: Partial<Api>, workers: WorkerInfo[] = [withLogin]) {
  root = document.createElement('div');
  document.body.appendChild(root);
  const app = settingsApp({ workers: vi.fn(async () => workers), ...api });
  await act(async () => render(<Settings app={app} />, root));
  await flush();
  await flush();
  return app;
}

describe('the status line', () => {
  it('says who it is signed in as, the plan and until when', () => {
    expect(claudeLine(signedIn)).toBe('Claude: owner@example.com · Max · until 12 Dec');
    expect(claudeLine({ logged_in: false })).toBe('Claude: not signed in');
  });

  it('opens only Anthropic’s https pages', () => {
    expect(signInHref('https://claude.com/cai/oauth/authorize?x=1')).toBe('https://claude.com/cai/oauth/authorize?x=1');
    expect(signInHref('https://platform.claude.com/oauth')).not.toBeNull();
    expect(signInHref('https://console.claude.ai/x')).not.toBeNull();
    expect(signInHref('http://claude.com/x')).toBeNull();
    expect(signInHref('https://claude.com.evil.example/x')).toBeNull();
    expect(signInHref('https://evilclaude.com/x')).toBeNull();
    expect(signInHref('javascript:alert(1)')).toBeNull();
  });

  it('shows the worker’s status under it, with its error', async () => {
    const claudeAuth = vi.fn(async () => ({ ...signedIn, error: 'the token is about to expire' }));
    await mount({ claudeAuth });
    expect(claudeAuth).toHaveBeenCalledWith('w1');
    expect($('.worker-claude-status')?.textContent).toBe('Claude: owner@example.com · Max · until 12 Dec');
    expect($('.worker-claude-error')?.textContent).toBe('the token is about to expire');
    expect(buttonByText('Sign in again')).toBeTruthy();
  });

  it('has no line and no button for a worker without the capability', async () => {
    const claudeAuth = vi.fn(async () => signedIn);
    await mount({ claudeAuth }, [
      { id: 'w2', name: 'old-mac', os: 'macos', status: 'online' },
      { ...withLogin, id: 'w3', status: 'offline' },
    ]);
    expect(claudeAuth).not.toHaveBeenCalled();
    expect($('.worker-claude')).toBeNull();
    expect(buttonByText('Sign in')).toBeUndefined();
    expect(buttonByText('Sign in again')).toBeUndefined();
  });
});

describe('the sign-in', () => {
  const url = 'https://claude.com/cai/oauth/authorize?code=true&state=s1';

  it('opens the page, sends the code and shows the new status', async () => {
    const claudeLoginCode = vi.fn(async () => ({ ok: true, status: signedIn }));
    await mount({
      claudeAuth: vi.fn(async () => ({ logged_in: false })),
      claudeLogin: vi.fn(async () => ({ login_id: 'L1', url })),
      claudeLoginCode,
      claudeLoginCancel: vi.fn(async () => {}),
    });
    expect($('.worker-claude-status')?.textContent).toBe('Claude: not signed in');
    await act(async () => buttonByText('Sign in')!.click());
    await flush();
    const page = $<HTMLAnchorElement>('a.claude-sign-in-page')!;
    expect(page.textContent?.trim()).toBe('Open the sign-in page');
    expect(page.href).toBe(url);
    expect(page.target).toBe('_blank');
    expect(page.rel).toBe('noopener noreferrer');
    const input = $<HTMLInputElement>('input.claude-sign-in-code')!;
    expect(input.getAttribute('autocomplete')).toBe('off');

    await typeInto(input, CODE);
    await act(async () => buttonByText('Send')!.click());
    await flush();
    expect(claudeLoginCode).toHaveBeenCalledWith('w1', 'L1', CODE);
    expect($('.worker-claude-form')).toBeNull();
    expect($('.worker-claude-status')?.textContent).toBe('Claude: owner@example.com · Max · until 12 Dec');
    expect(buttonByText('Sign in again')).toBeTruthy();
    // The code is nowhere the client keeps it, nor on the page.
    for (const st of [localStorage, sessionStorage]) {
      for (let i = 0; i < st.length; i++) expect(st.getItem(st.key(i)!) ?? '').not.toContain(CODE);
    }
    expect(root.innerHTML).not.toContain(CODE);
  });

  it('keeps the form on a wrong code, with the message and "Start again"', async () => {
    const claudeLogin = vi.fn(async () => ({ login_id: 'L1', url }));
    const claudeLoginCancel = vi.fn(async () => {});
    await mount({
      claudeAuth: vi.fn(async () => ({ logged_in: false })),
      claudeLogin,
      claudeLoginCode: vi.fn(async () => ({ ok: false, message: 'Invalid code', status: { logged_in: false } })),
      claudeLoginCancel,
    });
    await act(async () => buttonByText('Sign in')!.click());
    await flush();
    const input = $<HTMLInputElement>('input.claude-sign-in-code')!;
    await typeInto(input, 'wrong');
    await act(async () => buttonByText('Send')!.click());
    await flush();
    expect($('.worker-claude-form-error')?.textContent).toBe('Invalid code');
    expect($('.worker-claude-form')).toBeTruthy();
    expect($<HTMLInputElement>('input.claude-sign-in-code')!.value).toBe('');
    expect(root.innerHTML).not.toContain('wrong');

    await act(async () => buttonByText('Start again')!.click());
    await flush();
    expect(claudeLoginCancel).toHaveBeenCalledWith('w1', 'L1');
    expect(claudeLogin).toHaveBeenCalledTimes(2);
    expect($('.worker-claude-form-error')).toBeNull();
    expect($('a.claude-sign-in-page')).toBeTruthy();
  });

  it('cancels the sign-in', async () => {
    const claudeLoginCancel = vi.fn(async () => {});
    await mount({
      claudeAuth: vi.fn(async () => signedIn),
      claudeLogin: vi.fn(async () => ({ login_id: 'L1', url })),
      claudeLoginCode: vi.fn(),
      claudeLoginCancel,
    });
    await act(async () => buttonByText('Sign in again')!.click());
    await flush();
    await typeInto($<HTMLInputElement>('input.claude-sign-in-code')!, CODE);
    await act(async () => buttonByText('Cancel')!.click());
    await flush();
    expect(claudeLoginCancel).toHaveBeenCalledWith('w1', 'L1');
    expect($('.worker-claude-form')).toBeNull();
    expect(buttonByText('Sign in again')).toBeTruthy();
    expect(root.innerHTML).not.toContain(CODE);
  });

  it('does not open a page that is not Anthropic’s', async () => {
    const claudeLoginCancel = vi.fn(async () => {});
    await mount({
      claudeAuth: vi.fn(async () => ({ logged_in: false })),
      claudeLogin: vi.fn(async () => ({ login_id: 'L1', url: 'https://claude.com.evil.example/x' })),
      claudeLoginCancel,
    });
    await act(async () => buttonByText('Sign in')!.click());
    await flush();
    expect($('a.claude-sign-in-page')).toBeNull();
    expect($('.worker-claude-form-error')?.textContent).toContain('not Anthropic');
    expect(claudeLoginCancel).toHaveBeenCalledWith('w1', 'L1');
  });
});

describe('the feed', () => {
  it('leads a signed-out line to Settings', () => {
    const e = JSON.parse(readFileSync(join(WIRE, 'event.system.json'), 'utf8'));
    e.seq = 1;
    e.session_id = 's1';
    e.payload = {
      code: 'claude_signed_out',
      text: 'Claude Code on home-mac is signed out: sign it in from Settings → Workers',
    };
    const t = new Timeline();
    t.apply(parseEvent(e));
    const actions = {
      timeline: t,
      approve: vi.fn(),
      answer: vi.fn(),
      resend: vi.fn(),
      discard: vi.fn(),
      openArtifact: vi.fn(),
    } as unknown as ChatActions;
    root = document.createElement('div');
    document.body.appendChild(root);
    act(() => render(<ChatItems items={t.items()} actions={actions} />, root));
    const line = $('.line-system-claude_signed_out')!;
    expect(line.textContent).toContain('Claude Code on home-mac is signed out');
    expect(line.querySelector('.warn-icon')).toBeTruthy();
    const a = line.querySelector<HTMLAnchorElement>('a.line-sign-in')!;
    expect(a.textContent?.trim()).toBe('Sign in');
    expect(a.getAttribute('href')).toBe('#/settings');
  });
});
