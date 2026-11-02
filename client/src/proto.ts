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

/** A session's permission mode (T38): ask the worker owner, or Claude Code decides. */
export const PERMISSION_MODES = ['ask', 'auto'] as const;
export type PermissionMode = (typeof PERMISSION_MODES)[number];
/** The app's base palette (T49). */
export const THEME_NAMES = ['warm', 'graphite', 'contrast'] as const;
export type Theme = (typeof THEME_NAMES)[number];
/** A project's color (T49); `theme.ts` maps it to its hex value. */
export const PROJECT_COLOR_NAMES = ['teal', 'plum', 'blue', 'rose', 'moss', 'coral', 'cyan', 'slate'] as const;
export type ProjectColor = (typeof PROJECT_COLOR_NAMES)[number];
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
  /** The highest mode a turn this message starts may run in (T39); absent: no cap. */
  permission_cap?: PermissionMode;
  /** With an `ask` cap: the address of the session up the chain whose Ask set it. */
  capped_by?: string;
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
  /** On the final `agent.message` that answered an `hqc_ask`: when the asking worker
   *  acknowledged the reply (SPEC §6, T51); absent until then. */
  reply_delivered_at?: string;
}

// ---- payloads (SPEC §4) ---------------------------------------------------------------

export interface UserMessage {
  text: string;
  artifact_ids: Uuid[];
  /** The hub cron sent it for this schedule, on the owner's behalf (SPEC §16). */
  schedule_id?: Uuid;
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
  /** hqc rules the worker suggests for "Allow in this folder" (T48), one per part. */
  rule_suggestions?: RuleSuggestion[];
}
/** A simple part of a command and the hqc rule suggested for it (none: no pattern). */
export interface RuleSuggestion {
  part: string;
  rule?: string;
}
export interface ApprovalResponse {
  request_id: string;
  option_id: string;
  by: string;
  /** The user's text instead of the call, with a deny option (SPEC §10.1). */
  message?: string;
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
  /** The sender's address `<project-slug>/<session-slug>` (since SPEC 0.34). */
  from?: string;
  text: string;
  mode: PeerMode;
  request_id?: string;
  /** The files the sender handed over (since SPEC 0.46, T41); empty when it had none. */
  artifact_ids: Uuid[];
}
export interface Usage {
  input_tokens: number;
  output_tokens: number;
  cache_read?: number;
  cache_write?: number;
  /** This turn's cost. */
  cost_usd?: number;
  /** The runtime's running total of the whole session after this turn (T27). */
  session_cost_usd?: number;
  model?: string;
}
export interface Status {
  state: SessionState;
  detail?: string;
}
export interface SystemNote {
  text: string;
  code: string;
  /** `warn` | `error` for `code: worker_log`, a log line of the worker (SPEC §10.2). */
  level?: string;
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

function optStrArr(o: Obj, k: string): string[] | undefined {
  return present(o, k) ? strArr(o, k) : undefined;
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
    schedule_id: optStr(p, 'schedule_id'),
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
    rule_suggestions: present(p, 'rule_suggestions') ? parseSuggestions(p, 'rule_suggestions') : undefined,
  }),
  'approval.response': (p) => ({
    request_id: str(p, 'request_id'),
    option_id: str(p, 'option_id'),
    by: str(p, 'by'),
    message: optStr(p, 'message'),
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
    from: optStr(p, 'from'),
    text: str(p, 'text'),
    mode: tolerantEnum(p, 'mode', PEER_MODES),
    request_id: optStr(p, 'request_id'),
    artifact_ids: present(p, 'artifact_ids') ? strArr(p, 'artifact_ids') : [],
  }),
  usage: (p) => ({
    input_tokens: num(p, 'input_tokens'),
    output_tokens: num(p, 'output_tokens'),
    cache_read: optNum(p, 'cache_read'),
    cache_write: optNum(p, 'cache_write'),
    cost_usd: optNum(p, 'cost_usd'),
    session_cost_usd: optNum(p, 'session_cost_usd'),
    model: optStr(p, 'model'),
  }),
  status: (p) => ({ state: tolerantEnum(p, 'state', SESSION_STATES), detail: optStr(p, 'detail') }),
  system: (p) => ({ text: str(p, 'text'), code: str(p, 'code'), level: optStr(p, 'level') }),
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
  return {
    chain_id: str(c, 'chain_id'),
    depth: num(c, 'depth'),
    hops: num(c, 'hops'),
    permission_cap: optPermissionMode(c, 'permission_cap'),
    capped_by: optStr(c, 'capped_by'),
  };
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
    reply_delivered_at: optStr(e, 'reply_delivered_at'),
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
    // Since SPEC 0.56 (T43) objects; a plain name from an older worker still reads.
    for (const e of arr(p, 'environments')) {
      if (typeof e === 'string') continue;
      const o = obj(e, 'environment');
      str(o, 'name');
      optBool(o, 'allow_local_mcp');
      if (present(o, 'permissions')) {
        const r = obj(o.permissions, 'permissions');
        for (const k of ['unblock', 'allow', 'deny']) optStrArr(r, k);
      }
    }
    for (const b of arr(p, 'env_bindings')) {
      const o = obj(b, 'env_binding');
      str(o, 'path');
      str(o, 'environment');
    }
    strArr(p, 'allowed_roots');
    bool(p, 'allow_foreign_sessions');
    bool(p, 'share_local_models');
    arr(p, 'local_models');
  },
  'hello.ack': (p) => {
    num(p, 'protocol_version');
    str(p, 'worker_id');
    str(p, 'server_time');
    optStr(p, 'system_session_id');
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
  result: (p) => void str(p, 'request_id'),
  'fs.stat': (p) => {
    str(p, 'request_id');
    str(p, 'path');
  },
  'fs.list_dirs': (p) => {
    str(p, 'request_id');
    // `null`: the worker's allowed_roots.
    optStr(p, 'path');
  },
  'workspace.list_candidates': (p) => void str(p, 'request_id'),
  'mirror.set': (p) => {
    str(p, 'workspace_path');
    bool(p, 'enabled');
  },
  // T21: the inbox folders (or the scratch folder) of an archived or deleted session.
  'session.cleanup': (p) => {
    str(p, 'session_id');
    optStr(p, 'workspace_path');
    str(p, 'cwd');
  },
  // T24: the worker titles a session on its Claude Code subscription.
  'title.request': (p) => {
    str(p, 'request_id');
    str(p, 'session_id');
    str(p, 'user_text');
    str(p, 'agent_text');
  },
  // T48: hqc's allow rules of a folder, and the rules screen's question to the worker.
  'workspace.rules': (p) => {
    str(p, 'workspace_path');
    strArr(p, 'allow_rules');
  },
  'permissions.inspect': (p) => {
    str(p, 'request_id');
    str(p, 'path');
    optStr(p, 'command');
    optStr(p, 'tool');
  },
  // T45: a clone into a new folder and its outcome; the check and the deletion of a folder hqc
  // created.
  'workspace.create': (p) => {
    str(p, 'request_id');
    str(p, 'kind');
    str(p, 'path');
    str(p, 'url');
    optStr(p, 'branch');
  },
  'workspace.created': (p) => {
    str(p, 'request_id');
    if ((p.canonical === undefined) === (p.error === undefined)) {
      throw new ProtoError('workspace.created: canonical or error');
    }
    optStr(p, 'canonical');
    optStr(p, 'error');
  },
  'workspace.inspect': (p) => {
    str(p, 'request_id');
    str(p, 'path');
  },
  'workspace.delete': (p) => {
    str(p, 'request_id');
    str(p, 'path');
  },
  // T30: an agent's hqc MCP tool call and its answer (SPEC §5.2).
  'mcp.call': (p) => {
    str(p, 'request_id');
    str(p, 'session_token');
    str(p, 'tool');
    obj(p.args, 'args');
  },
  'mcp.cancel': (p) => {
    str(p, 'request_id');
    str(p, 'reason');
  },
  'mcp.result': (p) => {
    str(p, 'request_id');
    if (p.error !== undefined) {
      const e = obj(p.error, 'error');
      str(e, 'code');
      str(e, 'message');
    } else if (p.result === undefined) {
      throw new ProtoError('mcp.result: result or error');
    }
  },
  'mirror.event': (p) => {
    str(p, 'workspace_path');
    str(p, 'runtime_session_id');
    str(p, 'cwd');
    parseEvent(p.event);
    optStr(p, 'title');
    bool(p, 'recent');
  },
  'session.start': (p) => {
    str(p, 'session_id');
    strictEnum(p, 'mode', START_MODES);
    str(p, 'runtime');
    optStr(p, 'acp_agent');
    optStr(p, 'model');
    optStr(p, 'workspace_path');
    str(p, 'cwd');
    optStr(p, 'runtime_session_id');
    optStr(p, 'system_prompt');
    strArr(p, 'disallowed_tools');
    bool(p, 'restricted');
    bool(p, 'auto_approve');
    str(p, 'session_token');
    bool(p, 'owner_is_worker_owner');
    num(p, 'ask_timeout_s');
    optBool(p, 'requested_by_owner');
    optPermissionMode(p, 'permission_mode');
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
    if (present(p, 'peer')) {
      const x = obj(p.peer, 'peer');
      str(x, 'from');
      tolerantEnum(x, 'mode', PEER_MODES);
      optStr(x, 'request_id');
    }
    optPermissionMode(p, 'permission_cap');
  },
  'session.stop': (p) => void str(p, 'session_id'),
  'session.kill': (p) => void str(p, 'session_id'),
  'session.permission_mode': (p) => {
    str(p, 'session_id');
    strictEnum(p, 'permission_mode', PERMISSION_MODES);
  },
  'session.steer': (p) => {
    str(p, 'session_id');
    str(p, 'text');
  },
  'approval.response': (p) => {
    str(p, 'session_id');
    str(p, 'request_id');
    str(p, 'option_id');
    optStr(p, 'message');
  },
  'clarify.response': (p) => {
    str(p, 'session_id');
    str(p, 'request_id');
    parseAnswers(p);
  },
  'worker.approval.request': (p) => {
    str(p, 'request_id');
    str(p, 'kind');
    str(p, 'description');
    str(p, 'expires_at');
  },
  'worker.approval.response': (p) => {
    str(p, 'request_id');
    str(p, 'option_id');
  },
  'session.state': (p) => {
    str(p, 'session_id');
    strictEnum(p, 'process_state', PROCESS_STATES);
    optStr(p, 'runtime_session_id');
    strictEnum(p, 'locked_by', LOCKED_BY);
    optStr(p, 'title');
    optStrArr(p, 'commands');
    optPermissionMode(p, 'permission_mode');
    optStr(p, 'capped_input');
  },
};

/** An optional permission mode; an unknown one reads as absent. */
function optPermissionMode(o: Obj, k: string): PermissionMode | undefined {
  if (!present(o, k)) return undefined;
  const v = tolerantEnum(o, k, PERMISSION_MODES);
  return v === 'unknown' ? undefined : v;
}

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

/** A live Claude Code process outside hqc holding the session's transcript open
 *  (SPEC §9.3): `entrypoint` is `claude-vscode` for the VS Code extension, `cli`, `sdk-cli`
 *  and others; `status` is `idle` | `busy`. Both lists are open. */
export interface HeldBy {
  entrypoint: string;
  status: string;
}

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
  held_by?: HeldBy;
  permission_mode?: PermissionMode;
  effective_permission_mode?: PermissionMode;
  permission_capped_by?: string;
}

/** A session waiting on another through an open `hqc_ask`, directly or along a chain. */
export interface WaitingSession {
  session_id: Uuid;
  address: string;
  title: string;
}

/** An open request of the user (SPEC §6 `GET /api/requests`, the `requests` frame, T37). */
export interface OpenRequest {
  session_id: Uuid;
  request_id: string;
  /** `approval` | `clarify` | `worker_approval`; open. */
  kind: string;
  address: string;
  title: string;
  worker_id?: Uuid;
  worker_name?: string;
  /** In a worker's service session (SPEC §10.2). */
  system: boolean;
  event: EventOf<'approval.request' | 'clarify.request'>;
  /** Who waits on it, the nearest first; the last is the top of the chain. */
  waiting: WaitingSession[];
}

/** `{requests[]}`; an item whose event is not a request event this build reads is left out. */
export function parseRequestList(v: unknown): OpenRequest[] {
  const out: OpenRequest[] = [];
  for (const x of arr(obj(v, 'requests'), 'requests')) {
    const o = obj(x, 'request');
    let event: HqcEvent;
    try {
      event = parseEvent(o.event);
    } catch {
      continue;
    }
    if (event.type !== 'approval.request' && event.type !== 'clarify.request') continue;
    out.push({
      session_id: str(o, 'session_id'),
      request_id: str(o, 'request_id'),
      kind: str(o, 'kind'),
      address: str(o, 'address'),
      title: str(o, 'title'),
      worker_id: optStr(o, 'worker_id'),
      worker_name: optStr(o, 'worker_name'),
      system: optBool(o, 'system') ?? false,
      event,
      waiting: (present(o, 'waiting') ? arr(o, 'waiting') : []).map((w) => {
        const ws = obj(w, 'waiting');
        return { session_id: str(ws, 'session_id'), address: str(ws, 'address'), title: str(ws, 'title') };
      }),
    });
  }
  return out;
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
  | { type: 'requests'; requests: OpenRequest[] }
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
    held_by: optHeldBy(s),
    permission_mode: optPermissionMode(s, 'permission_mode'),
    effective_permission_mode: optPermissionMode(s, 'effective_permission_mode'),
    permission_capped_by: optStr(s, 'permission_capped_by'),
  };
}

/** `held_by` of a session or its summary; absent or `null` is nobody. */
export function optHeldBy(o: Obj): HeldBy | undefined {
  if (!present(o, 'held_by')) return undefined;
  const h = obj(o.held_by, 'held_by');
  return { entrypoint: str(h, 'entrypoint'), status: str(h, 'status') };
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
    case 'requests':
      return { type, requests: parseRequestList(f) };
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
  /** IANA name; the default zone of the user's schedules (SPEC §16). */
  timezone?: string;
  /** The app's base palette (T49), the same on every device. */
  theme: Theme;
}

/** The fields of the hub's session object (SPEC §6) the client uses. */
export interface SessionInfo {
  id: Uuid;
  project_id: Uuid;
  workspace_id?: Uuid;
  worker_id?: Uuid;
  /** Relative to the workspace, `.` by default. */
  cwd: string;
  title: string;
  slug: string;
  state: SessionState;
  queue_len: number;
  unread: number;
  last_seq: number;
  last_read_seq: number;
  archived: boolean;
  /** When it was archived (T50); absent for a live session and one archived before the
   *  hub kept the time. */
  archived_at?: string;
  updated_at: string;
  /** The service session of worker `worker_id` (SPEC §10.2): no input, listed under
   *  "Workers", titled with the worker's name. */
  system: boolean;
  /** Open in VS Code (or another Claude Code outside hqc) right now (SPEC §9.3). */
  held_by?: HeldBy;
  /** The runtime's slash commands without `/`, for completing `/` in the composer (T27);
   *  absent until a process of the session reported them. */
  commands?: string[];
  /** The session whose agent started this one with `hqc_spawn` (SPEC §7), while it exists. */
  spawned_by?: SpawnedBy;
  /** The mode hqc asks the worker for (T38); absent: the worker's own default. */
  permission_mode?: PermissionMode;
  /** The mode the agent runs in, as the worker last reported it; absent until reported. */
  effective_permission_mode?: PermissionMode;
  /** While the current turn runs in Ask under a peer message's cap (T39): the address of
   *  the session up the chain whose Ask set it. */
  permission_capped_by?: string;
}

/** `spawned_by` of a session: the spawning session and its address. */
export interface SpawnedBy {
  session_id: Uuid;
  address: string;
}

export function parseMe(v: unknown): Me {
  const o = obj(v, 'me');
  return {
    id: str(o, 'id'),
    email: str(o, 'email'),
    display_name: optStr(o, 'display_name'),
    personal_project_id: str(o, 'personal_project_id'),
    timezone: optStr(o, 'timezone'),
    theme: oneOf(o, 'theme', THEME_NAMES) ?? 'warm',
  };
}

/** A value of `values`; absent or one this build does not know: undefined. */
function oneOf<T extends string>(o: Obj, k: string, values: readonly T[]): T | undefined {
  const v = optStr(o, k);
  return (values as readonly string[]).includes(v ?? '') ? (v as T) : undefined;
}

/** A scheduled message (SPEC §3 ScheduledMessage, §16): exactly one of `cron` and `at`. */
export interface Schedule {
  id: Uuid;
  session_id: Uuid;
  cron?: string;
  at?: string;
  timezone: string;
  message: string;
  enabled: boolean;
  catch_up: boolean;
  /** Absent when it will not run again (an `at` that ran). */
  next_run?: string;
  last_run?: string;
}

export function parseSchedule(v: unknown): Schedule {
  const o = obj(v, 'schedule');
  return {
    id: str(o, 'id'),
    session_id: str(o, 'session_id'),
    cron: optStr(o, 'cron'),
    at: optStr(o, 'at'),
    timezone: str(o, 'timezone'),
    message: str(o, 'message'),
    enabled: bool(o, 'enabled'),
    catch_up: bool(o, 'catch_up'),
    next_run: optStr(o, 'next_run'),
    last_run: optStr(o, 'last_run'),
  };
}

export function parseScheduleList(v: unknown): Schedule[] {
  return arr(obj(v, 'schedule list'), 'schedules').map(parseSchedule);
}

export function parseSessionInfo(v: unknown): SessionInfo {
  const o = obj(v, 'session');
  return {
    id: str(o, 'id'),
    project_id: str(o, 'project_id'),
    workspace_id: optStr(o, 'workspace_id'),
    worker_id: optStr(o, 'worker_id'),
    cwd: optStr(o, 'cwd') ?? '.',
    title: str(o, 'title'),
    slug: str(o, 'slug'),
    state: tolerantEnum(o, 'state', SESSION_STATES),
    queue_len: num(o, 'queue_len'),
    unread: num(o, 'unread'),
    last_seq: num(o, 'last_seq'),
    last_read_seq: num(o, 'last_read_seq'),
    archived: bool(o, 'archived'),
    archived_at: optStr(o, 'archived_at'),
    updated_at: str(o, 'updated_at'),
    system: optBool(o, 'system') ?? false,
    held_by: optHeldBy(o),
    commands: optStrArr(o, 'commands'),
    spawned_by: optSpawnedBy(o),
    permission_mode: optPermissionMode(o, 'permission_mode'),
    effective_permission_mode: optPermissionMode(o, 'effective_permission_mode'),
    permission_capped_by: optStr(o, 'permission_capped_by'),
  };
}

function optSpawnedBy(o: Obj): SpawnedBy | undefined {
  if (!present(o, 'spawned_by')) return undefined;
  const p = obj(o.spawned_by, 'spawned_by');
  return { session_id: str(p, 'session_id'), address: str(p, 'address') };
}

/** `{sessions: [...]}` of `GET /api/projects/{id}/sessions`. */
export function parseSessionList(v: unknown): SessionInfo[] {
  return arr(obj(v, 'session list'), 'sessions').map(parseSessionInfo);
}

export const LIST_MODE_NAMES = ['state', 'recent', 'custom'] as const;
/** How a project's session list is ordered (T50): by state, by recency, or the user's own. */
export type ListMode = (typeof LIST_MODE_NAMES)[number];

/** `GET/PUT /api/projects/{id}/session_order` (T50): the user's order of a project's list,
 *  every unarchived session in it, the ones not placed yet first. */
export interface SessionOrder {
  mode: ListMode;
  session_ids: Uuid[];
}

export function parseSessionOrder(v: unknown): SessionOrder {
  const o = obj(v, 'session order');
  return {
    mode: oneOf(o, 'mode', LIST_MODE_NAMES) ?? 'state',
    session_ids: arr(o, 'session_ids').map((x, i) => {
      if (typeof x !== 'string') throw new ProtoError(`session order: session_ids[${i}] is not a string`);
      return x;
    }),
  };
}

/** A project (SPEC §3 Project, §6). */
export interface Project {
  id: Uuid;
  name: string;
  slug: string;
  description: string;
  default_workspace_id?: Uuid;
  /** The user's implicit project: neither renamed nor deleted. */
  is_personal: boolean;
  /** Stored, or picked by the hub by a hash of the id (T49). */
  color: ProjectColor;
  /** Absent: the first letters of the name. */
  icon?: ProjectIcon;
  created_at: string;
}

/** A project's icon (T49); `url` of an image changes with the image. */
export type ProjectIcon = { kind: 'emoji' | 'letters'; value: string } | { kind: 'image'; url: string };

function optProjectIcon(o: Obj): ProjectIcon | undefined {
  if (!present(o, 'icon')) return undefined;
  const i = obj(o.icon, 'icon');
  const kind = optStr(i, 'kind');
  if (kind === 'emoji' || kind === 'letters') return { kind, value: str(i, 'value') };
  if (kind === 'image') return { kind, url: str(i, 'url') };
  // A kind this build does not know: the letters.
  return undefined;
}

export function parseProject(v: unknown): Project {
  const o = obj(v, 'project');
  return {
    id: str(o, 'id'),
    name: str(o, 'name'),
    slug: str(o, 'slug'),
    description: optStr(o, 'description') ?? '',
    default_workspace_id: optStr(o, 'default_workspace_id'),
    is_personal: bool(o, 'is_personal'),
    color: oneOf(o, 'color', PROJECT_COLOR_NAMES) ?? 'teal',
    icon: optProjectIcon(o),
    created_at: str(o, 'created_at'),
  };
}

/** `{projects: [...]}` of `GET /api/projects`. */
export function parseProjectList(v: unknown): Project[] {
  return arr(obj(v, 'project list'), 'projects').map(parseProject);
}

/** A workspace (SPEC §3 Workspace): a folder on a worker, in one project. */
export interface Workspace {
  id: Uuid;
  project_id: Uuid;
  worker_id: Uuid;
  /** Canonical, as the worker reported it. */
  path: string;
  label: string;
  /** The environment the worker's bindings give the folder (T43; a display, the worker decides). */
  environment: string;
  /** `dir` | `clone` (T45) | `worktree` (later). */
  kind: string;
  /** hqc made the folder (T45): deleting the workspace can delete it. */
  created_by_hqc: boolean;
  /** Its Claude Code sessions are mirrored (imported from VS Code, SPEC §9). */
  mirror_enabled: boolean;
  /** How many hqc allow rules the folder has (T48). */
  hqc_rules: number;
  /** `creating` while the worker clones, `failed` with `error` when it could not (T45). */
  state: WorkspaceState | Unknown;
  error?: string;
  created_at: string;
}

export const WORKSPACE_STATES = ['creating', 'ready', 'failed'] as const;
export type WorkspaceState = (typeof WORKSPACE_STATES)[number];

export function parseWorkspace(v: unknown): Workspace {
  const o = obj(v, 'workspace');
  return {
    id: str(o, 'id'),
    project_id: str(o, 'project_id'),
    worker_id: str(o, 'worker_id'),
    path: str(o, 'path'),
    label: optStr(o, 'label') ?? '',
    environment: optStr(o, 'environment') ?? 'default',
    kind: str(o, 'kind'),
    created_by_hqc: o.created_by_hqc === true,
    mirror_enabled: o.mirror_enabled === true,
    hqc_rules: optNum(o, 'hqc_rules') ?? 0,
    // A hub before T45 sends none: every folder it knows exists.
    state: present(o, 'state') ? tolerantEnum(o, 'state', WORKSPACE_STATES) : 'ready',
    error: optStr(o, 'error'),
    created_at: str(o, 'created_at'),
  };
}

/** `GET /api/workspaces/{id}/inspect` (T45): what deleting a folder hqc created would lose. */
export interface WorkspaceInspection {
  /** Not a git folder: nothing to warn about but the folder itself. */
  git: boolean;
  /** Paths with uncommitted changes (at most 200 of `dirty_count`). */
  dirty: string[];
  dirty_count: number;
  /** Local branches with commits on no remote branch. */
  unpushed: { branch: string; commits: number }[];
}

export function parseWorkspaceInspection(v: unknown): WorkspaceInspection {
  const o = obj(v, 'inspection');
  const dirty = arr(o, 'dirty').filter((s): s is string => typeof s === 'string');
  return {
    git: o.git === true,
    dirty,
    dirty_count: optNum(o, 'dirty_count') ?? dirty.length,
    unpushed: arr(o, 'unpushed').map((e, i) => {
      const u = obj(e, `unpushed[${i}]`);
      return { branch: str(u, 'branch'), commits: num(u, 'commits') };
    }),
  };
}

/** One of hqc's allow rules of a workspace (T48). */
export interface WorkspaceRule {
  id: Uuid;
  rule: string;
  created_at: string;
}

/** `{rules: [...]}` of `GET`/`POST /api/workspaces/{id}/rules`, newest first. */
export function parseWorkspaceRuleList(v: unknown): WorkspaceRule[] {
  return arr(obj(v, 'rule list'), 'rules').map((e, i) => {
    const o = obj(e, `rules[${i}]`);
    return { id: str(o, 'id'), rule: str(o, 'rule'), created_at: str(o, 'created_at') };
  });
}

function parseSuggestions(o: Obj, key: string): RuleSuggestion[] {
  return arr(o, key).map((e, i) => {
    const s = obj(e, `${key}[${i}]`);
    return { part: str(s, 'part'), rule: optStr(s, 'rule') };
  });
}

/** The rules of one file the worker read (T48): `worker.toml`, `~/.claude/settings.json`,
 *  `.claude/settings.local.json`. */
export interface RuleSource {
  source: string;
  allow: string[];
  deny: string[];
  ask: string[];
  error?: string;
}

export const VERDICTS = ['deny', 'allow', 'ask'] as const;
export type Verdict = (typeof VERDICTS)[number] | Unknown;

/** How the worker's matcher decides a command or tool in the folder. */
export interface CheckResult {
  verdict: Verdict;
  rule?: string;
  source?: string;
  parts: number;
  uncovered: number;
  suggestions: RuleSuggestion[];
}

/** `GET /api/workspaces/{id}/permissions` (the worker's `permissions.inspect`, T48). */
export interface WorkspacePermissions {
  mode: PermissionMode;
  allow_auto: boolean;
  sources: RuleSource[];
  hqc: string[];
  check?: CheckResult;
}

export function parseWorkspacePermissions(v: unknown): WorkspacePermissions {
  const o = obj(v, 'permissions');
  const strs = (x: Obj, k: string) =>
    present(x, k) ? arr(x, k).filter((s): s is string => typeof s === 'string') : [];
  const c = present(o, 'check') ? obj(o.check, 'check') : undefined;
  return {
    mode: o.mode === 'auto' ? 'auto' : 'ask',
    allow_auto: o.allow_auto !== false,
    sources: arr(o, 'sources').map((e, i) => {
      const s = obj(e, `sources[${i}]`);
      return { source: str(s, 'source'), allow: strs(s, 'allow'), deny: strs(s, 'deny'), ask: strs(s, 'ask'), error: optStr(s, 'error') };
    }),
    hqc: strs(o, 'hqc'),
    check: c && {
      verdict: tolerantEnum(c, 'verdict', VERDICTS),
      rule: optStr(c, 'rule'),
      source: optStr(c, 'source'),
      parts: num(c, 'parts'),
      uncovered: optNum(c, 'uncovered') ?? 0,
      suggestions: present(c, 'suggestions') ? parseSuggestions(c, 'suggestions') : [],
    },
  };
}

/** `{workspaces: [...]}` of `GET /api/workspaces?project_id=`. */
export function parseWorkspaceList(v: unknown): Workspace[] {
  return arr(obj(v, 'workspace list'), 'workspaces').map(parseWorkspace);
}

/** A folder of `GET /api/workers/{id}/dirs` (the worker's `fs.list_dirs`, SPEC §5.3). */
export interface DirEntry {
  name: string;
  path: string;
}

export function parseDirList(v: unknown): DirEntry[] {
  return arr(obj(v, 'dir list'), 'entries').map((e, i) => {
    const o = obj(e, `entries[${i}]`);
    return { name: str(o, 'name'), path: str(o, 'path') };
  });
}

/** A folder with Claude Code sessions (`GET /api/workers/{id}/candidates`, SPEC §9.1). */
export interface Candidate {
  path: string;
  sessions_count: number;
  last_active: string;
}

export function parseCandidateList(v: unknown): Candidate[] {
  return arr(obj(v, 'candidate list'), 'candidates').map((e, i) => {
    const o = obj(e, `candidates[${i}]`);
    return { path: str(o, 'path'), sessions_count: num(o, 'sessions_count'), last_active: str(o, 'last_active') };
  });
}

/** The extra fields of `409 workspace_conflict`: the workspace that holds the folder. */
export interface WorkspaceConflict {
  existing_workspace_id: Uuid;
  existing_project_id: Uuid;
  existing_path: string;
}

/** `null` unless `v` is a `workspace_conflict` body. */
export function parseWorkspaceConflict(v: unknown): WorkspaceConflict | null {
  if (typeof v !== 'object' || v === null || (v as Obj).error !== 'workspace_conflict') return null;
  const o = v as Obj;
  return {
    existing_workspace_id: str(o, 'existing_workspace_id'),
    existing_project_id: str(o, 'existing_project_id'),
    existing_path: str(o, 'existing_path'),
  };
}

/** `{events: [...], last_seq}` of `GET /api/sessions/{id}/events`; events stay raw so that
 *  one bad event does not hide the page. */
export function parseEventPage(v: unknown): { events: unknown[]; last_seq: number } {
  const o = obj(v, 'event page');
  return { events: arr(o, 'events'), last_seq: num(o, 'last_seq') };
}

/** One hit of `GET /api/search` (SPEC §15): an event's text, or the session's title (no
 *  `seq`). */
export interface SearchHit {
  session_id: Uuid;
  address: string;
  title: string;
  archived: boolean;
  seq?: number;
  type?: string;
  ts: string;
  snippet: string;
}

export interface SearchPage {
  results: SearchHit[];
  next_cursor?: string;
}

export function parseSearchPage(v: unknown): SearchPage {
  const o = obj(v, 'search page');
  const results = arr(o, 'results').map((r) => {
    const h = obj(r, 'search hit');
    return {
      session_id: str(h, 'session_id'),
      address: str(h, 'address'),
      title: str(h, 'title'),
      archived: bool(h, 'archived'),
      seq: optNum(h, 'seq'),
      type: optStr(h, 'type'),
      ts: str(h, 'ts'),
      snippet: str(h, 'snippet'),
    };
  });
  return { results, next_cursor: optStr(o, 'next_cursor') };
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
  /** The user's offline or revoked workers this one may replace (SPEC §5.1). */
  replaceable: ReplaceableWorker[];
}

export interface ReplaceableWorker {
  id: Uuid;
  name: string;
  os: string;
  /** `offline` | `revoked`. */
  status: string;
  last_seen?: string;
  /** Sessions on it that are neither archived nor its service session. */
  sessions_count: number;
  /** It last reported the same machine fingerprint as the enrolling worker (T21). */
  same_machine?: boolean;
}

export function parseEnrollInfo(v: unknown): EnrollInfo {
  const o = obj(v, 'enroll info');
  return {
    name: str(o, 'name'),
    os: str(o, 'os'),
    created_at: str(o, 'created_at'),
    country: optStr(o, 'country'),
    replaceable: o.replaceable == null
      ? []
      : arr(o, 'replaceable').map((x) => {
          const w = obj(x, 'replaceable worker');
          return {
            id: str(w, 'id'),
            name: str(w, 'name'),
            os: str(w, 'os'),
            status: str(w, 'status'),
            last_seen: optStr(w, 'last_seen'),
            sessions_count: num(w, 'sessions_count'),
            same_machine: w.same_machine === true,
          };
        }),
  };
}

/** Reply to `POST /api/workers/enroll/confirm`; `replaced` — the former name of the worker
 *  that was replaced (SPEC §5.1). */
export interface EnrollConfirmed {
  worker_id: Uuid;
  name: string;
  replaced?: string;
}

export function parseEnrollConfirmed(v: unknown): EnrollConfirmed {
  const o = obj(v, 'enroll confirmed');
  return { worker_id: str(o, 'worker_id'), name: str(o, 'name'), replaced: optStr(o, 'replaced') };
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
  /** The session a click opens instead of `session_id`: the top of a chain of asks (T37). */
  open_session_id?: Uuid;
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
    open_session_id: optStr(o, 'open_session_id'),
  };
}

/** An artifact of `POST /api/artifacts` and `GET /api/artifacts/{id}` (SPEC §3, §12). */
export interface Artifact {
  id: Uuid;
  kind: ArtifactKind;
  name: string;
  mime: string;
  size: number;
  sha256: string;
  created_at: string;
}

export function parseArtifact(v: unknown): Artifact {
  const o = obj(v, 'artifact');
  return {
    id: str(o, 'id'),
    kind: tolerantEnum(o, 'kind', ARTIFACT_KINDS),
    name: str(o, 'name'),
    mime: optStr(o, 'mime') ?? '',
    size: num(o, 'size'),
    sha256: optStr(o, 'sha256') ?? '',
    created_at: optStr(o, 'created_at') ?? '',
  };
}

/** `GET /api/artifacts/{id}/url`: a signed link (SPEC §12); `url` may be relative to the hub. */
export interface ArtifactUrl {
  url: string;
  expires_at: string;
}

export function parseArtifactUrl(v: unknown): ArtifactUrl {
  const o = obj(v, 'artifact url');
  return { url: str(o, 'url'), expires_at: str(o, 'expires_at') };
}
