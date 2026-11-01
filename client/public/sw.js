// Service worker: Web Push only (SPEC §16). No offline cache: the client is useless without
// the hub, and a stale cached build talking to a newer hub is worse than a network error.
//
// Push body (SPEC §16, hqc_proto::api::PushMessage, examples/wire/api.push.json):
//   { session_id, type, title?, body?, tag?, payload?, open_session_id? }
// `payload` is the event of SPEC §4 (absent if it would not fit a push); without
// title/body they are derived from `type` and the event's payload.

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));

function describe(d) {
  const p = (d.payload && d.payload.payload) || {};
  switch (d.type) {
    case 'approval.request':
      return { title: `Permission: ${p.tool || 'tool'}`, body: p.description || '' };
    case 'clarify.request':
      return { title: 'The agent asks a question', body: (p.questions && p.questions[0] && p.questions[0].text) || '' };
    case 'agent.message':
      return { title: 'Agent reply', body: p.text || '' };
    case 'peer.message':
      return { title: 'Message from another session', body: p.text || '' };
    case 'system':
      return { title: 'HQC', body: p.text || '' };
    default:
      return { title: 'HQC', body: '' };
  }
}

self.addEventListener('push', (event) => {
  let d = {};
  try {
    d = event.data ? event.data.json() : {};
  } catch {
    d = { body: event.data ? event.data.text() : '' };
  }
  const derived = describe(d);
  const title = d.title || derived.title;
  const body = (d.body || derived.body || '').slice(0, 300);
  const sessionId = d.session_id;
  // A request of a session another one waits on opens where the user waits (T37); the tag
  // stays the request's own.
  const target = d.open_session_id || sessionId;
  const hash = target ? `#/s/${encodeURIComponent(target)}` : '#/';
  event.waitUntil(
    self.registration.showNotification(title, {
      body,
      tag: d.tag || (sessionId ? `${sessionId}:${d.type || 'event'}` : undefined),
      icon: '/icon-192.png',
      badge: '/icon-192.png',
      data: { hash },
      requireInteraction: d.type === 'approval.request' || d.type === 'clarify.request',
    }),
  );
});

self.addEventListener('notificationclick', (event) => {
  event.notification.close();
  const hash = (event.notification.data && event.notification.data.hash) || '#/';
  event.waitUntil(
    (async () => {
      const wins = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
      const own = wins.find((w) => new URL(w.url).origin === self.location.origin);
      if (own) {
        await own.focus();
        own.postMessage({ type: 'navigate', hash });
        return;
      }
      await self.clients.openWindow(`/${hash}`);
    })(),
  );
});
