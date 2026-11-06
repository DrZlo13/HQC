// Requests where the user is looking (T37, SPEC §10.1): a request of another session —
// the one this session waits on through `hqc_ask`, along the chain — drawn as that
// session's own card under a header naming it and its worker, and the "Requests" list of
// every open request of the user. Answers go to the request's own session.

import { useMemo } from 'preact/hooks';
import type { AppState } from '../app';
import type { OpenRequest } from '../proto';
import { Timeline } from '../timeline';
import { ClarifyCard, type ChatActions } from './cards';
import { useSubscription } from './hooks';
import { IconBack } from './icons';
import { PermissionBlock } from './permission';

/** What the request's card needs: answers through the app, no feed of its own. The empty
 *  timeline keeps the card open; once answered anywhere the request leaves the list. */
function requestActions(app: AppState, r: OpenRequest, timeline: Timeline): ChatActions {
  return {
    timeline,
    approve: (_id, o, m) => app.approveRequest(r, o, m),
    answer: (_id, a) => app.answerRequest(r, a),
    resend: () => {},
    discard: () => {},
    openArtifact: () => {},
    system: r.system,
  };
}

/** Whose request it is: the session (title, address) and its worker, with a link to it. */
function RequestHead({ r, current }: { r: OpenRequest; current?: string }) {
  const by = r.waiting.filter((w) => w.session_id !== current);
  return (
    <div class="req-head">
      <div class="req-from">
        <span class="req-label">{r.system ? 'Worker' : 'Request of'}</span>{' '}
        <a class="req-session" href={`#/s/${r.session_id}`} title="Open the session">
          {r.title || r.address}
        </a>
        {!r.system && <span class="req-address mono"> {r.address}</span>}
        {r.worker_name && !r.system && <span class="req-worker"> on {r.worker_name}</span>}
      </div>
      {by.length > 0 && (
        <div class="req-waiting muted">
          {current ? 'Also waiting: ' : 'Waiting on it: '}
          {by.map((w, i) => (
            <span key={w.session_id}>
              {i > 0 && ', '}
              <a href={`#/s/${w.session_id}`}>{w.title || w.address}</a>
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

/** Another session's request as its own card, under a header that names it. `current`: the
 *  session whose feed shows it. */
export function ForeignRequest({ app, r, current }: { app: AppState; r: OpenRequest; current?: string }) {
  const timeline = useMemo(() => new Timeline(), [r.session_id, r.request_id]);
  const actions = requestActions(app, r, timeline);
  return (
    <section
      class={`foreign-request ${current ? 'in-feed' : 'in-list'}`}
      aria-label={`Request of ${r.title || r.address}`}
      data-session={r.session_id}
      data-request={r.request_id}
    >
      <RequestHead r={r} current={current} />
      {r.event.type === 'approval.request' ? (
        <PermissionBlock event={r.event} actions={actions} />
      ) : (
        <ClarifyCard event={r.event} actions={actions} />
      )}
    </section>
  );
}

/** Under the "Waiting for a reply" line: the open requests of the sessions this one waits
 *  on. */
export function WaitedRequests({ app, sessionId }: { app: AppState; sessionId: string }) {
  const list = app.waitedRequests(sessionId);
  if (list.length === 0) return null;
  return (
    <div class="waited-requests">
      {list.map((r) => (
        <ForeignRequest key={`${r.session_id}/${r.request_id}`} app={app} r={r} current={sessionId} />
      ))}
    </div>
  );
}

/** The left column's entry (desktop) or the session list's button (phone): the count of
 *  open requests, nothing when there are none. */
export function RequestsLink({ app, current, phone }: { app: AppState; current?: boolean; phone?: boolean }) {
  const n = app.requests.length;
  if (n === 0 && !current) return null;
  return (
    <a
      class={`requests-link ${phone ? 'requests-link-phone' : ''} ${current ? 'is-current' : ''}`}
      href="#/requests"
      aria-label={`Requests: ${n}`}
      aria-current={current ? 'page' : undefined}
    >
      <span class="requests-label">Requests</span>
      {n > 0 && <span class="badge badge-request">{n > 99 ? '99+' : n}</span>}
    </a>
  );
}

/** "Requests": every open request of the user, the workers' own ones too; each answers in
 *  place and links to its session. */
export function RequestsScreen({ app, phone }: { app: AppState; phone?: boolean }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const list = app.requests;
  return (
    <section class="chat requests-screen">
      <header class="chat-head">
        {phone && (
          <a class="icon-btn icon-btn-round back" href="#/" aria-label="Back to sessions">
            <IconBack />
          </a>
        )}
        <div class="chat-head-main">
          <h1 class="chat-title">Requests</h1>
          <div class="chat-meta">{list.length ? `${list.length} open` : 'nothing waits for you'}</div>
        </div>
      </header>
      <div class="timeline">
        <div class="feed">
          {list.length === 0 ? (
            <div class="empty">No open requests</div>
          ) : (
            list.map((r) => <ForeignRequest key={`${r.session_id}/${r.request_id}`} app={app} r={r} />)
          )}
        </div>
      </div>
    </section>
  );
}
