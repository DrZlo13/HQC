// REST client for the hub (SPEC §6). Auth is Cloudflare Access in front of the hub; the
// client sends cookies along and never handles tokens itself.

import {
  parseEvent,
  parseEventPage,
  parseInputResponse,
  parseMe,
  parseSessionInfo,
  parseSessionList,
  parseVapidKey,
  type ClarifyAnswer,
  type HqcEvent,
  type Me,
  type SessionInfo,
} from './proto';

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
    /** The hub's error code (`{error, message}`, SPEC §6), e.g. `request_closed`. */
    readonly code?: string,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

export type Fetch = typeof fetch;

/** A page of events; `invalid` are raw events that failed validation (shown, not dropped). */
export interface EventPage {
  events: HqcEvent[];
  invalid: { raw: unknown; error: string }[];
  /** Highest `seq` in the page, valid or not. */
  maxSeq: number;
  count: number;
}

export interface WorkerInfo {
  id: string;
  name: string;
  os?: string;
  status?: string;
  last_seen?: string;
}

export class Api {
  constructor(
    private readonly base = '',
    private readonly fetchImpl: Fetch = (...a) => fetch(...a),
  ) {}

  private async req(method: string, path: string, body?: unknown): Promise<unknown> {
    const res = await this.fetchImpl(this.base + path, {
      method,
      credentials: 'include',
      headers: body === undefined ? undefined : { 'content-type': 'application/json' },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await res.text();
    let data: unknown = undefined;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        data = text;
      }
    }
    if (!res.ok) {
      const body = typeof data === 'object' && data !== null ? (data as Record<string, unknown>) : {};
      const msg =
        typeof body.message === 'string'
          ? body.message
          : typeof data === 'string' && data
            ? data
            : res.statusText;
      const code = typeof body.error === 'string' ? body.error : undefined;
      throw new ApiError(res.status, msg || `HTTP ${res.status}`, code);
    }
    return data;
  }

  async me(): Promise<Me> {
    return parseMe(await this.req('GET', '/api/me'));
  }

  async sessions(projectId: string): Promise<SessionInfo[]> {
    return parseSessionList(await this.req('GET', `/api/projects/${enc(projectId)}/sessions`));
  }

  async createSession(projectId: string, title: string): Promise<SessionInfo> {
    const data = await this.req('POST', `/api/projects/${enc(projectId)}/sessions`, { title });
    return parseSessionInfo(data);
  }

  async session(id: string): Promise<SessionInfo> {
    return parseSessionInfo(await this.req('GET', `/api/sessions/${enc(id)}`));
  }

  async events(id: string, afterSeq: number, limit = 200): Promise<EventPage> {
    const q = `after_seq=${afterSeq}&limit=${limit}`;
    const data = await this.req('GET', `/api/sessions/${enc(id)}/events?${q}`);
    const page: EventPage = { events: [], invalid: [], maxSeq: afterSeq, count: 0 };
    for (const raw of parseEventPage(data).events) {
      page.count++;
      const seq = (raw as { seq?: unknown })?.seq;
      if (typeof seq === 'number') page.maxSeq = Math.max(page.maxSeq, seq);
      try {
        page.events.push(parseEvent(raw));
      } catch (e) {
        page.invalid.push({ raw, error: (e as Error).message });
      }
    }
    return page;
  }

  /** Returns the stored `user.message`; a repeated `client_op_id` returns the same event. */
  async input(id: string, text: string, clientOpId: string): Promise<HqcEvent> {
    const data = await this.req('POST', `/api/sessions/${enc(id)}/input`, {
      text,
      artifact_ids: [],
      client_op_id: clientOpId,
    });
    return parseInputResponse(data);
  }

  async stop(id: string): Promise<void> {
    await this.req('POST', `/api/sessions/${enc(id)}/stop`);
  }

  async retry(id: string): Promise<void> {
    await this.req('POST', `/api/sessions/${enc(id)}/retry`);
  }

  async approve(id: string, requestId: string, optionId: string): Promise<void> {
    await this.req('POST', `/api/sessions/${enc(id)}/requests/${enc(requestId)}`, {
      option_id: optionId,
    });
  }

  async clarify(id: string, requestId: string, answers: Record<string, ClarifyAnswer>) {
    await this.req('POST', `/api/sessions/${enc(id)}/requests/${enc(requestId)}`, { answers });
  }

  async readCursor(id: string, lastReadSeq: number): Promise<void> {
    await this.req('PUT', `/api/sessions/${enc(id)}/read_cursor`, { last_read_seq: lastReadSeq });
  }

  async artifactUrl(id: string): Promise<string> {
    const data = (await this.req('GET', `/api/artifacts/${enc(id)}/url`)) as { url?: string };
    if (typeof data?.url !== 'string') throw new ApiError(500, 'artifact url missing');
    return data.url;
  }

  async workers(): Promise<WorkerInfo[]> {
    const data = (await this.req('GET', '/api/workers')) as { workers?: unknown };
    if (!Array.isArray(data?.workers)) throw new ApiError(500, 'workers: expected a list');
    return data.workers as WorkerInfo[];
  }

  /** Revokes the worker's token and drops its connection (SPEC §5.1). */
  async revokeWorker(id: string): Promise<void> {
    await this.req('DELETE', `/api/workers/${enc(id)}`);
  }

  async enrollConfirm(code: string): Promise<unknown> {
    return this.req('POST', '/api/workers/enroll/confirm', { code });
  }

  /** `null`: the hub has no VAPID keys, push is off. */
  async vapidKey(): Promise<string | null> {
    return parseVapidKey(await this.req('GET', '/api/push/vapid'));
  }

  async pushSubscribe(sub: PushSubscriptionJSON): Promise<void> {
    await this.req('POST', '/api/push/subscribe', sub);
  }
}

const enc = encodeURIComponent;
