// "MCP servers" of Settings (SPEC 0.235, iteration 7 T66): the owner's remote MCP servers,
// which the hub relays to the agents of the projects it is given to as `mcp__<name>__<tool>`.
// Header values (tokens) are write-only: the hub seals them and never sends them back, so the
// list shows header names only. A value lives in the form's memory until the form closes and
// goes nowhere else: no draft, no storage.

import { useEffect, useRef, useState } from 'preact/hooks';
import { ApiError, type McpHeader, type McpProbe, type McpServerFields } from '../api';
import type { AppState } from '../app';
import type { LocalMcpServer, McpServer, McpServerList, Workspace } from '../proto';
import { projectName } from './Appearance';
import { useUrlProbe } from './urlprobe';

export const MCP_TIMEOUT_MIN = 30;
export const MCP_TIMEOUT_MAX = 1800;
export const MCP_TIMEOUT_DEFAULT = 300;
const MAX_HEADERS = 8;
/** More tools than this fold under "N tools". */
const TOOLS_SHOWN = 8;

/** A server name as the hub takes it (`valid_server_name`): null when good, else why not. */
export function mcpNameProblem(name: string): string | null {
  const good =
    /^[a-z0-9_-]{1,32}$/.test(name) && name !== 'hqc' && !name.startsWith('_') && !name.endsWith('_') && !name.includes('__');
  return good
    ? null
    : 'Name: 1 to 32 lowercase letters, digits, “-” and “_”, not “hqc”, no “_” at either end and no “__”.';
}

const LOOPBACK = /^(localhost|127(\.\d{1,3}){3}|\[::1\])$/i;

/** The URL as the hub takes it: https (http only to a loopback host), no user or password. */
export function mcpUrlProblem(url: string): string | null {
  let u: URL;
  try {
    u = new URL(url);
  } catch {
    return 'URL: a full address, https://…';
  }
  if (u.username || u.password) return 'URL: without a user or password; give a token as a header.';
  if (u.protocol === 'https:') return null;
  if (u.protocol === 'http:' && LOOPBACK.test(u.hostname)) return null;
  return 'URL: https only (http only to this machine, localhost).';
}

export function mcpTimeoutProblem(text: string): string | null {
  const n = Number(text);
  return /^\d+$/.test(text.trim()) && n >= MCP_TIMEOUT_MIN && n <= MCP_TIMEOUT_MAX
    ? null
    : `Timeout: whole seconds from ${MCP_TIMEOUT_MIN} to ${MCP_TIMEOUT_MAX}.`;
}

function headersProblem(rows: McpHeader[]): string | null {
  if (rows.length > MAX_HEADERS) return `At most ${MAX_HEADERS} headers.`;
  for (const h of rows) {
    if (!/^[A-Za-z0-9_-]+$/.test(h.name)) return 'Header name: letters, digits, “-” and “_”.';
    if (!h.value) return `Header ${h.name}: a value is needed.`;
    if (/[\r\n]/.test(h.value) || h.value.length > 4096) return `Header ${h.name}: one line, at most 4 KiB.`;
  }
  return null;
}

/** The hub's error, with what to do for the ones the owner can fix only on the hub. */
export function mcpErrorText(e: unknown): string {
  if (e instanceof ApiError && e.code === 'mcp_key_missing') {
    return 'The hub keeps no tokens: set HQC_MCP_KEY on the hub to give a server headers.';
  }
  return (e as Error).message;
}

/** The servers project `projectId` gets. */
export function serversOf(list: McpServer[], projectId: string): McpServer[] {
  return list.filter((s) => s.all_projects || s.project_ids.includes(projectId));
}

/** The servers from the hub; a stub without the call or a failure: none (the page still shows). */
function loadServers(app: AppState): Promise<McpServerList> {
  return Promise.resolve().then(() => app.api.mcpServers());
}

/** The machines' servers the agents of a project's workspaces get (SPEC 0.262): those of each
 *  workspace's worker in the workspace's environment, once each. */
export function localServersOf(local: LocalMcpServer[], workspaces: Workspace[]): LocalMcpServer[] {
  return local.filter((l) => workspaces.some((w) => w.worker_id === l.worker_id && w.environment === l.environment));
}

/** How a server is declared on a machine, for the line under "On machines". */
const LOCAL_HOWTO = '[environments.<name>.mcp_servers.<server>]';

/** "On machines": the servers of the owner's workers' `worker.toml`, read-only (SPEC 0.262). */
function LocalServers({ local }: { local: LocalMcpServer[] }) {
  return (
    <div class="mcp-local">
      <h3>On machines</h3>
      <p class="muted">
        MCP servers that run on a worker, given to the sessions of one of its environments. They are declared on that machine,
        in worker.toml under <span class="mono">{LOCAL_HOWTO}</span>, and cannot be changed here; their tools are pinned, and a
        change waits for <span class="mono">hqc-worker mcp approve</span> there.
      </p>
      {local.length === 0 ? (
        <p class="empty-inline">None.</p>
      ) : (
        <ul class="plain-list mcp-local-list">
          {local.map((l) => (
            <li key={`${l.worker_id}/${l.environment}/${l.name}`} class="mcp-local-row">
              <span class="mcp-name mono">{l.name}</span>
              <span class="mcp-meta">
                {' '}
                · {l.worker_name} · env {l.environment}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function nameOfProject(app: AppState, id: string): string {
  const p = app.sortedProjects().find((x) => x.id === id) ?? app.project?.(id);
  return p ? projectName(p) : 'a deleted project';
}

function StatusTag({ s }: { s: McpServer }) {
  if (s.status === 'ok') return <span class="tag mcp-status mcp-status-ok">ok</span>;
  if (s.auth === 'oauth' && (!s.signed_in || s.status === 'sign_in'))
    return <span class="tag tag-failed mcp-status mcp-status-sign-in">sign in</span>;
  if (s.status === 'failing') return <span class="tag tag-failed mcp-status mcp-status-failing">failing</span>;
  return <span class="tag tag-progress mcp-status mcp-status-unknown">not tested yet</span>;
}

function Tools({ tools }: { tools: string[] }) {
  if (!tools.length) return null;
  const list = <span class="mcp-tools-list mono">{tools.join(', ')}</span>;
  return tools.length > TOOLS_SHOWN ? (
    <details class="mcp-tools">
      <summary>{tools.length} tools</summary>
      {list}
    </details>
  ) : (
    <div class="mcp-tools">Tools: {list}</div>
  );
}

export function McpServersSection({ app }: { app: AppState }) {
  const [servers, setServers] = useState<McpServer[] | null>(null);
  const [local, setLocal] = useState<LocalMcpServer[]>([]);
  // The server being edited: its id, `new` for one being added, or none.
  const [editing, setEditing] = useState<string | null>(null);
  const [testing, setTesting] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    loadServers(app).then(
      (l) => {
        setServers(l.servers);
        setLocal(l.local);
      },
      (e) => {
        setServers([]);
        if (e instanceof ApiError) setError(e.message);
      },
    );
  }, [app]);

  const put = (s: McpServer) =>
    setServers((cur) => [...(cur ?? []).filter((x) => x.id !== s.id), s].sort((a, b) => a.name.localeCompare(b.name)));

  const test = async (s: McpServer) => {
    setError(null);
    setTesting(s.id);
    try {
      put(await app.api.testMcpServer(s.id));
    } catch (e) {
      setError(mcpErrorText(e));
    } finally {
      setTesting(null);
    }
  };

  // The page opens in a tab of its own; the hub's callback page there says when it is done. The
  // tab is opened before the request, so that a phone does not block it as a pop-up.
  const signIn = async (s: McpServer) => {
    setError(null);
    const tab = window.open('', '_blank');
    try {
      const url = await app.api.signInMcpServer(s.id);
      if (tab) {
        tab.opener = null;
        tab.location.href = url;
      } else location.href = url;
    } catch (e) {
      tab?.close();
      setError(mcpErrorText(e));
    }
  };

  const signOut = async (s: McpServer) => {
    setError(null);
    try {
      await app.api.signOutMcpServer(s.id);
      put({ ...s, signed_in: false, status: 'sign_in' });
    } catch (e) {
      setError(mcpErrorText(e));
    }
  };

  const remove = async (s: McpServer) => {
    if (!confirm(`Delete the MCP server “${s.name}”? Agents no longer get its tools.`)) return;
    setError(null);
    try {
      await app.api.deleteMcpServer(s.id);
      setServers((cur) => (cur ?? []).filter((x) => x.id !== s.id));
    } catch (e) {
      setError(mcpErrorText(e));
    }
  };

  /** Saves and closes the form; a failure leaves it open with the hub's reason. */
  const save = async (id: string | undefined, f: McpServerFields): Promise<boolean> => {
    setError(null);
    try {
      put(id ? await app.api.updateMcpServer(id, f) : await app.api.createMcpServer(f));
      setEditing(null);
      return true;
    } catch (e) {
      setError(mcpErrorText(e));
      return false;
    }
  };

  const close = () => {
    setError(null);
    setEditing(null);
  };

  return (
    <>
      <h2>MCP servers</h2>
      <p class="muted">
        Remote MCP servers (Streamable HTTP) whose tools the agents of the chosen projects get, as mcp__&lt;name&gt;__&lt;tool&gt;. The
        hub connects to them; a token goes in a header, which the hub keeps sealed and never shows again.
      </p>
      {servers && servers.length > 0 && (
        <ul class="plain-list mcp-list">
          {servers.map((s) =>
            editing === s.id ? (
              <li key={s.id}>
                <McpServerForm app={app} initial={s} onSave={(f) => save(s.id, f)} onCancel={close} />
              </li>
            ) : (
              <li key={s.id} class="mcp-row">
                <div class="mcp-head">
                  <span class="mcp-name mono">{s.name}</span>
                  <StatusTag s={s} />
                </div>
                <span class="mcp-url mono">{s.url}</span>
                {s.status === 'failing' && s.last_error && <span class="error-text mcp-error">{s.last_error}</span>}
                <Tools tools={s.tools} />
                <span class="mcp-meta">
                  {s.all_projects
                    ? 'All projects'
                    : s.project_ids.length
                      ? s.project_ids.map((id) => nameOfProject(app, id)).join(', ')
                      : 'No project yet'}
                  {s.auth === 'oauth' ? ' · OAuth' : s.header_names.length > 0 && ` · headers: ${s.header_names.join(', ')}`}
                  {` · timeout ${s.timeout_s} s`}
                </span>
                <div class="row mcp-buttons">
                  <button type="button" class="btn btn-small btn-test-mcp" disabled={testing === s.id} onClick={() => test(s)}>
                    {testing === s.id ? 'Testing…' : 'Test'}
                  </button>
                  {s.auth === 'oauth' && (
                    <button type="button" class="btn btn-small btn-primary btn-sign-in-mcp" onClick={() => signIn(s)}>
                      {s.signed_in ? 'Sign in again' : 'Sign in'}
                    </button>
                  )}
                  {s.auth === 'oauth' && s.signed_in && (
                    <button type="button" class="btn btn-small btn-sign-out-mcp" onClick={() => signOut(s)}>
                      Sign out
                    </button>
                  )}
                  <button type="button" class="btn btn-small btn-edit-mcp" onClick={() => setEditing(s.id)}>
                    Edit
                  </button>
                  <button type="button" class="btn btn-small btn-revoke btn-delete-mcp" onClick={() => remove(s)}>
                    Delete
                  </button>
                </div>
              </li>
            ),
          )}
        </ul>
      )}
      {editing === 'new' ? (
        <McpServerForm app={app} onSave={(f) => save(undefined, f)} onCancel={close} />
      ) : (
        <button type="button" class="btn btn-add-mcp" onClick={() => setEditing('new')}>
          Add an MCP server
        </button>
      )}
      {error && <div class="error-text mcp-section-error">{error}</div>}
      <LocalServers local={local} />
    </>
  );
}

const NO_HEADER: McpHeader = { name: '', value: '' };

/** The probe's answer for the line under the URL. */
export function mcpProbeText(r: McpProbe): { text: string; error?: boolean } {
  if (r.error) return { text: r.error, error: true };
  if (r.auth === 'oauth') return { text: 'Signs in with OAuth' };
  if (r.auth === 'token') return { text: 'Needs a token' };
  if (r.auth === 'none') return { text: 'No sign-in needed' };
  return { text: 'Could not tell how it signs in' };
}

type Probe = { state: 'busy' } | { state: 'done'; text: string; error?: boolean };

/** The add or edit form. `initial`: the server being edited. On save the parent closes it,
 *  so the header values typed go with its state. */
function McpServerForm({
  app,
  initial,
  onSave,
  onCancel,
}: {
  app: AppState;
  initial?: McpServer;
  onSave: (f: McpServerFields) => Promise<boolean>;
  onCancel: () => void;
}) {
  const [name, setName] = useState(initial?.name ?? '');
  const [url, setUrl] = useState(initial?.url ?? '');
  const [timeout, setTimeoutText] = useState(String(initial?.timeout_s ?? MCP_TIMEOUT_DEFAULT));
  const [all, setAll] = useState(initial?.all_projects ?? true);
  const [picked, setPicked] = useState<string[]>(initial?.project_ids ?? []);
  // An existing server keeps its headers unless they are replaced: then all of them are typed
  // again, since the hub never sends a value back.
  const [replacing, setReplacing] = useState(!initial);
  const [rows, setRows] = useState<McpHeader[]>(initial ? [] : [NO_HEADER]);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // T65: how the hub signs in; the client id and scope are optional (the hub registers itself
  // where the server offers it), the secret write-only like a header's value.
  const [auth, setAuth] = useState<'headers' | 'oauth'>(initial?.auth ?? 'headers');
  const [clientId, setClientId] = useState(initial?.oauth_client_id ?? '');
  const [clientSecret, setClientSecret] = useState('');
  const [scope, setScope] = useState(initial?.oauth_scope ?? '');
  // SPEC 0.236: the URL is checked when it is pasted or typed, and the fields the owner has not
  // touched yet are filled from the answer; an edit starts with all of them touched.
  const touched = useRef(new Set<'name' | 'auth' | 'headers'>(initial ? ['name', 'auth', 'headers'] : []));
  const [probe, setProbe] = useState<Probe | null>(null);
  const urlProbe = useUrlProbe(url, (u, current) => {
    setProbe({ state: 'busy' });
    // Through a promise: an api without the call (a stub) is a failed check, not a throw.
    Promise.resolve(u).then((x) => app.api.probeMcp(x)).then(
      (r) => {
        if (!current()) return;
        setProbe({ state: 'done', ...mcpProbeText(r) });
        const t = touched.current;
        if (r.name && !t.has('name')) setName(r.name);
        if (r.auth === 'oauth' && !t.has('auth')) setAuth('oauth');
        if ((r.auth === 'token' || r.auth === 'none') && !t.has('auth')) setAuth('headers');
        // Rows not touched are the form's own: the empty one it starts with, or one filled here.
        if (r.auth === 'token' && !t.has('headers')) setRows([{ name: 'Authorization', value: '' }]);
        if (r.auth === 'none' && !t.has('headers')) setRows([]);
      },
      (e) => {
        if (current()) setProbe({ state: 'done', text: mcpErrorText(e), error: true });
      },
    );
  });

  const setRow = (i: number, patch: Partial<McpHeader>) => {
    touched.current.add('headers');
    setRows((cur) => cur.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  };

  const cancel = () => {
    setRows([]);
    setClientSecret('');
    onCancel();
  };

  const submit = async (e: Event) => {
    e.preventDefault();
    const headers = rows.map((r) => ({ name: r.name.trim(), value: r.value })).filter((r) => r.name || r.value);
    const why =
      mcpNameProblem(name.trim()) ?? mcpUrlProblem(url.trim()) ?? mcpTimeoutProblem(timeout) ?? (replacing ? headersProblem(headers) : null);
    setProblem(why);
    if (why) return;
    const f: McpServerFields = {
      name: name.trim(),
      url: url.trim(),
      timeout_s: Number(timeout),
      all_projects: all,
      project_ids: all ? [] : picked,
    };
    // A new server sends its headers if it has any; an edit only when they are replaced (an
    // empty list then removes them all).
    if (auth === 'headers' && (initial ? replacing : headers.length > 0)) f.headers = headers;
    if (auth !== (initial?.auth ?? 'headers')) f.auth = auth;
    if (auth === 'oauth') {
      if (clientId.trim() !== (initial?.oauth_client_id ?? '')) f.oauth_client_id = clientId.trim();
      if (clientSecret) f.oauth_client_secret = clientSecret;
      if (scope.trim() !== (initial?.oauth_scope ?? '')) f.oauth_scope = scope.trim();
    }
    setBusy(true);
    const ok = await onSave(f);
    setBusy(false);
    if (ok) {
      setRows([]);
      setClientSecret('');
    }
  };

  const projects = app.sortedProjects();
  return (
    <form class="mcp-form" onSubmit={submit}>
      <label class="field">
        <span class="field-label">URL</span>
        <input
          class="input mono"
          name="mcp_url"
          type="url"
          placeholder="https://api.githubcopilot.com/mcp/"
          autocomplete="off"
          value={url}
          onInput={(e) => setUrl((e.currentTarget as HTMLInputElement).value)}
          {...urlProbe}
        />
        {probe && (
          <span class={`small mcp-probe ${probe.state === 'done' && probe.error ? 'error-text' : 'muted'}`} role="status">
            {probe.state === 'busy' ? 'Checking…' : probe.text}
          </span>
        )}
      </label>
      <label class="field">
        <span class="field-label">Name</span>
        <input
          class="input mono"
          name="mcp_name"
          placeholder="github"
          autocomplete="off"
          value={name}
          onInput={(e) => {
            touched.current.add('name');
            setName((e.currentTarget as HTMLInputElement).value);
          }}
        />
        <span class="muted small">The agents see its tools as mcp__{name.trim() || '<name>'}__&lt;tool&gt;.</span>
      </label>
      <label class="field">
        <span class="field-label">
          Timeout of a call, seconds ({MCP_TIMEOUT_MIN}–{MCP_TIMEOUT_MAX})
        </span>
        <input
          class="input mono"
          name="mcp_timeout"
          inputMode="numeric"
          value={timeout}
          onInput={(e) => setTimeoutText((e.currentTarget as HTMLInputElement).value)}
        />
      </label>

      <label class="field">
        <span class="field-label">Sign in with</span>
        <select
          class="input"
          name="mcp_auth"
          value={auth}
          onChange={(e) => {
            touched.current.add('auth');
            setAuth((e.currentTarget as HTMLSelectElement).value === 'oauth' ? 'oauth' : 'headers');
          }}
        >
          <option value="headers">A token in a header</option>
          <option value="oauth">OAuth (sign in on the service's page)</option>
        </select>
      </label>
      {auth === 'oauth' && (
        <fieldset class="mcp-oauth">
          <legend class="field-label">OAuth</legend>
          <p class="muted small">
            After saving, press Sign in. A client id is needed only when the service does not let hqc register itself.
          </p>
          <input
            class="input mono"
            name="mcp_client_id"
            aria-label="Client id"
            placeholder="Client id (optional)"
            autocomplete="off"
            value={clientId}
            onInput={(e) => setClientId((e.currentTarget as HTMLInputElement).value)}
          />
          <input
            class="input mono"
            name="mcp_client_secret"
            type="password"
            aria-label="Client secret"
            placeholder={initial?.auth === 'oauth' ? 'Client secret (kept unless typed)' : 'Client secret (optional)'}
            autocomplete="off"
            value={clientSecret}
            onInput={(e) => setClientSecret((e.currentTarget as HTMLInputElement).value)}
          />
          <input
            class="input mono"
            name="mcp_scope"
            aria-label="Scope"
            placeholder="Scope (optional)"
            autocomplete="off"
            value={scope}
            onInput={(e) => setScope((e.currentTarget as HTMLInputElement).value)}
          />
        </fieldset>
      )}
      {auth === 'headers' && (
      <fieldset class="mcp-headers">
        <legend class="field-label">Headers</legend>
        {initial && !replacing && (
          <>
            <p class="muted small mcp-headers-kept">
              {initial.header_names.length
                ? `Kept: ${initial.header_names.join(', ')}. Their values stay as they are unless you replace the headers.`
                : 'None. Replace the headers to add some.'}
            </p>
            <button
              type="button"
              class="btn btn-small btn-replace-headers"
              onClick={() => {
                // The names to fill in again, each with its value.
                setRows(initial.header_names.map((n) => ({ name: n, value: '' })));
                setReplacing(true);
              }}
            >
              Replace headers
            </button>
          </>
        )}
        {replacing && (
          <>
            {initial && (
              <p class="muted small">
                Type every header again, values too: the ones you leave out are removed.
              </p>
            )}
            {rows.map((r, i) => (
              <div key={i} class="row mcp-header-row">
                <input
                  class="input mono"
                  name="mcp_header_name"
                  aria-label="Header name"
                  placeholder="Authorization"
                  autocomplete="off"
                  value={r.name}
                  onInput={(e) => setRow(i, { name: (e.currentTarget as HTMLInputElement).value })}
                />
                <input
                  class="input mono"
                  name="mcp_header_value"
                  type="password"
                  aria-label="Header value"
                  placeholder="Bearer …"
                  autocomplete="off"
                  value={r.value}
                  onInput={(e) => setRow(i, { value: (e.currentTarget as HTMLInputElement).value })}
                />
                <button
                  type="button"
                  class="btn btn-small btn-ghost btn-remove-header"
                  onClick={() => {
                    touched.current.add('headers');
                    setRows((cur) => cur.filter((_, j) => j !== i));
                  }}
                >
                  Remove
                </button>
              </div>
            ))}
            {rows.length < MAX_HEADERS && (
              <button type="button" class="btn btn-small btn-add-header" onClick={() => {
                  touched.current.add('headers');
                  setRows((cur) => [...cur, NO_HEADER]);
                }}>
                Add a header
              </button>
            )}
          </>
        )}
      </fieldset>
      )}

      <fieldset class="mcp-projects">
        <legend class="field-label">Projects</legend>
        <label class="mcp-all">
          <input type="checkbox" name="mcp_all_projects" checked={all} onChange={(e) => setAll((e.currentTarget as HTMLInputElement).checked)} />
          <span>All projects</span>
        </label>
        {!all &&
          projects.map((p) => (
            <label key={p.id} class="mcp-project">
              <input
                type="checkbox"
                name="mcp_project"
                value={p.id}
                checked={picked.includes(p.id)}
                onChange={(e) => {
                  const on = (e.currentTarget as HTMLInputElement).checked;
                  setPicked((cur) => (on ? [...cur.filter((x) => x !== p.id), p.id] : cur.filter((x) => x !== p.id)));
                }}
              />
              <span>{projectName(p)}</span>
            </label>
          ))}
      </fieldset>

      {problem && <div class="error-text mcp-problem">{problem}</div>}
      <div class="actions">
        <button type="submit" class="btn btn-primary btn-save-mcp" disabled={busy}>
          Save
        </button>
        <button type="button" class="btn btn-ghost btn-cancel-mcp" onClick={cancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}

/** A project's settings: the MCP servers its agents get, read-only (they are edited in
 *  Settings → MCP servers). Nothing at all when the owner has none. */
export function ProjectMcpServers({ app, projectId }: { app: AppState; projectId: string }) {
  const [list, setList] = useState<McpServerList | null>(null);
  useEffect(() => {
    loadServers(app).then(setList, () => setList(null));
  }, [app]);
  const workspaces = app.workspaces?.get(projectId) ?? [];
  const machines = list ? localServersOf(list.local, workspaces) : [];
  if (!list || (list.servers.length === 0 && machines.length === 0)) return null;
  const mine = serversOf(list.servers, projectId);
  return (
    <section class="ps-section mcp-project-servers" aria-labelledby="ps-mcp">
      <h2 id="ps-mcp">MCP servers</h2>
      <p class="ps-text">
        Remote MCP servers whose tools this project’s agents get. They are added and given to projects in{' '}
        <a href="#/settings">Settings → MCP servers</a>.
      </p>
      {mine.length === 0 ? (
        machines.length === 0 && <p class="empty-inline">None for this project.</p>
      ) : (
        mine.map((s) => (
          <div key={s.id} class="mcp-row ps-card">
            <div class="mcp-head">
              <span class="mcp-name mono">{s.name}</span>
              <StatusTag s={s} />
              {s.all_projects && <span class="muted small">all projects</span>}
            </div>
            <span class="mcp-url mono">{s.url}</span>
            <Tools tools={s.tools} />
          </div>
        ))
      )}
      {machines.length > 0 && (
        <>
          <p class="ps-text">On the machines of this project’s folders, declared in their worker.toml:</p>
          {machines.map((l) => (
            <div key={`${l.worker_id}/${l.environment}/${l.name}`} class="mcp-local-row ps-card">
              <span class="mcp-name mono">{l.name}</span>
              <span class="mcp-meta">
                {' '}
                · {l.worker_name} · env {l.environment}
              </span>
            </div>
          ))}
        </>
      )}
    </section>
  );
}
