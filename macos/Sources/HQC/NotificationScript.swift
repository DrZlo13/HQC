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
// from the hub and none here. On every `requests` frame it posts the tags of all the open
// requests, `{open: [tag]}`: the app takes away the permission requests not among them.
//
// The same names make the window bar's breadcrumbs: the script posts `{crumbs}` to the
// `hqcChrome` handler (ChromeScript) — on `#/s/<id>` the session's project, its title and its
// state worded as the client's header pill (client/src/ui/labels.ts STATE_LABELS, ui/Chat.tsx
// StatePill; a session in VS Code or Claude Code without the pill's "on <machine>"), on
// `#/p/<id>…` the project, else none — when the address, a summary or the open requests change.
// The page's own `document.title` is always "HQC".
enum NotificationScript {

    static let handlerName = "hqcNotify"

    static let source = """
        (() => {
          const handler = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.hqcNotify;
          const Native = window.WebSocket;
          if (!handler || !Native) return;

          // Per session: its summary as the hub last reported it (unread, last_seq, and the
          // title, project and state for the window's bar).
          const summaries = new Map();
          // Per session: the newest event already handled, so two reads post an event once.
          const posted = new Map();
          // Per session: the read in progress; the next one starts after it.
          const reads = new Map();
          // The tags of the open requests in the last `requests` frame, and of all the requests
          // any frame listed open.
          let lastOpen = new Set();
          const everOpen = new Set();
          // The sessions with an open permission request in the last `requests` frame.
          let asking = new Set();
          // Project id -> name, from the client's own projects call.
          const projects = new Map();
          let projectsLoad = null;
          // The workers' service sessions (`system`) and every session id the list call knew,
          // from the client's own all-sessions call (client/src/api.ts allSessions()).
          const service = new Set();
          const listed = new Set();
          let sessionsLoad = null;

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
          // the personal project), the archived projects too (the first call leaves them out,
          // client/src/api.ts archivedProjects()); read once, and again for a project not known
          // yet.
          const projectList = (url) =>
            fetch(url)
              .then((res) => (res.ok ? res.json() : null))
              .then((v) => (v && v.projects) || [])
              .catch(() => []);
          const loadProjects = () => {
            if (!projectsLoad) {
              projectsLoad = Promise.all([projectList('/api/projects'), projectList('/api/projects?archived=1')])
                .then(([live, archived]) => {
                  for (const p of [...live, ...archived]) {
                    if (p && typeof p.id === 'string') projects.set(p.id, p.is_personal ? 'Personal' : p.name);
                  }
                })
                .finally(() => {
                  projectsLoad = null;
                  postCrumbs();
                });
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

          // The project open in the page (`#/p/<id>`, its new session, settings, rules), if any.
          const openProject = () => {
            const m = location.hash.match(/^#\\/p\\/([^/?]+)/);
            if (!m) return null;
            try { return decodeURIComponent(m[1]); } catch { return null; }
          };

          // A session's state as the client's header pill words it (ui/Chat.tsx StatePill): Running
          // also while a subagent or a background task works after the turn (ui/sessions.ts
          // atWorkAfterTurn); waiting for permission while it has an open permission request.
          const STATE_LABELS = new Map([
            ['archived', 'Archived'],
            ['locked_vscode', 'In VS Code'],
            ['worker_offline', 'Worker offline'],
            ['waiting', 'Waiting for reply'],
            ['error', 'Error'],
            ['running', 'Running'],
            ['queued', 'Queued'],
            ['idle', 'Idle'],
          ]);
          const status = (s) => {
            // The pill names the holder (ui/Chat.tsx `holder`); its "on <machine>" is not in a summary.
            if (s.state === 'locked_vscode') {
              return s.held_by && s.held_by.entrypoint !== 'claude-vscode' ? 'In Claude Code' : 'In VS Code';
            }
            if (s.state === 'waiting' && asking.has(s.session_id)) return 'Waiting for permission';
            if (s.state === 'idle' && ((s.subagents || 0) > 0 || (s.background_tasks || 0) > 0)) return 'Running';
            return STATE_LABELS.get(s.state) || 'Unknown';
          };

          // The window bar's breadcrumbs: the open session's project, title and state, else the
          // open project; a part not known (yet) is left out.
          const chrome = window.webkit.messageHandlers.hqcChrome;
          let lastCrumbs = null;
          const postCrumbs = () => {
            const session = openSession();
            const project = openProject();
            let crumbs = [];
            if (session) {
              const s = summaries.get(session);
              if (s) crumbs = [projects.get(s.project_id) || '', s.title || '', status(s)].filter(Boolean);
            } else if (project) {
              crumbs = [projects.get(project) || ''].filter(Boolean);
            }
            const key = JSON.stringify(crumbs);
            if (!chrome || key === lastCrumbs) return;
            lastCrumbs = key;
            chrome.postMessage({ crumbs });
          };
          // A project's name changes only by a rename, which the page does not hear of: the
          // names are read again whenever a project is opened, and for an opened session whose
          // project is not known (or whose summary has not come yet).
          const routed = () => {
            const session = openSession();
            const s = session && summaries.get(session);
            if (openProject() || (session && !(s && projects.has(s.project_id)))) loadProjects();
            postCrumbs();
          };
          window.addEventListener('hashchange', routed);
          routed();

          // The Dock badge: the sessions with something unread, archived and service sessions
          // aside, as the client's tab icon counts them for its light dot (client/src/favicon.ts).
          let lastBadge = null;
          const postBadge = () => {
            let n = 0;
            for (const s of summaries.values()) {
              if (s.unread > 0 && s.state !== 'archived' && !service.has(s.session_id)) n++;
            }
            if (!chrome || n === lastBadge) return;
            lastBadge = n;
            chrome.postMessage({ badge: n });
          };
          // Which sessions are service ones: read once, and again when a summary names a session
          // the last read did not list (a worker enrolled since).
          const loadSessions = () => {
            if (!sessionsLoad) {
              sessionsLoad = fetch('/api/sessions')
                .then((res) => (res.ok ? res.json() : null))
                .then((v) => {
                  for (const s of (v && v.sessions) || []) {
                    if (!s || typeof s.id !== 'string') continue;
                    listed.add(s.id);
                    if (s.system) service.add(s.id);
                  }
                })
                .catch(() => {})
                .finally(() => {
                  sessionsLoad = null;
                  postBadge();
                });
            }
            return sessionsLoad;
          };
          loadSessions();

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
              // Answered or gone already: an earlier `requests` frame listed it, the last one not.
              if (everOpen.has(tag) && !lastOpen.has(tag)) return;
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
              const was = summaries.get(s.session_id) || { unread: 0, last_seq: 0 };
              summaries.set(s.session_id, s);
              if (!baseline && !s.held_by && s.unread > was.unread && s.last_seq > was.last_seq) {
                read(s.session_id, was.last_seq, s.title, s.project_id);
              }
            }
          };

          // The open requests, whole: a permission request in Notification Center no longer among
          // them was answered or went away.
          const opened = (requests) => {
            const open = new Set();
            const permission = new Set();
            for (const r of requests || []) {
              if (!r || typeof r.session_id !== 'string' || typeof r.request_id !== 'string') continue;
              const tag = `${r.session_id}:${r.request_id}`;
              open.add(tag);
              everOpen.add(tag);
              if (r.event && r.event.type === 'approval.request') permission.add(r.session_id);
            }
            lastOpen = open;
            asking = permission;
            handler.postMessage({ open: [...open] });
            postCrumbs();
          };

          // After summaries: the badge, re-read once the list call has not listed one of them.
          const sessionsSeen = (sessions) => {
            const unknown = (sessions || []).some((s) => s && typeof s.session_id === 'string' && !listed.has(s.session_id));
            if (unknown && !sessionsLoad) loadSessions();
            else postBadge();
          };

          // `socket.baseline`: until this socket's first summary, its frames only set the baseline.
          const watch = (data, socket) => {
            let f;
            try { f = JSON.parse(data); } catch { return; }
            if (!f) return;
            if (f.type === 'summary') {
              // The whole list: a session no longer in it was deleted and counts no more.
              const ids = new Set((f.sessions || []).map((s) => s && s.session_id));
              for (const id of [...summaries.keys()]) if (!ids.has(id)) summaries.delete(id);
              update(f.sessions, socket.baseline);
              socket.baseline = false;
              postCrumbs();
              sessionsSeen(f.sessions);
            } else if (f.type === 'session.summary') {
              update([f.session], socket.baseline);
              postCrumbs();
              sessionsSeen([f.session]);
            } else if (f.type === 'requests') {
              opened(f.requests);
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
