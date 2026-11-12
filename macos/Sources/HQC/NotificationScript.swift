// The page script behind the app's notifications, injected at document start into every tab's
// main frame. WKWebView has no service worker, so the browser's Web Push (client/public/sw.js)
// never reaches the app; instead the script watches the frames of the page's own `/api/ws`
// sockets. It keeps every session's `unread` and `last_seq` from `summary` and
// `session.summary` (each socket's first `summary` is a fresh baseline: what happened while
// it was not connected is not notified); when both grew, it reads the new events and posts
// each one the hub sends a push for (crates/hqc-hub/src/push.rs on_event; all of them count
// as unread, crates/hqc-hub/src/state.rs UNREAD_FILTER) to the `hqcNotify` handler, worded as
// the push: the project's name as the title, the session's title (sw.js describe() for an
// untitled one) under it. A session mirrored from a CLI outside hqc (`held_by`) gets no push
// from the hub and none here. A permission request that the `requests` frame no longer lists
// is posted as `{resolved: tag}`.
enum NotificationScript {

    static let handlerName = "hqcNotify"

    static let source = """
        (() => {
          const handler = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.hqcNotify;
          const Native = window.WebSocket;
          if (!handler || !Native) return;

          // Per session: unread and last_seq as the hub last reported them.
          const kept = new Map();
          // Per session: the newest event already handled, so two reads post an event once.
          const posted = new Map();
          // Per session: the read in progress; the next one starts after it.
          const reads = new Map();
          // The tags of the permission requests posted and not yet seen answered.
          const approvals = new Set();
          // Project id -> name, from the client's own projects call.
          const projects = new Map();
          let projectsLoad = null;

          const clip = (s, n) => {
            const c = Array.from(s || '');
            return c.length <= n ? c.join('') : c.slice(0, n).join('') + '…';
          };

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

          // The project names, named as the client names them (projectName(): "Personal" for
          // the personal project); read once, and again for a project not known yet.
          const loadProjects = () => {
            if (!projectsLoad) {
              projectsLoad = fetch('/api/projects')
                .then((res) => (res.ok ? res.json() : null))
                .then((v) => {
                  for (const p of (v && v.projects) || []) {
                    if (p && typeof p.id === 'string') projects.set(p.id, p.is_personal ? 'Personal' : p.name);
                  }
                })
                .catch(() => {})
                .finally(() => { projectsLoad = null; });
            }
            return projectsLoad;
          };
          const projectName = async (id) => {
            if (id && !projects.has(id)) await loadProjects();
            return (id && projects.get(id)) || '';
          };

          // The session open in the page (`#/s/<id>`), if any.
          const openSession = () => {
            const m = location.hash.match(/^#\\/s\\/([^/?]+)/);
            if (!m) return null;
            try { return decodeURIComponent(m[1]); } catch { return null; }
          };

          // One notification per request; other events of a session replace each other (SPEC §16).
          // A permission request (not a publication: the client publishes only on a click on its
          // card) carries the options of the card's Allow and Deny buttons.
          const post = (id, project, title, e) => {
            const p = e.payload || {};
            const request = e.type === 'approval.request' || e.type === 'clarify.request';
            const tag = request && p.request_id ? `${id}:${p.request_id}` : `${id}:${e.type}`;
            const subtitle = title || describe(e).title;
            const m = {
              session_id: id,
              tag,
              type: e.type,
              title: project || subtitle,
              subtitle: project ? subtitle : '',
              body: (hubBody(e) || describe(e).body || '').slice(0, 300),
              viewing: openSession() === id,
            };
            if (e.type === 'approval.request' && p.request_id) {
              approvals.add(tag);
              if (p.tool !== 'hqc_publish') {
                const options = Array.isArray(p.options) ? p.options : [];
                const once = options.find((o) => o && o.canonical === 'once');
                const deny = options.find((o) => o && o.canonical === 'deny') || options.find((o) => o && o.canonical === 'deny_always');
                m.request_id = p.request_id;
                m.allow_option = once ? once.id : null;
                m.deny_option = deny ? deny.id : null;
              }
            }
            handler.postMessage(m);
          };

          const fetchNew = async (id, after, title, projectId) => {
            try {
              const res = await fetch(`/api/sessions/${encodeURIComponent(id)}/events?after_seq=${after}&limit=50`);
              if (!res.ok) return;
              const page = await res.json();
              const fresh = [];
              for (const e of (page && page.events) || []) {
                if (!e || !(e.seq > (posted.get(id) || 0))) continue;
                posted.set(id, e.seq);
                if (pushed(e)) fresh.push(e);
              }
              if (!fresh.length) return;
              const project = await projectName(projectId);
              for (const e of fresh) post(id, project, title, e);
            } catch {}
          };

          // Reads of one session run one after another: a later read finishing first would
          // mark the earlier one's events handled before they were posted.
          const read = (id, after, title, projectId) => {
            const next = (reads.get(id) || Promise.resolve()).then(() => fetchNew(id, after, title, projectId));
            reads.set(id, next);
          };

          const update = (sessions, baseline) => {
            for (const s of sessions || []) {
              if (!s || typeof s.session_id !== 'string') continue;
              const was = kept.get(s.session_id) || { unread: 0, last_seq: 0 };
              kept.set(s.session_id, { unread: s.unread, last_seq: s.last_seq });
              if (!baseline && !s.held_by && s.unread > was.unread && s.last_seq > was.last_seq) {
                read(s.session_id, was.last_seq, s.title, s.project_id);
              }
            }
          };

          // The open requests, whole: a posted permission request no longer among them was
          // answered or went away.
          const resolve = (requests) => {
            const open = new Set();
            for (const r of requests || []) {
              if (r && r.session_id && r.request_id) open.add(`${r.session_id}:${r.request_id}`);
            }
            for (const tag of [...approvals]) {
              if (open.has(tag)) continue;
              approvals.delete(tag);
              handler.postMessage({ resolved: tag });
            }
          };

          // `socket.baseline`: until this socket's first summary, its frames only set the baseline.
          const watch = (data, socket) => {
            let f;
            try { f = JSON.parse(data); } catch { return; }
            if (!f) return;
            if (f.type === 'summary') {
              update(f.sessions, socket.baseline);
              socket.baseline = false;
            } else if (f.type === 'session.summary') {
              update([f.session], socket.baseline);
            } else if (f.type === 'requests') {
              resolve(f.requests);
            }
          };

          class HqcWebSocket extends Native {
            constructor(url, protocols) {
              if (protocols === undefined) super(url);
              else super(url, protocols);
              if (String(url).endsWith('/api/ws')) {
                const socket = { baseline: true };
                this.addEventListener('message', (m) => watch(m.data, socket));
              }
            }
          }
          window.WebSocket = HqcWebSocket;
        })();
        """
}
