// The page script behind the app's notifications, injected at document start into every tab's
// main frame. WKWebView has no service worker, so the browser's Web Push (client/public/sw.js)
// never reaches the app; instead the script watches the frames of the page's own `/api/ws`
// sockets. It keeps every session's `unread` and `last_seq` from `summary` and
// `session.summary` (each socket's first `summary` is a fresh baseline: what happened while
// it was not connected is not notified); when both grew, it reads the new events and posts
// each one the hub sends a push for (crates/hqc-hub/src/push.rs on_event; all of them count
// as unread, crates/hqc-hub/src/state.rs UNREAD_FILTER) to the `hqcNotify` handler for the
// app's card: the project's name, colour and tile letters, the session's title (sw.js
// describe() for an untitled one) and the event's whole text. A session mirrored from a CLI
// outside hqc (`held_by`) gets no push from the hub and none here. On every `requests` frame
// it posts the tags of all the open requests, `{open: [tag]}`: the app takes away the
// permission requests not among them.
//
// The same names make the window bar's breadcrumbs: the script posts `{crumbs}` to the
// `hqcChrome` handler (ChromeScript) — on `#/s/<id>` the session's project, its title and its
// state worded as the client's header pill (client/src/ui/labels.ts STATE_LABELS, ui/Chat.tsx
// StatePill; a session in VS Code or Claude Code without the pill's "on <machine>"), on
// `#/p/<id>…` the project, else none — when the address, a summary or the open requests change.
// The page's own `document.title` is always "HQC".
//
// For the Dock badge it posts `{badge: n}` to `hqcChrome`: the sessions with `unread > 0`,
// archived and service ones and a room's agents' sessions aside (a room counts for them), as
// client/src/favicon.ts picks them for its light dot. The service sessions, the rooms and their
// agents come from `GET /api/sessions`, read at load and again for a session the last read did
// not list.
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
          // Project id -> its name, colour (`#RRGGBB`) and the letters of its tile, from the
          // client's own projects call.
          const projects = new Map();
          let projectsLoad = null;
          // The workers' service sessions (`system`) and every session id the list call knew,
          // from the client's own all-sessions call (client/src/api.ts allSessions()).
          const service = new Set();
          const listed = new Set();
          // Rooms (`room_mode`) and their agents' sessions (`room_id`), from the same call: an
          // agent's final message gets no push, its post in the room does (push.rs on_event).
          const rooms = new Set();
          const members = new Set();
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
              case 'peer.message':
                return clip(p.text, 160);
              case 'system':
                return p.code === 'agent_start_failed' || p.code === 'input_rejected' ? clip(p.text, 160) : null;
              default:
                return null;
            }
          };

          // The events of session `id` the hub sends a push for (push.rs on_event): the requests,
          // the final reply (not of a room's agent), a post in a room and a `system` line about
          // input that failed. The other unread ones get no push.
          const pushed = (e, id) => {
            if (hubBody(e) === null) return false;
            if (e.type === 'agent.message') return (e.payload || {}).final === true && !members.has(id);
            if (e.type === 'peer.message') return rooms.has(id);
            return true;
          };

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

          // The project colours (client/src/theme.ts PROJECT_COLORS); one not known is the first.
          const COLORS = {
            teal: '#3FB8A8', plum: '#B57BD6', blue: '#6E9BFF', rose: '#E77A9A',
            moss: '#8DBA5B', coral: '#F08A5D', cyan: '#4FC3E8', slate: '#8A93A6',
          };
          // The first letters of a name (client/src/ui/sessions.ts projectInitials()).
          const initials = (name, personal) => {
            if (personal) return name.slice(0, 1).toUpperCase();
            const words = name.split(/[\\s\\-_./]+/).filter(Boolean);
            if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
            return (words[0] || name).slice(0, 2).toUpperCase();
          };

          // The projects, named as the client names them (projectName(): "Personal" for the
          // personal project), with their colour and what their tile shows (ui/Appearance.tsx
          // ProjectGlyph(): the emoji or letters set for it, else the first letters of the name;
          // for an image, the letters), the archived projects too (the first call leaves them
          // out, client/src/api.ts archivedProjects()); read once, and again for a project not
          // known yet.
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
                    if (!p || typeof p.id !== 'string') continue;
                    const name = p.is_personal ? 'Personal' : String(p.name || '');
                    const icon = p.icon && (p.icon.kind === 'emoji' || p.icon.kind === 'letters') ? String(p.icon.value || '') : '';
                    projects.set(p.id, {
                      name,
                      color: COLORS[p.color] || COLORS.teal,
                      glyph: icon || initials(name, p.is_personal),
                    });
                  }
                })
                .finally(() => {
                  projectsLoad = null;
                  postCrumbs();
                });
            }
            return projectsLoad;
          };
          const projectOf = async (id) => {
            if (id && !projects.has(id)) await loadProjects();
            return (id && projects.get(id)) || null;
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
          const nameOf = (id) => (projects.get(id) || {}).name || '';
          const postCrumbs = () => {
            const session = openSession();
            const project = openProject();
            let crumbs = [];
            if (session) {
              const s = summaries.get(session);
              if (s) crumbs = [nameOf(s.project_id), s.title || '', status(s)].filter(Boolean);
            } else if (project) {
              crumbs = [nameOf(project)].filter(Boolean);
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

          // The Dock badge: the sessions with something unread, archived and service sessions and
          // rooms' agents aside, as the client's tab icon counts them for its light dot
          // (client/src/favicon.ts).
          let lastBadge = null;
          const postBadge = () => {
            let n = 0;
            for (const s of summaries.values()) {
              if (s.unread > 0 && s.state !== 'archived' && !service.has(s.session_id) && !members.has(s.session_id)) n++;
            }
            if (!chrome || n === lastBadge) return;
            lastBadge = n;
            chrome.postMessage({ badge: n });
          };
          // Which sessions are service ones, rooms and rooms' agents: read once, and again when a
          // summary or an event read names a session the last read did not list (a worker enrolled
          // or a room made since). A session named while a read is in flight may be missing from
          // it (a room's agents are made one by one after the room): one more read follows it.
          let relist = false;
          const loadSessions = () => {
            if (!sessionsLoad) {
              sessionsLoad = fetch('/api/sessions')
                .then((res) => (res.ok ? res.json() : null))
                .then((v) => {
                  for (const s of (v && v.sessions) || []) {
                    if (!s || typeof s.id !== 'string') continue;
                    listed.add(s.id);
                    if (s.system) service.add(s.id);
                    if (s.room_mode) rooms.add(s.id);
                    if (s.room_id) members.add(s.id);
                  }
                })
                .catch(() => {})
                .finally(() => {
                  sessionsLoad = null;
                  if (relist) {
                    relist = false;
                    loadSessions();
                  } else postBadge();
                });
            }
            return sessionsLoad;
          };
          loadSessions();

          // The hub tells the page's socket nothing when a session or a project is deleted (the
          // row is gone, crates/hqc-hub/src/client_ws.rs SessionChanged), and the client drops
          // it from its own list (client/src/app.ts): the badge follows the client's successful
          // DELETE of a session or a project.
          const nativeFetch = window.fetch.bind(window);
          window.fetch = async (input, init) => {
            const res = await nativeFetch(input, init);
            try {
              const method = String((init && init.method) || (input instanceof Request ? input.method : 'GET')).toUpperCase();
              if (res.ok && method === 'DELETE') {
                const path = new URL(input instanceof Request ? input.url : String(input), location.href).pathname;
                let m;
                if ((m = path.match(/^\\/api\\/sessions\\/([^/]+)$/))) {
                  summaries.delete(decodeURIComponent(m[1]));
                } else if ((m = path.match(/^\\/api\\/projects\\/([^/]+)$/))) {
                  const project = decodeURIComponent(m[1]);
                  for (const [id, s] of [...summaries]) if (s.project_id === project) summaries.delete(id);
                }
                postBadge();
              }
            } catch {}
            return res;
          };

          // One notification per request; other events of a session replace each other (SPEC §16).
          // The card's text is the whole description (a permission request's command or path
          // first), question or message, up to 600 characters; a permission request names its
          // tool, a question counts the ones after the first. A permission request, a publication
          // included (the client publishes on a click on a button, as the card's Allow is),
          // carries the options of the card's Allow and Deny buttons.
          // A publication's text (the hub's publish.rs card()) for the card's few lines: its
          // fixed sentence gives way to the path; the files stay in the hub's order, which puts
          // the names that look like a secret first and the hidden files next, so
          // they are on the card first. Text of another form stays as it is.
          const publication = (d) => {
            const lines = d.split('\\n');
            const head = /^The agent asks to publish (.*) at a public link:/.exec(lines[0]);
            if (!head || lines.length < 2) return d;
            return [head[1], ...lines.slice(1)].join('\\n');
          };

          // The arguments of a call, as the client's callArgs() (client/src/ui/feed.ts):
          // `args_preview` is the input as JSON cut at 4 KB, so a long one may not parse; then
          // its whole string fields.
          const callArgs = (s) => {
            try {
              const v = JSON.parse(s);
              if (v && typeof v === 'object' && !Array.isArray(v)) return v;
            } catch {}
            const out = {};
            for (const m of String(s || '').matchAll(/"(\\w+)"\\s*:\\s*"((?:[^"\\\\]|\\\\.)*)"/g)) {
              try {
                out[m[1]] = JSON.parse(`"${m[2]}"`);
              } catch {
                out[m[1]] = m[2];
              }
            }
            return out;
          };

          // A permission request's text with the command or path first. The worker's description
          // is the CLI's description, then the command or path, cut at 4 KB (engine.rs
          // describe()); a Bash call's description is the agent's own text, and a long one would
          // fill the card's lines, or the whole 4 KB, before the command its Allow allows.
          // `calls`: the calls before the request with no result yet; one of the request's tool is
          // the request's (client/src/ui/feed.ts pairs them so), not for a subagent's request,
          // whose calls are not in the session's events. The command of the call that ends the
          // description as whole lines (the longest, of calls run side by side) comes first;
          // with no such call, a description cut by the worker and a single call of the tool,
          // that one's. Without a call, a description of two lines is the CLI's and the command,
          // so the last line is it; a longer or cut one does not tell where the command starts,
          // and the card says so first rather than show a line of it as the command. A worker's
          // own request is its description alone.
          const WHAT_KEYS = ['command', 'file_path', 'notebook_path', 'url', 'pattern'];
          // How many events before the unread ones a read starts, for the requests' calls.
          const LOOKBACK = 30;
          const WORKER_TOOLS = ['workspace.use', 'workspace.create', 'workspace.delete', 'session.follow'];
          const UNSEEN = 'Command unclear here: open the session to see it.';
          // Cut by the worker before the command: the session does not have it either.
          const CUT_OFF = 'Command cut off before it reached hqc: deny unless you know it.';
          const commandFirst = (e, calls) => {
            const p = e.payload || {};
            const d = p.description || '';
            if ((e.author && e.author.kind === 'system') || WORKER_TOOLS.includes(p.tool)) return d;
            // The request's own arguments (SPEC 0.207, `approval.request.args_preview`): the command
            // Allow allows, never one guessed from another call.
            const own = callArgs(p.args_preview);
            const mine = WHAT_KEYS.map((key) => own[key]).find((v) => typeof v === 'string' && v);
            if (mine) {
              const rest = d === mine ? '' : d.endsWith(`\\n${mine}`) ? d.slice(0, d.length - mine.length).trim() : d;
              return rest ? `${mine}\\n${rest}` : mine;
            }
            const whats = p.subagent
              ? []
              : calls
                  .filter((c) => c.payload && c.payload.tool === p.tool)
                  .map((c) => {
                    const args = callArgs(c.payload.args_preview);
                    const k = WHAT_KEYS.find((key) => typeof args[key] === 'string' && args[key]);
                    return k ? args[k] : '';
                  })
                  .filter(Boolean);
            const whole = whats
              .filter((w) => d === w || d.endsWith(`\\n${w}`))
              .sort((a, b) => b.length - a.length)[0];
            if (whole) {
              const rest = d.slice(0, d.length - whole.length).trim();
              return rest ? `${whole}\\n${rest}` : whole;
            }
            // engine.rs preview(): the worker cuts at 4096 bytes, on a character's boundary.
            const cut = new TextEncoder().encode(d).length >= 4093;
            // A cut description with no call that ends it whole: the command is not known here, and
            // a single pending call of the tool need not be the request's.
            const lines = d.split('\\n');
            if (lines.length < 2 && !cut) return d;
            if (lines.length === 2 && !cut) return `${lines[1]}\\n${lines[0]}`;
            return `${cut ? CUT_OFF : UNSEEN}\\n${d}`;
          };

          // A permission request's text with the characters that break a line or turn the
          // writing's direction shown as U+FFFD, as Claude Code's own dialog shows them: AppKit
          // breaks the card's line at U+2028 and U+2029, so a short command could push its
          // tail past the card's lines.
          const visible = (s) => s.replace(/[\\u2028\\u2029\\u061C\\u202A-\\u202E\\u2066-\\u2069]/g, '\\uFFFD');

          const post = (id, project, title, e, calls) => {
            const p = e.payload || {};
            const request = e.type === 'approval.request' || e.type === 'clarify.request';
            const tag = request && p.request_id ? `${id}:${p.request_id}` : `${id}:${e.type}`;
            const subtitle = title || describe(e).title;
            const name = project ? project.name : '';
            const m = {
              session_id: id,
              tag,
              type: e.type,
              title: name || subtitle,
              subtitle: name ? subtitle : '',
              body: clip(describe(e).body, 600),
              color: project ? project.color : COLORS.teal,
              glyph: project ? project.glyph : initials(subtitle, false),
              viewing: openSession() === id,
            };
            if (e.type === 'approval.request') m.tool = String(p.tool || 'tool');
            if (e.type === 'approval.request') m.body = clip(visible(p.tool === 'hqc_publish' ? publication(p.description || '') : commandFirst(e, calls)), 600);
            if (e.type === 'clarify.request' && Array.isArray(p.questions)) m.more = Math.max(0, p.questions.length - 1);
            if (e.type === 'approval.request' && p.request_id) {
              // Answered or gone already: an earlier `requests` frame listed it, the last one not.
              if (everOpen.has(tag) && !lastOpen.has(tag)) return;
              const options = Array.isArray(p.options) ? p.options : [];
              const once = options.find((o) => o && o.canonical === 'once');
              const deny = options.find((o) => o && o.canonical === 'deny') || options.find((o) => o && o.canonical === 'deny_always');
              m.request_id = p.request_id;
              m.allow_option = once ? once.id : null;
              m.deny_option = deny ? deny.id : null;
            }
            handler.postMessage(m);
          };

          const fetchNew = async (id, after, title, projectId) => {
            try {
              // From LOOKBACK events before `after`: the call a permission request is about comes
              // before it, and a call is not unread (crates/hqc-hub/src/state.rs), so its summary
              // moved `after` past it without a read. Only the events after `after` are posted.
              const from = Math.max(0, after - LOOKBACK);
              const res = await fetch(`/api/sessions/${encodeURIComponent(id)}/events?after_seq=${from}&limit=${50 + LOOKBACK}`);
              if (!res.ok) return;
              const page = await res.json();
              // A room or an agent made since the last list call: list again before deciding.
              if (!listed.has(id)) await loadSessions();
              if (!listed.has(id)) await loadSessions();
              // Each event with the calls before it that have no result yet.
              const fresh = [];
              const pending = new Map();
              for (const e of (page && page.events) || []) {
                if (!e) continue;
                const p = e.payload || {};
                if (e.type === 'tool.call') pending.set(p.call_id, e);
                if (e.type === 'tool.result') pending.delete(p.call_id);
                if (!(e.seq > Math.max(after, posted.get(id) || 0))) continue;
                posted.set(id, e.seq);
                if (pushed(e, id)) fresh.push([e, [...pending.values()]]);
              }
              if (!fresh.length) return;
              const project = await projectOf(projectId);
              for (const [e, calls] of fresh) post(id, project, title, e, calls);
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

          // The open requests, whole: a permission request no longer among them was answered or
          // went away, and its card goes.
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
            if (!unknown) postBadge();
            else if (sessionsLoad) relist = true;
            else loadSessions();
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
