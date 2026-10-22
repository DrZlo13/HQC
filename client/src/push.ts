// Web Push (SPEC §16): subscribe through PushManager with the hub's public VAPID key.
// The service worker (public/sw.js) shows notifications and opens the session on click.

import type { Api } from './api';

export type PushStatus =
  | 'unsupported' // no service worker / PushManager (e.g. iOS outside an installed PWA)
  | 'insecure' // not a secure context: plain HTTP on a LAN address
  | 'denied'
  | 'hub_off' // the hub has no VAPID keys (`public_key: null`)
  | 'subscribed'
  | 'available';

export function pushSupported(): boolean {
  return 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window;
}

export async function registerServiceWorker(): Promise<void> {
  if (!('serviceWorker' in navigator) || !isSecureContext) return;
  try {
    await navigator.serviceWorker.register('/sw.js');
  } catch (e) {
    console.warn('service worker registration failed', e);
  }
}

export async function pushStatus(): Promise<PushStatus> {
  if (!isSecureContext) return 'insecure';
  if (!pushSupported()) return 'unsupported';
  if (Notification.permission === 'denied') return 'denied';
  const reg = await navigator.serviceWorker.getRegistration();
  const sub = await reg?.pushManager.getSubscription();
  return sub ? 'subscribed' : 'available';
}

/** Asks for permission, subscribes and hands the subscription to the hub. Idempotent:
 *  an existing subscription is sent again, so the hub can re-learn it. */
export async function subscribePush(api: Api): Promise<PushStatus> {
  const st = await pushStatus();
  if (st === 'insecure' || st === 'unsupported' || st === 'denied') return st;
  const perm = await Notification.requestPermission();
  if (perm !== 'granted') return perm === 'denied' ? 'denied' : 'available';
  const reg = await navigator.serviceWorker.ready;
  let sub = await reg.pushManager.getSubscription();
  if (!sub) {
    const key = await api.vapidKey();
    if (key === null) return 'hub_off';
    sub = await reg.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey: base64UrlToBytes(key),
    });
  }
  await api.pushSubscribe(sub.toJSON());
  return 'subscribed';
}

export function base64UrlToBytes(s: string): Uint8Array<ArrayBuffer> {
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (s.length % 4)) % 4);
  const bin = atob(b64);
  const out = new Uint8Array(new ArrayBuffer(bin.length));
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** The service worker asks an open window to switch to a session instead of opening a new one. */
export function listenForNavigation(): void {
  navigator.serviceWorker?.addEventListener('message', (e: MessageEvent) => {
    const d = e.data as { type?: string; hash?: string } | null;
    if (d?.type === 'navigate' && typeof d.hash === 'string' && d.hash.startsWith('#/')) {
      location.hash = d.hash;
    }
  });
}
