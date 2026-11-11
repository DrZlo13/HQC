// The rules of a folder (T48, `Rules.dc.html`, `MRules2.dc.html`): what decides the agent's
// tool calls in one workspace, in the order it acts — denied, allowed, the rest — each rule
// with its source; hqc's own allow rules are added and removed here, the rules of
// `worker.toml` and Claude Code's settings files are read-only. The worker reads the files and
// checks commands with its own matcher (`GET /api/workspaces/{id}/permissions`); offline, only
// hqc's rules are shown.

import { useEffect, useRef, useState } from 'preact/hooks';
import { ApiError, type WorkerInfo } from '../api';
import type { AppState } from '../app';
import { onBack } from '../nav';
import type { CheckResult, PermissionMode, WorkspacePermissions, WorkspaceRule } from '../proto';
import { isToolName, ruleCommand, ruleError } from '../rules';
import { useSubscription } from './hooks';
import { IconBack, IconCross, IconInfo, IconPlus, IconShield } from './icons';
import { homePath } from './labels';

const HQC = 'HQC';
const EXAMPLES = ['git status', 'npm install && npm test', 'Edit'];
const FORMATS = ['Bash(command *)', 'Edit', 'mcp__server'];
const MODE_LABELS: Record<PermissionMode, string> = { ask: 'Ask', auto: 'Auto' };
/** How long the check waits for the typing to stop. */
export const CHECK_DELAY = 300;

interface Row {
  rule: string;
  source: string;
  /** An hqc rule: its id and date, removable. */
  hqc?: WorkspaceRule;
}

/** The worker's view of the folder, `offline` when it is not connected. */
type Inspect = { state: 'loading' } | { state: 'online'; perms: WorkspacePermissions } | { state: 'offline'; why: string };

function inspectError(e: unknown): Inspect {
  if (e instanceof ApiError && e.code === 'worker_offline') return { state: 'offline', why: 'offline' };
  return { state: 'offline', why: (e as Error).message };
}

function shortDate(iso: string): string {
  return new Date(iso).toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
}

/** A value that follows `v` once it has stayed the same for `ms`. */
function useDebounced<T>(v: T, ms: number): T {
  const [d, setD] = useState(v);
  useEffect(() => {
    const t = setTimeout(() => setD(v), ms);
    return () => clearTimeout(t);
  }, [v, ms]);
  return d;
}

export function RulesScreen({
  app,
  projectId,
  workspaceId,
  sessionId,
  phone,
}: {
  app: AppState;
  projectId: string;
  workspaceId: string;
  sessionId?: string;
  phone?: boolean;
}) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const ws = app.workspaces.get(projectId)?.find((w) => w.id === workspaceId);
  const session = sessionId ? app.sessions.get(sessionId) : undefined;
  const [workers, setWorkers] = useState<WorkerInfo[] | null>(null);
  const [rules, setRules] = useState<WorkspaceRule[] | null>(null);
  const [inspect, setInspect] = useState<Inspect>({ state: 'loading' });
  const [error, setError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [formOpen, setFormOpen] = useState(false);
  const [draft, setDraft] = useState('');
  const [q, setQ] = useState('');

  const loadRules = () =>
    app.api.workspaceRules(workspaceId).then(setRules, (e: Error) => {
      setRules([]);
      setError(e.message);
    });
  const loadInspect = () => app.api.workspacePermissions(workspaceId).then((perms) => setInspect({ state: 'online', perms }), (e) => setInspect(inspectError(e)));
  useEffect(() => {
    if (!app.workspaces.has(projectId)) app.loadWorkspaces(projectId).catch(() => {});
    app.api.workers().then(setWorkers, () => setWorkers([]));
    void loadRules();
    void loadInspect();
  }, [app, projectId, workspaceId]);

  // The check: the worker decides, with the rules as they are now.
  const query = useDebounced(q.trim(), CHECK_DELAY);
  const [check, setCheck] = useState<{ q: string; result: CheckResult | null } | null>(null);
  const rulesKey = rules?.map((r) => r.id).join(',');
  useEffect(() => {
    if (!query || inspect.state === 'offline') return setCheck(null);
    let live = true;
    app.api.workspacePermissions(workspaceId, isToolName(query) ? { tool: query } : { command: query }).then(
      (r) => live && setCheck({ q: query, result: r.check ?? null }),
      (e) => {
        if (!live) return;
        setCheck(null);
        setInspect(inspectError(e));
      },
    );
    return () => {
      live = false;
    };
  }, [app, workspaceId, query, rulesKey, inspect.state === 'offline']);

  const changed = () => app.loadWorkspaces(projectId).catch(() => {});
  const remove = async (r: WorkspaceRule) => {
    setError(null);
    try {
      await app.api.deleteWorkspaceRule(workspaceId, r.id);
      setConfirming(null);
      await loadRules();
      void changed();
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const add = async (rule: string) => {
    setRules(await app.api.addWorkspaceRules(workspaceId, [rule]));
    setFormOpen(false);
    setDraft('');
    void changed();
  };
  const openForm = (rule = '') => {
    setDraft(rule);
    setFormOpen(true);
  };

  const online = inspect.state === 'online' ? inspect.perms : null;
  const offline = inspect.state === 'offline';
  const worker = workers?.find((w) => w.id === ws?.worker_id);
  const files = online?.sources ?? [];
  const denied: Row[] = files.flatMap((s) => s.deny.map((rule) => ({ rule, source: s.source })));
  const hqcRows: Row[] = (rules ?? []).map((r) => ({ rule: r.rule, source: HQC, hqc: r }));
  const allowed: Row[] = [...hqcRows, ...files.flatMap((s) => s.allow.map((rule) => ({ rule, source: s.source })))];
  const asks: Row[] = files.flatMap((s) => s.ask.map((rule) => ({ rule, source: s.source })));
  const unread = files.filter((s) => s.error);
  // The mode the "Everything else" text and the check speak of: the session's, else the
  // worker's default.
  const sessionMode = session?.effective_permission_mode ?? session?.permission_mode;
  const mode: PermissionMode = sessionMode ?? online?.mode ?? 'ask';
  const back = sessionId ? `#/s/${sessionId}` : `#/p/${projectId}/settings`;
  const path = ws ? homePath(ws.path) : '';

  const rowView = (x: Row) => {
    const id = x.hqc?.id;
    return (
      <li key={`${x.source}:${x.rule}`} class={`rule-row ${x.hqc ? 'is-hqc' : ''}`}>
        <span class="rule-text mono">{x.rule}</span>
        <span class={`rule-source mono ${x.hqc ? 'is-hqc' : ''}`}>{x.source}</span>
        {x.hqc && <span class="rule-date">{shortDate(x.hqc.created_at)}</span>}
        {x.hqc && confirming !== id && (
          <button type="button" class="icon-btn rule-remove" aria-label={`Remove rule ${x.rule}`} title="Remove" onClick={() => setConfirming(id!)}>
            <IconCross size={13} />
          </button>
        )}
        {x.hqc && confirming === id && (
          <div class="rule-confirm">
            <span>
              Remove <span class="mono">{x.rule}</span>? Takes effect in running sessions right away.
            </span>
            <button type="button" class="btn btn-small btn-danger btn-confirm-remove" onClick={() => remove(x.hqc!)}>
              Remove
            </button>
            <button type="button" class="btn btn-small btn-ghost" onClick={() => setConfirming(null)}>
              Cancel
            </button>
          </div>
        )}
      </li>
    );
  };

  return (
    <section class={`page rules-page ${phone ? 'is-phone' : ''}`}>
      <header class="bar">
        <a
          class="back icon-btn"
          href={back}
          aria-label={sessionId ? 'Back to session' : 'Back to project settings'}
          title="Back"
          onClick={(e) => onBack(e, back)}
        >
          <IconBack />
        </a>
        <div class="bar-title">
          <h1 class="title">Folder rules</h1>
          <div class="rules-meta">
            <span class="rules-worker">
              <span class={`dot ${worker?.status === 'online' && !offline ? 'is-online' : ''}`} />
              {worker?.name ?? 'worker'}
            </span>
            <span class="sep">/</span>
            <span class="mono rules-path">{path}</span>
            {!phone && (
              <>
                <span class="sep">/</span>
                <span>
                  Environment <span class="rules-env">{ws?.environment ?? 'default'}</span>
                </span>
              </>
            )}
            {online && <span class="chip rules-mode">{MODE_LABELS[online.mode]} by default</span>}
            {sessionMode && <span class="chip rules-session-mode">This session: {MODE_LABELS[sessionMode]}</span>}
          </div>
        </div>
      </header>
      <div class="page-scroll">
        <div class="page-body rules-body">
          <p class="rules-intro">
            HQC rules apply to every session in this folder as soon as you save them, running ones included. Denies in{' '}
            <span class="mono">worker.toml</span> always win.
          </p>
          {error && <div class="error-text">{error}</div>}

          <section class="rules-check ps-card" aria-label="Check a command">
            <label for="rules-check" class="rules-check-label">
              Check a command
            </label>
            {/* A textarea: a pasted script keeps its line breaks, which separate commands. */}
            <textarea
              id="rules-check"
              class="input mono"
              rows={1}
              placeholder="Command or tool name, e.g. git push or Edit"
              value={q}
              disabled={offline}
              onInput={(e) => setQ((e.currentTarget as HTMLTextAreaElement).value)}
            />
            <div class="rules-chips">
              {EXAMPLES.map((t) => (
                <button key={t} type="button" class="chip-btn mono" disabled={offline} onClick={() => setQ(t)}>
                  {t}
                </button>
              ))}
            </div>
            <CheckView offline={offline} q={q.trim()} check={check} mode={mode} onAllow={openForm} />
          </section>

          <section class="rules-group is-denied" aria-label="Denied">
            <div class="rules-group-head">
              <span class="rules-group-name">Denied</span>
              <span class="rules-group-text">Won’t run. The agent doesn’t ask.</span>
            </div>
            <ul class="rule-list">{denied.map(rowView)}</ul>
            {online && denied.length === 0 && <div class="rules-none">none</div>}
            {offline && <div class="rules-none">Worker unavailable: rules from worker.toml and Claude Code files can’t be read.</div>}
          </section>

          <section class="rules-group is-allowed" aria-label="Allowed">
            <div class="rules-group-head">
              <span class="rules-group-name">Allowed</span>
              <span class="rules-group-text">Runs without asking.</span>
              <button type="button" class="btn btn-small btn-primary btn-add-rule" onClick={() => openForm()}>
                <IconPlus size={15} />
                Allow in this folder
              </button>
            </div>
            {formOpen && (
              <RuleForm
                app={app}
                workspaceId={workspaceId}
                path={path}
                phone={phone}
                draft={draft}
                taken={(rules ?? []).map((r) => r.rule)}
                checks={!offline}
                onDraft={setDraft}
                onSave={add}
                onCancel={() => {
                  setFormOpen(false);
                  setDraft('');
                }}
              />
            )}
            <ul class="rule-list">{allowed.map(rowView)}</ul>
            {rules && allowed.length === 0 && !offline && inspect.state !== 'loading' && <div class="rules-none">none</div>}
            {offline && (
              <div class="rules-none">Worker unavailable: rules from files aren’t shown. HQC rules still save and apply when it’s back.</div>
            )}
            {unread.map((s) => (
              <div key={s.source} class="rules-none error-text">
                {s.source} could not be read: {s.error}
              </div>
            ))}
            <div class="rules-files-note">
              <IconInfo />
              <span>
                Rules from files are read-only here. Edit them on the worker: <span class="mono">worker.toml</span> in the HQC
                worker config, <span class="mono">~/.claude/settings.json</span> for all your Claude Code projects,{' '}
                <span class="mono">.claude/settings.local.json</span> inside this folder, and in an environment with{' '}
                <span class="mono">project_settings</span> the repository's <span class="mono">.claude/settings.json</span>.
              </span>
            </div>
          </section>

          <section class="rules-group is-else" aria-label="Everything else">
            <div class="rules-group-head">
              <span class="rules-group-name">Everything else</span>
              <span class="rules-group-text">
                {mode === 'ask'
                  ? 'This folder runs in Ask: a permission card appears in the session. (In Auto, the classifier would decide.)'
                  : 'This folder runs in Auto: the classifier decides. (In Ask, a permission card would appear.)'}
              </span>
            </div>
            {asks.length > 0 && <ul class="rule-list">{asks.map(rowView)}</ul>}
          </section>
        </div>
      </div>
    </section>
  );
}

function CheckView({
  offline,
  q,
  check,
  mode,
  onAllow,
}: {
  offline: boolean;
  q: string;
  check: { q: string; result: CheckResult | null } | null;
  mode: PermissionMode;
  onAllow: (rule: string) => void;
}) {
  const card = mode === 'ask' ? 'A permission card will appear.' : 'In Auto, the classifier decides.';
  let label = '—';
  let kind = 'none';
  let body = <>Type a command to see what happens.</>;
  let suggestion: string | undefined;
  const r = check && check.q === q ? check.result : null;
  if (offline) {
    label = 'Unavailable';
    body = <>The worker is offline, so commands can’t be checked.</>;
  } else if (q && !r) {
    label = '…';
    body = <>Checking on the worker</>;
  } else if (r) {
    const by = r.rule && (
      <>
        by rule <span class="mono rule-hit">{r.rule}</span>
        <span class="muted"> · {r.source === 'hqc' ? HQC : r.source}</span>
      </>
    );
    if (r.verdict === 'deny') {
      label = 'Denied';
      kind = 'deny';
      body = <>{by}</>;
    } else if (r.verdict === 'allow') {
      label = 'Allowed';
      kind = 'allow';
      body = by || <>all {r.parts} parts are allowed</>;
    } else {
      label = 'Will ask';
      kind = 'ask';
      suggestion = r.suggestions.find((s) => s.rule)?.rule;
      body =
        r.parts === 0 ? (
          <>The worker can’t split this command into simple parts, so no rule covers it. {card}</>
        ) : r.parts > 1 ? (
          <>
            {r.uncovered} of {r.parts} parts not covered. {mode === 'ask' ? 'A permission card will appear.' : 'The classifier decides.'}
          </>
        ) : (
          <>{card}</>
        );
    }
  }
  return (
    <div class={`rules-result is-${kind}`} role="status">
      <span class="rules-result-label">{label}</span>
      <span class="rules-result-text">{body}</span>
      {suggestion && (
        <button type="button" class="btn btn-small btn-allow-check" onClick={() => onAllow(suggestion!)}>
          <IconShield />
          Allow…
        </button>
      )}
    </div>
  );
}

/** "+ Allow in this folder": a live format check (an error blocks saving), a duplicate is an
 *  error, a deny of the worker or the files that would win is a warning (saving is allowed).
 *  On a phone a full-screen form (Cancel / New rule / Save). */
function RuleForm({
  app,
  workspaceId,
  path,
  phone,
  draft,
  taken,
  checks,
  onDraft,
  onSave,
  onCancel,
}: {
  app: AppState;
  workspaceId: string;
  path: string;
  phone?: boolean;
  draft: string;
  taken: string[];
  checks: boolean;
  onDraft: (v: string) => void;
  onSave: (rule: string) => Promise<void>;
  onCancel: () => void;
}) {
  const field = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [warning, setWarning] = useState<{ rule: string; text: string } | null>(null);
  useEffect(() => field.current?.focus(), []);
  const rule = draft.trim();
  const syntax = rule ? ruleError(rule) : null;
  const err = syntax ?? (taken.includes(rule) ? 'This rule is already in the folder.' : null);
  const settled = useDebounced(rule, CHECK_DELAY);
  useEffect(() => {
    if (!checks || !settled || ruleError(settled)) return;
    let live = true;
    const command = ruleCommand(settled);
    app.api.workspacePermissions(workspaceId, command ? { command } : { tool: settled }).then(
      (r) => {
        if (!live) return;
        const c = r.check;
        setWarning(
          c?.verdict === 'deny' && c.rule
            ? { rule: settled, text: `Won’t take effect: ${c.rule} in ${c.source}. You can still save it.` }
            : null,
        );
      },
      () => live && setWarning(null),
    );
    return () => {
      live = false;
    };
  }, [app, workspaceId, settled, checks]);
  const warn = !err && warning?.rule === rule ? warning.text : null;
  const save = async () => {
    if (!rule || err) return;
    setBusy(true);
    setSaveError(null);
    try {
      await onSave(rule);
    } catch (e) {
      setSaveError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };
  const disabled = busy || !rule || !!err;
  const body = (
    <>
      <label for="rules-new" class="rules-form-label">
        New rule for {path}
      </label>
      <input
        id="rules-new"
        ref={field}
        class={`input mono ${err ? 'is-invalid' : ''}`}
        placeholder="Bash(npm test *)"
        value={draft}
        onInput={(e) => onDraft((e.currentTarget as HTMLInputElement).value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            void save();
          } else if (e.key === 'Escape') onCancel();
        }}
      />
      <div class="rules-chips">
        <span class="muted small">Format:</span>
        {FORMATS.map((f) => (
          <button key={f} type="button" class="chip-btn mono" onClick={() => onDraft(f)}>
            {f}
          </button>
        ))}
      </div>
      {err && <div class="rules-msg is-error">{err}</div>}
      {warn && <div class="rules-msg is-warn">{warn}</div>}
      {saveError && <div class="rules-msg is-error">{saveError}</div>}
    </>
  );
  if (phone) {
    return (
      <section class="rules-form-page" aria-label="New rule">
        <header class="rules-form-bar">
          <button type="button" class="btn btn-ghost" onClick={onCancel}>
            Cancel
          </button>
          <div class="rules-form-title">New rule</div>
          <button type="button" class="btn btn-ghost btn-save-rule" disabled={disabled} onClick={() => void save()}>
            Save
          </button>
        </header>
        <div class="rules-form-body">{body}</div>
      </section>
    );
  }
  return (
    <div class="rules-form">
      {body}
      <div class="row">
        <button type="button" class="btn btn-small btn-primary btn-save-rule" disabled={disabled} onClick={() => void save()}>
          Save
        </button>
        <button type="button" class="btn btn-small btn-ghost" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </div>
  );
}
