// Event and frame schema (SPEC §3 Event, §4, §5, §6), mirrored from crates/hqc-proto.
// The contract is crates/hqc-proto/examples/wire/*.json; tests/proto.test.ts parses all of it.
//
// Tolerance rules (SPEC §4): unknown payload fields are ignored, an event of unknown `type`
// parses as `UnknownEvent`, an unknown enum value inside an event reads as "unknown".
// An event of a known `type` with a wrong payload is an error (`ProtoError`).
// Worker <-> hub frames are strict (SPEC §5.2): unknown frame `type` is an error.

export class ProtoError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'ProtoError';
  }
}

export type Uuid = string;
export type Unknown = 'unknown';

// ---- enums ----------------------------------------------------------------------------

export const SESSION_STATES = [
  'archived',
  'locked_vscode',
  'worker_offline',
  'waiting',
  'error',
  'running',
  'queued',
  'idle',
] as const;
export type SessionState = (typeof SESSION_STATES)[number] | Unknown;

export const CANONICALS = ['once', 'session', 'always', 'deny', 'deny_always'] as const;
export type ApprovalCanonical = (typeof CANONICALS)[number] | Unknown;

export const POLICY_HINTS = ['allow', 'deny'] as const;
export type PolicyHint = (typeof POLICY_HINTS)[number] | Unknown;

export const CANCEL_REASONS = ['timeout', 'stopped', 'worker_restart', 'archived'] as const;
export type CancelReason = (typeof CANCEL_REASONS)[number] | Unknown;

export const PEER_MODES = ['send', 'ask'] as const;
export type PeerMode = (typeof PEER_MODES)[number] | Unknown;

export const ARTIFACT_KINDS = ['file', 'image', 'diff', 'html', 'text'] as const;
export type ArtifactKind = (typeof ARTIFACT_KINDS)[number] | Unknown;

export const DELIVERIES = ['pending', 'delivered', 'failed'] as const;
export type Delivery = (typeof DELIVERIES)[number];

export const PROCESS_STATES = ['stopped', 'starting', 'running', 'stopping'] as const;
export type ProcessState = (typeof PROCESS_STATES)[number];

export const LOCKED_BY = ['none', 'hqc', 'vscode'] as const;
export type LockedBy = (typeof LOCKED_BY)[number];

export const START_MODES = ['new', 'resume'] as const;
export type StartMode = (typeof START_MODES)[number];

// ---- event envelope -------------------------------------------------------------------

export type Author =
  | { kind: 'user'; id: Uuid }
  | { kind: 'agent'; id: Uuid }
  | { kind: 'system' };

export interface Chain {
  chain_id: Uuid;
  depth: number;
  hops: number;
}

export interface EventBase {
  session_id: Uuid;
  event_id: Uuid;
  /** Assigned by the hub; absent only on `agent.delta` (SPEC §5.2, §6). */
  seq?: number;
  ts: string;
  author: Author;
  chain?: Chain;
  client_op_id?: string;
  delivery?: Delivery;
  delivered_at?: string;
}

// ---- payloads (SPEC §4) ---------------------------------------------------------------

export interface UserMessage {
  text: string;
  artifact_ids: Uuid[];
}
export interface AgentDelta {
  text: string;
}
export interface AgentMessage {
  text: string;
  final: boolean;
  in_reply_to?: Uuid;
}
export interface AgentThinking {
  text: string;
}
export interface ToolCall {
  call_id: string;
  tool: string;
  args_preview: string;
}
export interface ToolResult {
  call_id: string;
  result_preview: string;
  error: boolean;
  error_text?: string;
  duration_ms?: number;
  artifact_id?: Uuid;
}
export interface ApprovalOption {
  id: string;
  label: string;
  canonical: ApprovalCanonical;
}
export interface ApprovalRequest {
  request_id: string;
  tool: string;
  description: string;
  options: ApprovalOption[];
  expires_at?: string;
  /** A rule of the worker's `[permissions]` matches the call (SPEC §8.1, §14). */
  policy_hint?: PolicyHint;
}
export interface ApprovalResponse {
  request_id: string;
  option_id: string;
  by: string;
}
export interface RequestCancel {
  request_id: string;
  reason: CancelReason;
}
export interface ClarifyQuestion {
  id: string;
  text: string;
  options?: string[];
  multi?: boolean;
}
export interface ClarifyRequest {
  request_id: string;
  questions: ClarifyQuestion[];
}
export type ClarifyAnswer = string | string[];
export interface ClarifyResponse {
  request_id: string;
  answers: Record<string, ClarifyAnswer>;
}
export interface Attachment {
  artifact_id: Uuid;
  kind: ArtifactKind;
  name: string;
  caption?: string;
}
export interface PeerMessage {
  from_session_id: Uuid;
  text: string;
  mode: PeerMode;
  request_id?: string;
}
export interface Usage {
  input_tokens: number;
  output_tokens: number;
  cache_read?: number;
  cache_write?: number;
  cost_usd?: number;
  model?: string;
}
export interface Status {
  state: SessionState;
  detail?: string;
}
export interface SystemNote {
  text: string;
  code: string;
}

export interface PayloadByType {
  'user.message': UserMessage;
  'agent.delta': AgentDelta;
  'agent.message': AgentMessage;
  'agent.thinking': AgentThinking;
  'tool.call': ToolCall;
  'tool.result': ToolResult;
  'approval.request': ApprovalRequest;
  'approval.response': ApprovalResponse;
  'approval.cancel': RequestCancel;
  'clarify.request': ClarifyRequest;
  'clarify.response': ClarifyResponse;
  'clarify.cancel': RequestCancel;
  attachment: Attachment;
  'peer.message': PeerMessage;
  usage: Usage;
  status: Status;
  system: SystemNote;
}
export type EventType = keyof PayloadByType;

export type KnownEvent = {
  [T in EventType]: EventBase & { type: T; payload: PayloadByType[T] };
}[EventType];

/** An event whose `type` this build does not know (SPEC §4): kept as is, not rendered. */
export interface UnknownEvent extends EventBase {
  type: 'unknown';
  wire_type: string;
  payload: unknown;
}

export type HqcEvent = KnownEvent | UnknownEvent;
export type EventOf<T extends EventType> = Extract<KnownEvent, { type: T }>;

// ---- field readers --------------------------------------------------------------------

type Obj = Record<string, unknown>;

function isObj(v: unknown): v is Obj {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function obj(v: unknown, what: string): Obj {
  if (!isObj(v)) throw new ProtoError(`${what}: expected an object`);
  return v;
}

function present(o: Obj, k: string): boolean {
  return o[k] !== undefined && o[k] !== null;
}

function str(o: Obj, k: string): string {
  const v = o[k];
  if (typeof v !== 'string') throw new ProtoError(`${k}: expected a string`);
  return v;
}

function optStr(o: Obj, k: string): string | undefined {
  return present(o, k) ? str(o, k) : undefined;
}

function num(o: Obj, k: string): number {
  const v = o[k];
  if (typeof v !== 'number' || !Number.isFinite(v)) throw new ProtoError(`${k}: expected a number`);
  return v;
}

function optNum(o: Obj, k: string): number | undefined {
  return present(o, k) ? num(o, k) : undefined;
}

function bool(o: Obj, k: string): boolean {
  const v = o[k];
  if (typeof v !== 'boolean') throw new ProtoError(`${k}: expected a bool`);
  return v;
}

function optBool(o: Obj, k: string): boolean | undefined {
  return present(o, k) ? bool(o, k) : undefined;
}

function arr(o: Obj, k: string): unknown[] {
  const v = o[k];
  if (!Array.isArray(v)) throw new ProtoError(`${k}: expected an array`);
  return v;
}

function strArr(o: Obj, k: string): string[] {
  return arr(o, k).map((v, i) => {
    if (typeof v !== 'string') throw new ProtoError(`${k}[${i}]: expected a string`);
    return v;
  });
}

/** Tolerant enum: a string outside `values` reads as "unknown"; a non-string is an error. */
function tolerantEnum<T extends string>(o: Obj, k: string, values: readonly T[]): T | Unknown {
  const v = str(o, k);
  return (values as readonly string[]).includes(v) ? (v as T) : 'unknown';
}

/** Strict enum, for fields outside the SPEC §4 tolerance list. */
function strictEnum<T extends string>(o: Obj, k: string, values: readonly T[]): T {
  const v = str(o, k);
  if (!(values as readonly string[]).includes(v)) throw new ProtoError(`${k}: unknown value ${v}`);
  return v as T;
}

// ---- payload parsers ------------------------------------------------------------------

const cancel = (p: Obj): RequestCancel => ({
  request_id: str(p, 'request_id'),
  reason: tolerantEnum(p, 'reason', CANCEL_REASONS),
});

const PAYLOAD_PARSERS: { [T in EventType]: (p: Obj) => PayloadByType[T] } = {
  'user.message': (p) => ({
    text: str(p, 'text'),
    artifact_ids: present(p, 'artifact_ids') ? strArr(p, 'artifact_ids') : [],
  }),
  'agent.delta': (p) => ({ text: str(p, 'text') }),
  'agent.message': (p) => ({
    text: str(p, 'text'),
    final: bool(p, 'final'),
    in_reply_to: optStr(p, 'in_reply_to'),
  }),
  'agent.thinking': (p) => ({ text: str(p, 'text') }),
  'tool.call': (p) => ({
    call_id: str(p, 'call_id'),
    tool: str(p, 'tool'),
    args_preview: str(p, 'args_preview'),
  }),
  'tool.result': (p) => ({
    call_id: str(p, 'call_id'),
    result_preview: str(p, 'result_preview'),
    error: bool(p, 'error'),
    error_text: optStr(p, 'error_text'),
    duration_ms: optNum(p, 'duration_ms'),
    artifact_id: optStr(p, 'artifact_id'),
  }),
  'approval.request': (p) => ({
    request_id: str(p, 'request_id'),
    tool: str(p, 'tool'),
    description: str(p, 'description'),
    options: arr(p, 'options').map((v, i) => {
      const o = obj(v, `options[${i}]`);
      return {
        id: str(o, 'id'),
        label: str(o, 'label'),
        canonical: tolerantEnum(o, 'canonical', CANONICALS),
      };
    }),
    expires_at: optStr(p, 'expires_at'),
    policy_hint: present(p, 'policy_hint') ? tolerantEnum(p, 'policy_hint', POLICY_HINTS) : undefined,
  }),
  'approval.response': (p) => ({
    request_id: str(p, 'request_id'),
    option_id: str(p, 'option_id'),
    by: str(p, 'by'),
  }),
  'approval.cancel': cancel,
  'clarify.request': (p) => ({
    request_id: str(p, 'request_id'),
    questions: arr(p, 'questions').map((v, i) => {
      const q = obj(v, `questions[${i}]`);
      return {
        id: str(q, 'id'),
        text: str(q, 'text'),
        options: present(q, 'options') ? strArr(q, 'options') : undefined,
        multi: optBool(q, 'multi'),
      };
    }),
  }),
  'clarify.response': (p) => ({
    request_id: str(p, 'request_id'),
    answers: parseAnswers(p),
  }),
  'clarify.cancel': cancel,
  attachment: (p) => ({
    artifact_id: str(p, 'artifact_id'),
    kind: tolerantEnum(p, 'kind', ARTIFACT_KINDS),
    name: str(p, 'name'),
    caption: optStr(p, 'caption'),
  }),
  'peer.message': (p) => ({
    from_session_id: str(p, 'from_session_id'),
    text: str(p, 'text'),
    mode: tolerantEnum(p, 'mode', PEER_MODES),
    request_id: optStr(p, 'request_id'),
  }),
  usage: (p) => ({
    input_tokens: num(p, 'input_tokens'),
    output_tokens: num(p, 'output_tokens'),
    cache_read: optNum(p, 'cache_read'),
    cache_write: optNum(p, 'cache_write'),
    cost_usd: optNum(p, 'cost_usd'),
    model: optStr(p, 'model'),
  }),
  status: (p) => ({ state: tolerantEnum(p, 'state', SESSION_STATES), detail: optStr(p, 'detail') }),
  system: (p) => ({ text: str(p, 'text'), code: str(p, 'code') }),
};

function parseAnswers(p: Obj): Record<string, ClarifyAnswer> {
  const a = obj(p.answers, 'answers');
  const out: Record<string, ClarifyAnswer> = {};
  for (const [k, v] of Object.entries(a)) {
    if (typeof v === 'string') out[k] = v;
    else if (Array.isArray(v) && v.every((x) => typeof x === 'string')) out[k] = v as string[];
    else throw new ProtoError(`answers.${k}: expected a string or an array of strings`);
  }
  return out;
}

export function isKnownType(t: string): t is EventType {
  return Object.prototype.hasOwnProperty.call(PAYLOAD_PARSERS, t);
}

function parseAuthor(v: unknown): Author {
  const a = obj(v, 'author');
  const kind = str(a, 'kind');
  if (kind === 'user' || kind === 'agent') return { kind, id: str(a, 'id') };
  if (kind === 'system') return { kind };
  throw new ProtoError(`author.kind: unknown value ${kind}`);
}

function parseChain(v: unknown): Chain {
  const c = obj(v, 'chain');
  return { chain_id: str(c, 'chain_id'), depth: num(c, 'depth'), hops: num(c, 'hops') };
}

/** Parses a stored or streamed event (SPEC §3 Event). Throws `ProtoError` on a bad envelope
 *  or on a bad payload of a known type. */
export function parseEvent(json: unknown): HqcEvent {
  const e = obj(json, 'event');
  const base: EventBase = {
    session_id: str(e, 'session_id'),
    event_id: str(e, 'event_id'),
    seq: optNum(e, 'seq'),
    ts: str(e, 'ts'),
    author: parseAuthor(e.author),
    chain: present(e, 'chain') ? parseChain(e.chain) : undefined,
    client_op_id: optStr(e, 'client_op_id'),
    delivery: present(e, 'delivery') ? strictEnum(e, 'delivery', DELIVERIES) : undefined,
    delivered_at: optStr(e, 'delivered_at'),
  };
  const type = str(e, 'type');
  if (!isKnownType(type)) return { ...base, type: 'unknown', wire_type: type, payload: e.payload };
  const payload = PAYLOAD_PARSERS[type](obj(e.payload, `${type} payload`));
  return { ...base, type, payload } as KnownEvent;
}

// ---- worker <-> hub frames (SPEC §5.3, §5.4) -------------------------------------------
// The client never speaks this protocol; the parser exists so the client and the Rust side
// share one checked contract (examples/wire).

export interface Frame<T extends string = string, P = Obj> {
  id: string;
  type: T;
  payload: P;
}

const FRAME_PARSERS: Record<string, (p: Obj) => void> = {
  hello: (p) => {
    num(p, 'protocol_version');
    str(p, 'boot_id');
    obj(p.capabilities, 'capabilities');
    strArr(p, 'environments');
    arr(p, 'env_bindings');
    strArr(p, 'allowed_roots');
    bool(p, 'allow_foreign_sessions');
    bool(p, 'share_local_models');
    arr(p, 'local_models');
  },
  'hello.ack': (p) => {
    num(p, 'protocol_version');
    str(p, 'worker_id');
    str(p, 'server_time');
  },
  event: (p) => {
    const ev = parseEvent(p.event);
    if (ev.seq !== undefined) throw new ProtoError('event frame: seq is assigned by the hub');
  },
  ack: (p) => {
    optStr(p, 'event_id');
    optNum(p, 'seq');
  },
  error: (p) => {
    str(p, 'code');
    str(p, 'message');
    bool(p, 'retryable');
  },
  result: () => {},
  'session.start': (p) => {
    str(p, 'session_id');
    strictEnum(p, 'mode', START_MODES);
    str(p, 'runtime');
    optStr(p, 'acp_agent');
    optStr(p, 'model');
    optStr(p, 'workspace_path');
    str(p, 'cwd');
    str(p, 'environment');
    optStr(p, 'runtime_session_id');
    optStr(p, 'system_prompt');
    strArr(p, 'disallowed_tools');
    bool(p, 'restricted');
    bool(p, 'auto_approve');
    str(p, 'session_token');
    bool(p, 'owner_is_worker_owner');
    num(p, 'ask_timeout_s');
  },
  'session.input': (p) => {
    str(p, 'session_id');
    str(p, 'event_id');
    str(p, 'text');
    strArr(p, 'artifact_ids');
    arr(p, 'peer_prefix').forEach((v, i) => {
      const x = obj(v, `peer_prefix[${i}]`);
      str(x, 'from');
      str(x, 'text');
      str(x, 'ts');
    });
  },
  'session.stop': (p) => void str(p, 'session_id'),
  'session.kill': (p) => void str(p, 'session_id'),
  'session.steer': (p) => {
    str(p, 'session_id');
    str(p, 'text');
  },
  'approval.response': (p) => {
    str(p, 'session_id');
    str(p, 'request_id');
    str(p, 'option_id');
  },
  'clarify.response': (p) => {
    str(p, 'session_id');
    str(p, 'request_id');
    parseAnswers(p);
  },
  'session.state': (p) => {
    str(p, 'session_id');
    strictEnum(p, 'process_state', PROCESS_STATES);
    optStr(p, 'runtime_session_id');
    strictEnum(p, 'locked_by', LOCKED_BY);
  },
};

/** Parses a worker <-> hub frame strictly (SPEC §5.2). */
export function parseFrame(json: unknown): Frame {
  const f = obj(json, 'frame');
  const id = str(f, 'id');
  const type = str(f, 'type');
  const parse = FRAME_PARSERS[type];
  if (!parse) throw new ProtoError(`frame: unknown type ${type}`);
  const payload = obj(f.payload, `${type} payload`);
  parse(payload);
  return { id, type, payload };
}

// ---- client <-> hub WebSocket /api/ws (SPEC §6) ----------------------------------------
// Flat JSON objects `{type, ...}` without `id` or `payload`, typed in
// crates/hqc-proto/src/client_frame.rs; contract examples: examples/wire/client.*.json.

export type ClientFrame =
  | { type: 'subscribe'; session_id: Uuid; after_seq?: number }
  | { type: 'unsubscribe'; session_id: Uuid }
  | { type: 'subscribe_summary' }
  | { type: 'ping' };

/** Session line of `summary` / `session.summary` (SPEC §6, §17.1). */
export interface SessionSummary {
  session_id: Uuid;
  project_id: Uuid;
  title: string;
  state: SessionState;
  queue_len: number;
  unread: number;
  last_seq: number;
  last_read_seq: number;
}

/** A change of `delivery` of a stored input (SPEC §6); the event is not sent again. */
export interface DeliveryChange {
  session_id: Uuid;
  event_id: Uuid;
  seq: number;
  delivery: Delivery;
  delivered_at?: string;
}

export type ServerFrame =
  | { type: 'event'; event: HqcEvent }
  | { type: 'event.invalid'; error: string; raw: unknown }
  | { type: 'subscribed'; session_id: Uuid; last_seq: number }
  | ({ type: 'delivery' } & DeliveryChange)
  | { type: 'summary'; sessions: SessionSummary[] }
  | { type: 'session.summary'; session: SessionSummary }
  | { type: 'pong' }
  | { type: 'error'; code: string; message: string; session_id?: Uuid }
  | { type: 'other'; wire_type: string };

export function parseSummary(v: unknown): SessionSummary {
  const s = obj(v, 'summary');
  return {
    session_id: str(s, 'session_id'),
    project_id: str(s, 'project_id'),
    title: str(s, 'title'),
    state: tolerantEnum(s, 'state', SESSION_STATES),
    queue_len: num(s, 'queue_len'),
    unread: num(s, 'unread'),
    last_seq: num(s, 'last_seq'),
    last_read_seq: num(s, 'last_read_seq'),
  };
}

export function parseDelivery(v: unknown): DeliveryChange {
  const d = obj(v, 'delivery');
  return {
    session_id: str(d, 'session_id'),
    event_id: str(d, 'event_id'),
    seq: num(d, 'seq'),
    delivery: strictEnum(d, 'delivery', DELIVERIES),
    delivered_at: optStr(d, 'delivered_at'),
  };
}

/** Parses a hub -> client WebSocket message. Unknown `type` comes back as `other`; an
 *  event that fails validation comes back as `event.invalid` so the chat can show it.
 *  Throws `ProtoError` on a known frame with a wrong shape. */
export function parseServerFrame(json: unknown): ServerFrame {
  const f = obj(json, 'frame');
  const type = str(f, 'type');
  switch (type) {
    case 'event': {
      const raw = f.event;
      try {
        return { type, event: parseEvent(raw) };
      } catch (e) {
        return { type: 'event.invalid', error: String((e as Error).message), raw };
      }
    }
    case 'subscribed':
      return { type, session_id: str(f, 'session_id'), last_seq: num(f, 'last_seq') };
    case 'delivery':
      return { type, ...parseDelivery(f) };
    case 'summary':
      return { type, sessions: arr(f, 'sessions').map(parseSummary) };
    case 'session.summary':
      return { type, session: parseSummary(f.session) };
    case 'pong':
      return { type };
    case 'error':
      return {
        type,
        code: str(f, 'code'),
        message: str(f, 'message'),
        session_id: optStr(f, 'session_id'),
      };
    default:
      return { type: 'other', wire_type: type };
  }
}

// ---- REST shapes (SPEC §3, §6) --------------------------------------------------------
// Typed in crates/hqc-proto/src/api.rs; contract examples: examples/wire/api.*.json.

export interface Me {
  id: Uuid;
  email: string;
  display_name?: string;
  personal_project_id: Uuid;
}

/** The fields of the hub's session object (SPEC §6) the client uses. */
export interface SessionInfo {
  id: Uuid;
  project_id: Uuid;
  title: string;
  slug: string;
  state: SessionState;
  queue_len: number;
  unread: number;
  last_seq: number;
  last_read_seq: number;
  archived: boolean;
  updated_at: string;
}

export function parseMe(v: unknown): Me {
  const o = obj(v, 'me');
  return {
    id: str(o, 'id'),
    email: str(o, 'email'),
    display_name: optStr(o, 'display_name'),
    personal_project_id: str(o, 'personal_project_id'),
  };
}

export function parseSessionInfo(v: unknown): SessionInfo {
  const o = obj(v, 'session');
  return {
    id: str(o, 'id'),
    project_id: str(o, 'project_id'),
    title: str(o, 'title'),
    slug: str(o, 'slug'),
    state: tolerantEnum(o, 'state', SESSION_STATES),
    queue_len: num(o, 'queue_len'),
    unread: num(o, 'unread'),
    last_seq: num(o, 'last_seq'),
    last_read_seq: num(o, 'last_read_seq'),
    archived: bool(o, 'archived'),
    updated_at: str(o, 'updated_at'),
  };
}

/** `{sessions: [...]}` of `GET /api/projects/{id}/sessions`. */
export function parseSessionList(v: unknown): SessionInfo[] {
  return arr(obj(v, 'session list'), 'sessions').map(parseSessionInfo);
}

/** `{events: [...], last_seq}` of `GET /api/sessions/{id}/events`; events stay raw so that
 *  one bad event does not hide the page. */
export function parseEventPage(v: unknown): { events: unknown[]; last_seq: number } {
  const o = obj(v, 'event page');
  return { events: arr(o, 'events'), last_seq: num(o, 'last_seq') };
}

/** `{event}` of `POST /api/sessions/{id}/input`: the stored `user.message`. */
export function parseInputResponse(v: unknown): HqcEvent {
  return parseEvent(obj(v, 'input response').event);
}

/** `{error, message}` of a failed REST call. */
export function parseApiError(v: unknown): { error: string; message: string } {
  const o = obj(v, 'error');
  return { error: str(o, 'error'), message: str(o, 'message') };
}

/** `{public_key}` of `GET /api/push/vapid`; `null` when the hub has no push. */
/** `GET /api/workers/enroll/{code}` (SPEC §5.1, §6): who asks to be enrolled. */
export interface EnrollInfo {
  name: string;
  os: string;
  created_at: string;
  /** Cloudflare's CF-IPCountry of the enrolling request. */
  country?: string;
}

export function parseEnrollInfo(v: unknown): EnrollInfo {
  const o = obj(v, 'enroll info');
  return {
    name: str(o, 'name'),
    os: str(o, 'os'),
    created_at: str(o, 'created_at'),
    country: optStr(o, 'country'),
  };
}

export function parseVapidKey(v: unknown): string | null {
  const o = obj(v, 'vapid');
  return o.public_key === null ? null : str(o, 'public_key');
}

/** Web Push body (SPEC §16), read by public/sw.js; `payload` is the event of SPEC §4. */
export interface PushMessage {
  session_id: Uuid;
  type: string;
  title?: string;
  body?: string;
  tag?: string;
  payload?: HqcEvent;
}

export function parsePushMessage(v: unknown): PushMessage {
  const o = obj(v, 'push');
  return {
    session_id: str(o, 'session_id'),
    type: str(o, 'type'),
    title: optStr(o, 'title'),
    body: optStr(o, 'body'),
    tag: optStr(o, 'tag'),
    payload: present(o, 'payload') ? parseEvent(o.payload) : undefined,
  };
}
