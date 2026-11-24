// @vitest-environment happy-dom
// SPEC 0.147 (docs/ITERATION-6.md T59): a secret or an ssh key for the session's agent — the
// composer's "Secret" (only where the worker takes secrets), the agent's `hqc_secret` card,
// the service line, the workspace's remembered keys on the rules screen. The value goes only
// into the request: never into the draft, the storage or the feed.

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ApiError, type Api, type SecretFields } from '../src/api';
import { AppState } from '../src/app';
import { parseEvent, type EventOf, type HqcEvent, type OpenRequest, type SessionInfo } from '../src/proto';
import { Timeline } from '../src/timeline';
import { ChatItems, type ChatActions } from '../src/ui/cards';
import { Chat } from '../src/ui/Chat';
import { RequestsScreen } from '../src/ui/requests';
import { RulesScreen } from '../src/ui/Rules';
import { secretNameError } from '../src/ui/secrets';
import type { HubSocket } from '../src/ws';

const WIRE = join(import.meta.dirname, '../../crates/hqc-proto/examples/wire');
const raw = (name: string): any => JSON.parse(readFileSync(join(WIRE, `${name}.json`), 'utf8'));
const ev = (name: string, seq: number, patch: (e: any) => void = () => {}): HqcEvent => {
  const e = raw(name);
  e.seq = seq;
  e.session_id = 's1';
  patch(e);
  return parseEvent(e);
};

const PASSWORD = 'correct horse battery staple';
const NOTE =
  'The secret WIFI_PASS is in the file "$HQC_SECRETS/WIFI_PASS" for as long as your process runs: read it in a command, as "$(cat "$HQC_SECRETS/WIFI_PASS")", and do not print it.';

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
  localStorage.clear();
  sessionStorage.clear();
});

const $ = <E extends Element = HTMLElement>(sel: string) => root.querySelector<E>(sel);
const $$ = (sel: string) => [...root.querySelectorAll<HTMLElement>(sel)];
const buttonByText = (text: string) => $$('button').find((b) => b.textContent?.trim() === text) as HTMLButtonElement | undefined;
const flush = () => act(async () => void (await new Promise((r) => setTimeout(r, 0))));
const typeInto = (el: HTMLInputElement | HTMLTextAreaElement, value: string) =>
  act(() => {
    el.value = value;
    el.dispatchEvent(new Event('input', { bubbles: true }));
  });
/** Nothing the client keeps holds `s`. */
const nowhere = (s: string) => {
  for (const st of [localStorage, sessionStorage]) {
    for (let i = 0; i < st.length; i++) expect(st.getItem(st.key(i)!) ?? '').not.toContain(s);
  }
};

describe('secret names', () => {
  it('are what the worker takes', () => {
    for (const ok of ['WIFI_PASS', 'pi-lab', 'a.b', 'x'.repeat(64)]) expect(secretNameError(ok), ok).toBeNull();
    for (const bad of ['', '.x', '-x', 'a b', 'a/b', 'x'.repeat(65), 'пароль']) expect(secretNameError(bad), bad).not.toBeNull();
  });
});

describe('the composer’s Secret', () => {
  function setup(opts: { secrets?: boolean; fail?: boolean; events?: HqcEvent[] } = {}) {
    const api = {
      session: vi.fn(async () => info),
      events: vi.fn(async () => ({ events: [], invalid: [], maxSeq: 0, count: 0 })),
      eventsBefore: vi.fn(async () => ({ events: opts.events ?? [], invalid: [], maxSeq: opts.events?.length ?? 0, minSeq: opts.events ? 1 : undefined, count: opts.events?.length ?? 0 })),
      readCursor: vi.fn(async () => {}),
      sessions: vi.fn(async () => []),
      projects: vi.fn(async () => []),
      workspaces: vi.fn(async () => []),
      giveSecret: vi.fn(async (_id: string, f: SecretFields, _requestId?: string) => {
        if (opts.fail) throw new ApiError(422, 'a wrong passphrase', 'secret_refused');
        return f.kind === 'secret'
          ? { note: NOTE, path: '$HQC_SECRETS/WIFI_PASS' }
          : { note: `The ssh key ${f.name} (SHA256:abc) is loaded in your ssh-agent.`, fingerprint: 'SHA256:abc', remembered: f.remember };
      }),
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
      title: 'camera',
      slug: 'camera',
      state: 'idle',
      queue_len: 0,
      unread: 0,
      last_seq: 0,
      last_read_seq: 0,
      archived: false,
      updated_at: '2026-11-15T19:47:00Z',
      system: false,
    };
    app.sessions.set('s1', info);
    if (opts.secrets !== false) app.secretWorkers = new Set(['wk1']);
    return { api, app };
  }
  async function mount(app: AppState) {
    root = document.createElement('div');
    document.body.appendChild(root);
    await act(async () => {
      render(<Chat app={app} sessionId="s1" />, root);
      await new Promise((r) => setTimeout(r, 50));
    });
  }
  const area = () => $<HTMLTextAreaElement>('.composer-input')!;

  const secretAsked = () =>
    ev('event.approval.request', 1, (e) => {
      e.author = { kind: 'system' };
      e.payload = {
        request_id: 'toolu_1',
        tool: 'hqc_secret',
        description: 'The Wi-Fi password',
        options: [{ id: 'deny', label: 'Decline', canonical: 'deny' }],
        secret: { name: 'WIFI_PASS', kind: 'secret' },
      };
    }) as EventOf<'approval.request'>;
  const openSecret = (event: EventOf<'approval.request'>): OpenRequest => ({
    session_id: 's1',
    request_id: 'toolu_1',
    kind: 'approval',
    address: 'home/camera',
    title: 'camera',
    worker_id: 'wk1',
    system: false,
    event,
    waiting: [],
  });

  it("the card of a request closes when the secret was given, before the hub's event", async () => {
    const asked = secretAsked();
    const { app } = setup({ events: [asked] });
    app.setRequests([openSecret(asked)]);
    await mount(app);
    await flush();
    await typeInto($<HTMLInputElement>('.secret-value')!, PASSWORD);
    await act(async () => buttonByText('Give')!.click());
    await flush();
    expect($('section[aria-label="Secret request"]')).toBeNull();
    expect($('.secret-done')!.textContent).toContain('given');
    expect(app.requests).toEqual([]);
  });

  it('a secret given on the Requests screen leaves the list at once', async () => {
    const { api, app } = setup();
    app.setRequests([openSecret(secretAsked())]);
    root = document.createElement('div');
    document.body.appendChild(root);
    await act(async () => render(<RequestsScreen app={app} />, root));
    await typeInto($<HTMLInputElement>('.foreign-request .secret-value')!, PASSWORD);
    await act(async () => buttonByText('Give')!.click());
    await flush();
    expect(api.giveSecret.mock.calls[0][0]).toBe('s1');
    expect(api.giveSecret.mock.calls[0][2]).toBe('toolu_1');
    expect($('.foreign-request')).toBeNull();
    expect(app.requests).toEqual([]);
  });

  it('is not offered where the worker does not take secrets', async () => {
    const { app } = setup({ secrets: false });
    await mount(app);
    expect($('.btn-secret')).toBeNull();
    expect($('.btn-attach')).not.toBeNull();
  });

  it('sends a password and puts the note, not the value, into the message', async () => {
    const { app, api } = setup();
    await mount(app);
    await typeInto(area(), 'Write it into the camera');
    await act(async () => $('.btn-secret')!.click());
    const form = $('.composer-secret form')!;
    // Not inside the composer's own form.
    expect(form.closest('form.composer')).toBeNull();
    const value = $<HTMLInputElement>('.secret-value')!;
    expect(value.type).toBe('password');
    expect(value.getAttribute('autocomplete')).toBe('current-password');
    expect(buttonByText('Give')!.disabled).toBe(true);
    await typeInto($<HTMLInputElement>('.secret-name')!, 'WIFI_PASS');
    await typeInto(value, PASSWORD);
    await act(async () => buttonByText('Give')!.click());
    await flush();
    expect(api.giveSecret).toHaveBeenCalledWith('s1', { kind: 'secret', name: 'WIFI_PASS', value: PASSWORD });
    expect($('.composer-secret')).toBeNull();
    expect(area().value).toBe(`Write it into the camera\n${NOTE}`);
    expect(root.innerHTML).not.toContain(PASSWORD);
    nowhere(PASSWORD);
    // Opened again: empty.
    await act(async () => $('.btn-secret')!.click());
    expect($<HTMLInputElement>('.secret-value')!.value).toBe('');
  });

  it('keeps the form filled on an error, and the value out of the draft and storage', async () => {
    const { app } = setup({ fail: true });
    await mount(app);
    await act(async () => $('.btn-secret')!.click());
    await act(async () => buttonByText('SSH key')!.click());
    expect($('.secret-value')).toBeNull();
    const key = $<HTMLTextAreaElement>('.secret-key')!;
    expect(key.getAttribute('autocomplete')).toBe('off');
    await typeInto($<HTMLInputElement>('.secret-name')!, 'pi-lab');
    await typeInto(key, '-----BEGIN OPENSSH PRIVATE KEY-----\nSECRETKEYDATA\n-----END OPENSSH PRIVATE KEY-----');
    await typeInto($<HTMLInputElement>('.secret-passphrase')!, 'wrongpass');
    await act(async () => $<HTMLInputElement>('.secret-remember input')!.click());
    await act(async () => buttonByText('Give')!.click());
    await flush();
    expect($('.composer-secret .error-text')!.textContent).toBe('a wrong passphrase');
    expect($<HTMLTextAreaElement>('.secret-key')!.value).toContain('SECRETKEYDATA');
    expect(area().value).toBe('');
    nowhere('SECRETKEYDATA');
    nowhere('wrongpass');
    // Cancel clears it.
    await act(async () => buttonByText('Cancel')!.click());
    await act(async () => $('.btn-secret')!.click());
    expect($<HTMLInputElement>('.secret-name')!.value).toBe('');
  });

  it('sends a key with its passphrase and remember', async () => {
    const { app, api } = setup();
    await mount(app);
    await act(async () => $('.btn-secret')!.click());
    await act(async () => buttonByText('SSH key')!.click());
    await typeInto($<HTMLInputElement>('.secret-name')!, 'pi-lab');
    await typeInto($<HTMLTextAreaElement>('.secret-key')!, 'KEY');
    await typeInto($<HTMLInputElement>('.secret-passphrase')!, 'pass');
    await act(async () => $<HTMLInputElement>('.secret-remember input')!.click());
    await act(async () => buttonByText('Give')!.click());
    await flush();
    expect(api.giveSecret).toHaveBeenCalledWith('s1', { kind: 'ssh_key', name: 'pi-lab', value: 'KEY', passphrase: 'pass', remember: true });
    expect(area().value).toBe('The ssh key pi-lab (SHA256:abc) is loaded in your ssh-agent.');
  });
});

describe('the agent’s hqc_secret card', () => {
  const asked = () =>
    ev('event.approval.request', 6, (e) => {
      e.author = { kind: 'system' };
      e.payload = {
        request_id: 'toolu_1',
        tool: 'hqc_secret',
        description: 'The Wi-Fi password to write into the camera config',
        options: [{ id: 'deny', label: 'Decline', canonical: 'deny' }],
        secret: { name: 'WIFI_PASS', kind: 'secret' },
      };
    });

  function show(over: Partial<ChatActions> = {}, more: HqcEvent[] = []) {
    const t = new Timeline();
    t.apply(asked());
    for (const e of more) t.apply(e);
    const actions: ChatActions = {
      timeline: t,
      approve: vi.fn(async () => {}),
      answer: vi.fn(async () => {}),
      resend: vi.fn(),
      discard: vi.fn(),
      openArtifact: vi.fn(),
      giveSecret: vi.fn(async () => {}),
      ...over,
    };
    root = document.createElement('div');
    document.body.appendChild(root);
    act(() => render(<ChatItems items={t.items()} actions={actions} />, root));
    return actions;
  }

  it('parses the request it asks for', () => {
    const e = asked();
    expect(e.type === 'approval.request' && e.payload.secret).toEqual({ name: 'WIFI_PASS', kind: 'secret' });
  });

  it('shows what is asked and why, and gives it with its request', async () => {
    const actions = show();
    const card = $('section[aria-label="Secret request"]')!;
    expect(card.textContent).toContain('The agent asks for a secret: WIFI_PASS');
    expect(card.textContent).toContain('The Wi-Fi password to write into the camera config');
    // Name and kind are the agent's.
    expect($('.secret-name')).toBeNull();
    expect(buttonByText('SSH key')).toBeUndefined();
    await typeInto($<HTMLInputElement>('.secret-value')!, PASSWORD);
    await act(async () => buttonByText('Give')!.click());
    await flush();
    expect(actions.giveSecret).toHaveBeenCalledWith('toolu_1', { kind: 'secret', name: 'WIFI_PASS', value: PASSWORD });
    expect(actions.approve).not.toHaveBeenCalled();
  });

  describe('the Vaultwarden source on the card', () => {
    const asksVault = (secret: object = { name: 'WIFI_PASS', kind: 'secret', vault: 'home-wifi' }) =>
      ev('event.approval.request', 6, (e) => {
        e.author = { kind: 'system' };
        e.payload = {
          request_id: 'toolu_1',
          tool: 'hqc_secret',
          description: 'The Wi-Fi password to write into the camera config',
          options: [{ id: 'deny', label: 'Decline', canonical: 'deny' }],
          secret,
        };
      });
    const showVault = (over: Partial<ChatActions> = {}, secret?: object) => {
      const t = new Timeline();
      t.apply(asksVault(secret));
      const actions: ChatActions = {
        timeline: t,
        approve: vi.fn(async () => {}),
        answer: vi.fn(async () => {}),
        resend: vi.fn(),
        discard: vi.fn(),
        openArtifact: vi.fn(),
        giveSecret: vi.fn(async () => {}),
        ...over,
      };
      root = document.createElement('div');
      document.body.appendChild(root);
      act(() => render(<ChatItems items={t.items()} actions={actions} />, root));
      return actions;
    };
    const entry = () => $<HTMLInputElement>('.secret-entry')!;

    it('parses the entry, and a card without one has none', () => {
      const e = asksVault();
      expect(e.type === 'approval.request' && e.payload.secret).toEqual({ name: 'WIFI_PASS', kind: 'secret', vault: 'home-wifi' });
      const plain = asked();
      expect(plain.type === 'approval.request' && plain.payload.secret?.vault).toBeUndefined();
    });

    it('shows it filled in, and gives the entry, not a value', async () => {
      const actions = showVault();
      const seg = (label: string) => $$('.secret-source [role="radio"]').find((b) => b.textContent === label)!;
      expect(seg('Vaultwarden').getAttribute('aria-checked')).toBe('true');
      expect(seg('Type it').getAttribute('aria-checked')).toBe('false');
      expect(entry().value).toBe('home-wifi');
      expect($('.secret-value')).toBeNull();
      expect($('.secret-hint')!.textContent).toContain('the value does not pass through hqc');
      await act(async () => buttonByText('Give')!.click());
      await flush();
      expect(actions.giveSecret).toHaveBeenCalledWith('toolu_1', { kind: 'secret', name: 'WIFI_PASS', vault: 'home-wifi' });
    });

    it('takes no focus when it opens on the entry, so a typed Enter gives nothing unseen', async () => {
      showVault();
      expect(document.activeElement).not.toBe(entry());
      expect(document.activeElement?.closest('.secret-form')).toBeNull();
    });

    it('typed mode keeps the focus on the value', async () => {
      show();
      await flush();
      expect(document.activeElement).toBe($('.secret-value'));
    });

    it('lets the owner change the entry, and needs one', async () => {
      const actions = showVault();
      await typeInto(entry(), '  ');
      expect(buttonByText('Give')!.disabled).toBe(true);
      await typeInto(entry(), ' wifi-camera ');
      await act(async () => buttonByText('Give')!.click());
      await flush();
      expect(actions.giveSecret).toHaveBeenCalledWith('toolu_1', { kind: 'secret', name: 'WIFI_PASS', vault: 'wifi-camera' });
    });

    it('"Type it" is the usual form, and gives a value', async () => {
      const actions = showVault();
      await act(async () => $$('.secret-source [role="radio"]').find((b) => b.textContent === 'Type it')!.click());
      expect($('.secret-entry')).toBeNull();
      await typeInto($<HTMLInputElement>('.secret-value')!, PASSWORD);
      await act(async () => buttonByText('Give')!.click());
      await flush();
      expect(actions.giveSecret).toHaveBeenCalledWith('toolu_1', { kind: 'secret', name: 'WIFI_PASS', value: PASSWORD });
    });

    describe('on a worker that reads Vaultwarden', () => {
      const seg = (label: string) => $$('.secret-source [role="radio"]').find((b) => b.textContent === label)!;
      it('offers the switch with no entry proposed, starts on "Type it" and focuses the value', async () => {
        const actions = showVault({}, { name: 'WIFI_PASS', kind: 'secret', reads_vault: true });
        await flush();
        expect(seg('Vaultwarden').getAttribute('aria-checked')).toBe('false');
        expect(seg('Type it').getAttribute('aria-checked')).toBe('true');
        expect(document.activeElement).toBe($('.secret-value'));
        // Switching over: an entry and a field of the owner's own.
        await act(async () => seg('Vaultwarden').click());
        await typeInto($<HTMLInputElement>('.secret-entry')!, 'home-wifi');
        await typeInto($<HTMLInputElement>('.secret-field')!, 'username');
        await act(async () => buttonByText('Give')!.click());
        await flush();
        expect(actions.giveSecret).toHaveBeenCalledWith('toolu_1', {
          kind: 'secret',
          name: 'WIFI_PASS',
          vault: 'home-wifi',
          vault_field: 'username',
        });
      });

      it('prefills the field the agent proposed and gives it', async () => {
        const actions = showVault({}, { name: 'WIFI_PASS', kind: 'secret', vault: 'home-wifi', vault_field: 'username', reads_vault: true });
        expect(seg('Vaultwarden').getAttribute('aria-checked')).toBe('true');
        expect($<HTMLInputElement>('.secret-field')!.value).toBe('username');
        expect($<HTMLInputElement>('.secret-field')!.getAttribute('aria-label')).toBe('Vaultwarden field');
        // Labelled, not masked, and the placeholder says what an empty field gives.
        expect($('.secret-field-label')!.textContent).toBe('Field');
        expect($<HTMLInputElement>('.secret-field')!.type).not.toBe('password');
        expect($<HTMLInputElement>('.secret-field')!.placeholder).toBe('empty: password');
        await act(async () => buttonByText('Give')!.click());
        await flush();
        expect(actions.giveSecret).toHaveBeenCalledWith('toolu_1', {
          kind: 'secret',
          name: 'WIFI_PASS',
          vault: 'home-wifi',
          vault_field: 'username',
        });
      });

      it('sends no field when the field is empty', async () => {
        const actions = showVault({}, { name: 'WIFI_PASS', kind: 'secret', vault: 'home-wifi', vault_field: 'username', reads_vault: true });
        await typeInto($<HTMLInputElement>('.secret-field')!, '  ');
        await act(async () => buttonByText('Give')!.click());
        await flush();
        expect(actions.giveSecret).toHaveBeenCalledWith('toolu_1', { kind: 'secret', name: 'WIFI_PASS', vault: 'home-wifi' });
      });

      it('says an empty field is the private key for an ssh key', () => {
        showVault({}, { name: 'pi-lab', kind: 'ssh_key', vault: 'pi-lab', reads_vault: true });
        expect($<HTMLInputElement>('.secret-field')!.placeholder).toBe('empty: private key');
      });

      it('has no field input on a card that names an entry but not reads_vault (an older worker)', async () => {
        const actions = showVault({}, { name: 'WIFI_PASS', kind: 'secret', vault: 'home-wifi' });
        expect(seg('Vaultwarden').getAttribute('aria-checked')).toBe('true');
        expect($('.secret-field')).toBeNull();
        await act(async () => buttonByText('Give')!.click());
        await flush();
        expect(actions.giveSecret).toHaveBeenCalledWith('toolu_1', { kind: 'secret', name: 'WIFI_PASS', vault: 'home-wifi' });
      });

      it('sends no field with a typed value', async () => {
        const actions = showVault({}, { name: 'WIFI_PASS', kind: 'secret', vault: 'home-wifi', vault_field: 'username' });
        await act(async () => seg('Type it').click());
        await typeInto($<HTMLInputElement>('.secret-value')!, PASSWORD);
        await act(async () => buttonByText('Give')!.click());
        await flush();
        expect(actions.giveSecret).toHaveBeenCalledWith('toolu_1', { kind: 'secret', name: 'WIFI_PASS', value: PASSWORD });
      });
    });
  });

  it('has no source switch without an entry', () => {
    show();
    expect($('.secret-source')).toBeNull();
    expect($('.secret-entry')).toBeNull();
  });

  it('declines through the request', async () => {
    const actions = show();
    await act(async () => buttonByText('Decline')!.click());
    await flush();
    expect(actions.approve).toHaveBeenCalledWith('toolu_1', 'deny');
    expect(actions.giveSecret).not.toHaveBeenCalled();
  });

  it('folds into "given" after the hub answered it, beside the service line', () => {
    show({}, [
      ev('event.system', 7, (e) => (e.payload = { text: 'Secret WIFI_PASS given', code: 'secret_given' })),
      ev('event.approval.response', 8, (e) => (e.payload = { request_id: 'toolu_1', option_id: 'give', by: 'u1' })),
    ]);
    expect($('section[aria-label="Secret request"]')).toBeNull();
    expect($('.secret-done')!.textContent).toContain('secret WIFI_PASS');
    expect($('.secret-done')!.textContent).toContain('given');
    expect(root.textContent).toContain('Secret WIFI_PASS given');
  });

  it('only declines where it cannot be given', () => {
    show({ giveSecret: undefined });
    expect($('.secret-value')).toBeNull();
    expect(buttonByText('Decline')).toBeDefined();
  });
});

describe('the workspace’s ssh keys', () => {
  function setup(opts: { secrets?: boolean; offline?: boolean } = {}) {
    let keys = [{ name: 'pi-kitchen', fingerprint: 'SHA256:mVPwvezndPv' }];
    const api = {
      workspaces: vi.fn(async () => [
        {
          id: 'w1',
          project_id: 'p1',
          worker_id: 'wk1',
          path: '/Users/me/work/pi',
          label: '',
          environment: 'default',
          kind: 'dir',
          mirror_enabled: true,
          hqc_rules: 0,
          created_at: '2026-10-20T10:00:00Z',
        },
      ]),
      workers: vi.fn(async () => [{ id: 'wk1', name: 'home-mac', status: 'online', session_secrets: opts.secrets !== false }]),
      workspaceRules: vi.fn(async () => []),
      workspacePermissions: vi.fn(async () => {
        throw new ApiError(409, 'the worker is offline', 'worker_offline');
      }),
      workspaceKeys: vi.fn(async () => {
        if (opts.offline) throw new ApiError(409, 'the worker is offline', 'worker_offline');
        return keys;
      }),
      deleteWorkspaceKey: vi.fn(async (_id: string, name: string) => {
        keys = keys.filter((k) => k.name !== name);
      }),
    };
    const socket = { onState: () => () => {}, subscribe: () => () => {} } as unknown as HubSocket;
    const app = new AppState(api as unknown as Api, socket);
    root = document.createElement('div');
    document.body.appendChild(root);
    act(() => render(<RulesScreen app={app} projectId="p1" workspaceId="w1" />, root));
    return api;
  }

  it('shows a spinner and "Loading…" until the keys are in', async () => {
    const api = setup();
    api.workspaceKeys.mockImplementation(() => new Promise(() => {}) as never);
    await flush();
    await flush();
    const loading = $('section[aria-label="SSH keys"] > .loading')!;
    expect(loading.textContent).toBe('Loading…');
    expect(loading.querySelector('.spinner')).not.toBeNull();
  });

  it('lists them and deletes one after a confirmation', async () => {
    const api = setup();
    await flush();
    await flush();
    const section = $('section[aria-label="SSH keys"]')!;
    expect(section.querySelector('.ws-key-name')!.textContent).toBe('pi-kitchen');
    expect(section.querySelector('.ws-key-fp')!.textContent).toBe('SHA256:mVPwvezndPv');
    await act(async () => $<HTMLButtonElement>('button[aria-label="Delete pi-kitchen"]')!.click());
    expect(api.deleteWorkspaceKey).not.toHaveBeenCalled();
    await act(async () => buttonByText('Delete pi-kitchen')!.click());
    await flush();
    expect(api.deleteWorkspaceKey).toHaveBeenCalledWith('w1', 'pi-kitchen');
    expect($('.ws-keys-none')!.textContent).toBe('No keys');
  });

  it('is not shown where the worker does not take secrets', async () => {
    const api = setup({ secrets: false });
    await flush();
    await flush();
    expect($('section[aria-label="SSH keys"]')).toBeNull();
    expect(api.workspaceKeys).not.toHaveBeenCalled();
  });

  it('says so when the worker is offline', async () => {
    setup({ offline: true });
    await flush();
    await flush();
    expect($('.ws-keys-off')!.textContent).toContain('the worker is offline');
  });
});
