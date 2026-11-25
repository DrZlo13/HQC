// @vitest-environment happy-dom
// SPEC 0.236: the provider form starts with the base URL; a pasted or typed one is read by a
// worker of the owner's, and the model list, the context window, the name and the label are
// filled where the owner has not typed. Every field stays editable.

import { afterEach, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { ApiError, type Api, type ProviderFields, type ProviderProbe } from '../src/api';
import type { AppState } from '../src/app';
import type { ModelProvider } from '../src/proto';
import { ProvidersSection, providerNameOf } from '../src/ui/Providers';

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
});

const PROBED: ProviderProbe = {
  models: [{ id: 'qwen3.8-27b', context_tokens: 131072 }, { id: 'Qwen3.8 Flash/Next' }],
  worker_id: 'w1',
};

function stubApp(api: Partial<Api>, providers: ModelProvider[] = []) {
  const saveProvider = vi.fn(async (_id: string | undefined, _f: ProviderFields) => {});
  return {
    app: { api, providers, saveProvider, deleteProvider: vi.fn(), workerName: () => undefined } as unknown as AppState,
    saveProvider,
  };
}

async function mount(app: AppState) {
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(<ProvidersSection app={app} />, root));
}

const $ = <T extends Element = HTMLElement>(sel: string) => root.querySelector<T>(sel);
const input = (name: string) => $<HTMLInputElement>(`input[name="provider_${name}"]`)!;
const settle = () => act(() => new Promise((r) => setTimeout(r, 0)));
async function type(el: HTMLInputElement, value: string) {
  await act(async () => {
    el.value = value;
    el.dispatchEvent(new Event('input', { bubbles: true }));
  });
}
async function paste(el: HTMLInputElement, value: string) {
  await act(async () => {
    el.dispatchEvent(new Event('paste', { bubbles: true }));
    el.value = value;
    el.dispatchEvent(new Event('input', { bubbles: true }));
  });
  await settle();
  await settle();
}
async function add() {
  await act(async () => $<HTMLButtonElement>('.btn-add-provider')!.click());
}
const status = () => $('.provider-probe')?.textContent ?? null;
const options = () => [...root.querySelectorAll<HTMLOptionElement>('datalist option')].map((o) => o.value);

it('the base URL comes first; a pasted one fills the models, the model, its context, the name and label', async () => {
  const probeProvider = vi.fn(async () => PROBED);
  const { app, saveProvider } = stubApp({ probeProvider });
  await mount(app);
  await add();
  const names = [...root.querySelectorAll<HTMLInputElement>('.provider-form input')].map((i) => i.name);
  expect(names).toEqual(['provider_base_url', 'provider_model', 'provider_context_tokens', 'provider_name', 'provider_label']);
  await paste(input('base_url'), 'http://192.168.1.229:8082');
  expect(probeProvider).toHaveBeenCalledWith('http://192.168.1.229:8082', undefined);
  expect(status()).toBe('2 models');
  // A free-text input with the models offered.
  const model = input('model');
  expect(model.getAttribute('list')).toBe($('datalist')!.id);
  expect(options()).toEqual(['qwen3.8-27b', 'Qwen3.8 Flash/Next']);
  expect(model.value).toBe('qwen3.8-27b');
  expect(input('context_tokens').value).toBe('131072');
  expect(input('name').value).toBe('qwen3-8-27b');
  expect(input('label').value).toBe('qwen3.8-27b');

  // Another model: its fields follow, the context it does not give is cleared.
  await type(model, 'Qwen3.8 Flash/Next');
  expect(input('name').value).toBe('qwen3-8-flash-next');
  expect(input('label').value).toBe('Qwen3.8 Flash/Next');
  expect(input('context_tokens').value).toBe('');

  await act(async () => $<HTMLButtonElement>('.btn-save-provider')!.click());
  expect(saveProvider).toHaveBeenCalledWith(undefined, {
    name: 'qwen3-8-flash-next',
    label: 'Qwen3.8 Flash/Next',
    base_url: 'http://192.168.1.229:8082',
    model: 'Qwen3.8 Flash/Next',
    context_tokens: undefined,
  });
});

it('keeps what the owner typed and sends the typed name to the probe', async () => {
  const probeProvider = vi.fn(async () => PROBED);
  const { app } = stubApp({ probeProvider });
  await mount(app);
  await add();
  await type(input('name'), 'qwen-local');
  await type(input('model'), 'my-model');
  await type(input('context_tokens'), '64000');
  await paste(input('base_url'), 'http://192.168.1.229:8082/');
  expect(probeProvider).toHaveBeenCalledWith('http://192.168.1.229:8082/', 'qwen-local');
  expect(options()).toHaveLength(2);
  expect(input('model').value).toBe('my-model');
  expect(input('context_tokens').value).toBe('64000');
  expect(input('name').value).toBe('qwen-local');
  // The label was not typed: it follows the model.
  expect(input('label').value).toBe('my-model');
});

it('a typed base URL is checked once typing settles, on blur at once', async () => {
  const probeProvider = vi.fn(async () => PROBED);
  const { app } = stubApp({ probeProvider });
  await mount(app);
  await add();
  await type(input('base_url'), 'http://192.168.1.229:80');
  await type(input('base_url'), 'http://192.168.1.229:8082');
  expect(probeProvider).not.toHaveBeenCalled();
  await act(() => new Promise((r) => setTimeout(r, 600)));
  expect(probeProvider).toHaveBeenCalledTimes(1);
  await type(input('base_url'), 'http://192.168.1.229:8083');
  await act(async () => void input('base_url').dispatchEvent(new Event('blur')));
  await settle();
  expect(probeProvider).toHaveBeenCalledTimes(2);
  expect(probeProvider).toHaveBeenLastCalledWith('http://192.168.1.229:8083', 'qwen3-8-27b');
});

it('says when no worker is online, and what the endpoint answered', async () => {
  const probeProvider = vi
    .fn<Api['probeProvider']>()
    .mockRejectedValueOnce(new ApiError(409, 'no worker of yours is online to read the endpoint', 'worker_offline'))
    .mockResolvedValueOnce({ models: [], error: 'home-mac: connection refused' });
  const { app } = stubApp({ probeProvider });
  await mount(app);
  await add();
  await paste(input('base_url'), 'http://192.168.1.229:8082');
  expect(status()).toBe('No worker is online to read it');
  expect($('.provider-probe')!.classList.contains('error-text')).toBe(true);
  await paste(input('base_url'), 'http://192.168.1.229:9999');
  expect(status()).toBe('home-mac: connection refused');
  expect(input('model').value).toBe('');
  expect(options()).toEqual([]);
});

it('an edit keeps every field the provider has', async () => {
  const probeProvider = vi.fn(async () => PROBED);
  const p = {
    id: 'pr1',
    name: 'qwen-local',
    label: 'Qwen (local)',
    base_url: 'http://192.168.1.229:8082',
    model: 'old-model',
    workers: [],
  } as unknown as ModelProvider;
  const { app } = stubApp({ probeProvider }, [p]);
  await mount(app);
  await act(async () => $<HTMLButtonElement>('.btn-edit-provider')!.click());
  await act(async () => void input('base_url').dispatchEvent(new Event('blur')));
  expect(probeProvider).not.toHaveBeenCalled();
  await paste(input('base_url'), 'http://192.168.1.229:8083');
  expect(options()).toHaveLength(2);
  expect(input('model').value).toBe('old-model');
  expect(input('name').value).toBe('qwen-local');
  expect(input('label').value).toBe('Qwen (local)');
  expect(input('context_tokens').value).toBe('');
});

it('names a provider after its model', () => {
  expect(providerNameOf('Qwen3.8 27B')).toBe('qwen3-8-27b');
  expect(providerNameOf('  --a//b__c--  ')).toBe('a-b__c');
  expect(providerNameOf('x'.repeat(70))).toHaveLength(64);
});
