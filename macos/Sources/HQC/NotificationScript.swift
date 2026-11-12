// The page script behind the app's notifications, injected at document start into every tab's
// main frame. WKWebView has no service worker, so the browser's Web Push (client/public/sw.js)
// never reaches the app; instead the script watches the frames of the page's own `/api/ws`
// socket. It keeps every session's `unread` and `last_seq` from `summary` and
// `session.summary` (the page's first `summary` is the baseline); when both grew, it reads
// the new events and posts each one the hub sends a push for (crates/hqc-hub/src/push.rs
// on_event; all of them count as unread, crates/hqc-hub/src/state.rs UNREAD_FILTER) to the
// `hqcNotify` handler, worded as the push, with sw.js describe() for an untitled session.
enum NotificationScript {

    static let handlerName = "hqcNotify"

    static let source = """
        (() => {
          const handler = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.hqcNotify;
          const Native = window.WebSocket;
          if (!handler || !Native) return;

          // Per session: unread and last_seq as the hub last reported them.
          const kept = new Map();
          // Until the page's first summary, frames only set the baseline.
          let baseline = true;

          const clip = (s, n) => {
            const c = Array.from(s || '');
            return c.length <= n ? c.join('') : c.slice(0, n).join('') + '…';
          };

          // Per session: the newest event already posted, so two overlapping reads post it once.
          const posted = new Map();

          // The body of the hub's push (push.rs on_event); null where the hub sends no push.
          const hubBody = (e) => {
            const p = e.payload || {};
            switch (e.type) {
              case 'approval.request':
                return `Approval needed: ${p.tool} ${clip(p.description, 120)}`;
              case 'clarify.request':
                return p.questions && p.questions[0] ? `Question: ${clip(p.questions[0].text, 140)}` : 'The agent has a question';
              case 'agent.message':
                return clip(p.text, 160);
              case 'system':
                return p.code === 'agent_start_failed' || p.code === 'input_rejected' ? clip(p.text, 160) : null;
              default:
                return null;
            }
          };

          // The events the hub sends a push for (push.rs on_event): the requests, the final reply
          // and a `system` line about input that failed. The other unread ones get no push.
          const pushed = (e) => hubBody(e) !== null && (e.type !== 'agent.message' || (e.payload || {}).final === true);

          // sw.js describe(), on the event itself.
          const describe = (e) => {
            const p = e.payload || {};
            switch (e.type) {
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
          };

          // The session open in the page (`#/s/<id>`), if any.
          const openSession = () => {
            const m = location.hash.match(/^#\\/s\\/([^/?]+)/);
            if (!m) return null;
            try { return decodeURIComponent(m[1]); } catch { return null; }
          };

          // One notification per request; other events of a session replace each other (SPEC §16).
          const post = (id, title, e) => {
            const p = e.payload || {};
            const request = e.type === 'approval.request' || e.type === 'clarify.request';
            const d = describe(e);
            handler.postMessage({
              session_id: id,
              tag: request && p.request_id ? `${id}:${p.request_id}` : `${id}:${e.type}`,
              title: title || d.title,
              body: (hubBody(e) || d.body || '').slice(0, 300),
              viewing: openSession() === id,
            });
          };

          const fetchNew = async (id, after, title) => {
            try {
              const res = await fetch(`/api/sessions/${encodeURIComponent(id)}/events?after_seq=${after}&limit=50`);
              if (!res.ok) return;
              const page = await res.json();
              for (const e of (page && page.events) || []) {
                if (!e || !(e.seq > (posted.get(id) || 0))) continue;
                posted.set(id, e.seq);
                if (pushed(e)) post(id, title, e);
              }
            } catch {}
          };

          const update = (sessions) => {
            for (const s of sessions || []) {
              if (!s || typeof s.session_id !== 'string') continue;
              const was = kept.get(s.session_id) || { unread: 0, last_seq: 0 };
              kept.set(s.session_id, { unread: s.unread, last_seq: s.last_seq });
              if (!baseline && s.unread > was.unread && s.last_seq > was.last_seq) {
                fetchNew(s.session_id, was.last_seq, s.title);
              }
            }
          };

          const watch = (data) => {
            let f;
            try { f = JSON.parse(data); } catch { return; }
            if (!f) return;
            if (f.type === 'summary') {
              update(f.sessions);
              baseline = false;
            } else if (f.type === 'session.summary') {
              update([f.session]);
            }
          };

          class HqcWebSocket extends Native {
            constructor(url, protocols) {
              if (protocols === undefined) super(url);
              else super(url, protocols);
              if (String(url).endsWith('/api/ws')) this.addEventListener('message', (m) => watch(m.data));
            }
          }
          window.WebSocket = HqcWebSocket;
        })();
        """
}
