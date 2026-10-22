// Service worker: Web Push only (SPEC §16). No offline cache: the client is useless without
// the hub, and a stale cached build talking to a newer hub is worse than a network error.
//
// Push payload (JSON) the client expects from the hub; see docs/notes/t5-questions.md:
//   { session_id, title?, body?, type?, tag? }
// A raw event of SPEC §4 is accepted too; title and body are then derived from it.

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (e) => e.waitUntil(self.clients.claim()));

function describe(d) {
  const p = d.payload || {};
  switch (d.type) {
    case 'approval.request':
      return { title: `Разрешение: ${p.tool || 'инструмент'}`, body: p.description || '' };
    case 'clarify.request':
      return { title: 'Агент задаёт вопрос', body: (p.questions && p.questions[0] && p.questions[0].text) || '' };
    case 'agent.message':
      return { title: 'Ответ агента', body: p.text || '' };
    case 'peer.message':
      return { title: 'Сообщение от другой сессии', body: p.text || '' };
    case 'system':
      return { title: 'hqc', body: p.text || '' };
    default:
      return { title: 'hqc', body: '' };
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
  const hash = sessionId ? `#/s/${encodeURIComponent(sessionId)}` : '#/';
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
