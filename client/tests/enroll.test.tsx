// @vitest-environment happy-dom
// The enrollment screen (SPEC §5.1, §6): the worker's name, OS and time are shown before
// "Подтвердить" can be pressed; the button stays disabled until they are loaded.

import { afterEach, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { ApiError } from '../src/api';
import type { AppState } from '../src/app';
import type { EnrollInfo } from '../src/proto';
import { Enroll, normalizeCode } from '../src/ui/Enroll';

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
});

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((a, b) => ((resolve = a), (reject = b)));
  return { promise, resolve, reject };
}

function mount(api: Partial<AppState['api']>, code: string) {
  root = document.createElement('div');
  document.body.appendChild(root);
  const app = { api } as unknown as AppState;
  act(() => render(<Enroll app={app} initialCode={code} />, root));
}

const button = () => root.querySelector<HTMLButtonElement>('button[type=submit]')!;
const field = (f: string) => root.querySelector(`[data-field="${f}"]`)?.textContent;

it('normalizes codes', () => {
  expect(normalizeCode('abcd 1234')).toBe('ABCD-1234');
  expect(normalizeCode('ABCD-123')).toBeNull();
});

it('shows who asks and enables the button only after that', async () => {
  const info = deferred<EnrollInfo>();
  const enrollInfo = vi.fn(() => info.promise);
  const enrollConfirm = vi.fn(async () => ({ worker_id: 'w', name: 'attacker-box' }));
  mount({ enrollInfo, enrollConfirm }, 'abcd-1234');
  expect(enrollInfo).toHaveBeenCalledWith('ABCD-1234');
  expect(button().disabled).toBe(true);
  // Submitting before the info arrived does nothing.
  await act(async () => {
    root.querySelector('form')!.dispatchEvent(new Event('submit', { cancelable: true }));
  });
  expect(enrollConfirm).not.toHaveBeenCalled();

  await act(async () =>
    info.resolve({ name: 'attacker-box', os: 'linux', created_at: '2026-10-22T10:00:00Z', country: 'DE', replaceable: [] }),
  );
  expect(field('name')).toBe('attacker-box');
  expect(field('os')).toBe('linux');
  expect(field('country')).toBe('DE');
  expect(field('created_at')).toBeTruthy();
  expect(button().disabled).toBe(false);

  await act(async () => button().click());
  expect(enrollConfirm).toHaveBeenCalledWith('ABCD-1234', undefined);
  expect(root.querySelector('.outcome.ok')!.textContent).toContain('attacker-box');
});

it('offers to replace an offline or revoked worker and sends the choice (T19)', async () => {
  const enrollInfo = vi.fn(async () => ({
    name: 'home-mac',
    os: 'macos',
    created_at: '2026-10-22T10:00:00Z',
    replaceable: [
      { id: 'w-old', name: 'home-mac-old', os: 'macos', status: 'offline', sessions_count: 7 },
      { id: 'w-gone', name: 'llm-server', os: 'linux', status: 'revoked', sessions_count: 0 },
    ],
  }));
  const enrollConfirm = vi.fn(async () => ({ worker_id: 'w-old', name: 'home-mac', replaced: 'home-mac-old' }));
  mount({ enrollInfo, enrollConfirm }, 'abcd-1234');
  await act(async () => {});
  const radios = [...root.querySelectorAll<HTMLInputElement>('.enroll-replace input[type=radio]')];
  expect(radios.map((r) => r.value)).toEqual(['', 'w-old', 'w-gone']);
  // A new worker is the default.
  expect(radios[0].checked).toBe(true);
  const labels = [...root.querySelectorAll('.enroll-replace label')].map((l) => l.textContent);
  expect(labels[1]).toContain('Это та же машина, заменить «home-mac-old»');
  expect(labels[1]).toContain('сессий: 7');
  expect(labels[2]).toContain('отозван');

  await act(async () => {
    radios[1].click();
  });
  expect(button().textContent).toBe('Подтвердить замену');
  await act(async () => button().click());
  expect(enrollConfirm).toHaveBeenCalledWith('ABCD-1234', 'w-old');
  expect(root.querySelector('.outcome.ok')!.textContent).toContain('заменил «home-mac-old»');
});

it('confirms a new worker when nothing is chosen', async () => {
  const enrollInfo = vi.fn(async () => ({
    name: 'box',
    os: 'linux',
    created_at: '2026-10-22T10:00:00Z',
    replaceable: [{ id: 'w-old', name: 'old', os: 'linux', status: 'offline', sessions_count: 1 }],
  }));
  const enrollConfirm = vi.fn(async () => ({ worker_id: 'w-new', name: 'box' }));
  mount({ enrollInfo, enrollConfirm }, 'abcd-1234');
  await act(async () => {});
  await act(async () => button().click());
  expect(enrollConfirm).toHaveBeenCalledWith('ABCD-1234', undefined);
  expect(root.querySelector('.outcome.ok')!.textContent).toBe('Воркер «box» подключён.');
});

it('shows no choice without replaceable workers', async () => {
  const enrollInfo = vi.fn(async () => ({ name: 'box', os: 'linux', created_at: '2026-10-22T10:00:00Z', replaceable: [] }));
  mount({ enrollInfo, enrollConfirm: vi.fn() }, 'abcd-1234');
  await act(async () => {});
  expect(root.querySelector('.enroll-replace')).toBeNull();
  expect(button().disabled).toBe(false);
});

it('keeps the button disabled for an unknown code', async () => {
  const enrollInfo = vi.fn(async () => {
    throw new ApiError(404, 'not found', 'not_found');
  });
  mount({ enrollInfo, enrollConfirm: vi.fn() }, 'ZZZZ-9999');
  await act(async () => {});
  expect(root.querySelector('.error-text')!.textContent).toContain('не найден');
  expect(button().disabled).toBe(true);
  expect(root.querySelector('.enroll-info')).toBeNull();
});

it('asks nothing until the code is complete', () => {
  const enrollInfo = vi.fn();
  mount({ enrollInfo }, 'ABC');
  expect(enrollInfo).not.toHaveBeenCalled();
  expect(button().disabled).toBe(true);
});
