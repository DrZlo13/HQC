// A new room (`#/p/<project>/new-room`): a chat with several agents, each with its own model and
// prompt, opened from the room button beside "+" of the session column. The form names the room,
// picks its mode, where its agents work (the workspace, the folder and the permissions, with the
// choices of a new session) and the agents; "Create room" makes the room and its agents on the
// hub and opens the room's chat.

import { useEffect, useRef, useState } from 'preact/hooks';
import type { AppState } from '../app';
import { onBack, replaceHash } from '../nav';
import { ROOM_MODES, type PermissionMode, type RoomMode } from '../proto';
import { ModelField } from './agentSettings';
import { projectName } from './Appearance';
import { useSubscription } from './hooks';
import { IconBack, IconPlus } from './icons';
import { ROOM_MODE_LABELS, homePath, modeConfirm } from './labels';
import { PERMISSIONS } from './NewSession';
import { workspaceName } from './ProjectSettings';

/** What each mode does, under its name. */
const MODE_NOTES: Record<RoomMode, string> = {
  conversation: "An agent's reply wakes the other agents",
  council: "Only your message wakes the agents, all at once; they see the room's history",
};

/** An agent of the form; `key` keeps a row's fields with it when a row above is removed. */
interface AgentRow {
  key: number;
  model?: string;
  /** A provider instead of a Claude model, as on a new session. */
  provider_id?: string;
  prompt: string;
}

export function NewRoom({ app, projectId, phone = false }: { app: AppState; projectId: string; phone?: boolean }) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [title, setTitle] = useState('');
  const [mode, setMode] = useState<RoomMode>('conversation');
  const [workspace, setWorkspace] = useState<string | undefined>(undefined);
  const [cwd, setCwd] = useState('');
  const [permission, setPermission] = useState<PermissionMode | undefined>(undefined);
  const [agents, setAgents] = useState<AgentRow[]>([
    { key: 0, prompt: '' },
    { key: 1, prompt: '' },
  ]);
  const nextKey = useRef(2);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const project = app.project(projectId);
  const list = app.workspaces.get(projectId);
  useEffect(() => {
    app.loadWorkspaces(projectId).catch((e: Error) => setError(`Workspaces not loaded: ${e.message}`));
  }, [app, projectId]);
  // As for a new session: the one picked, else the project's default, else its first.
  const ws = app.newSessionWorkspace({ projectId, workspace_id: workspace, cwd });
  // The providers its worker allows, in each agent's model menu.
  const providers = app.providersFor(list?.find((w) => w.id === ws)?.worker_id);
  // An agent's provider that this workspace's worker does not allow is neither shown nor sent,
  // as on a new session.
  const allowed = (id: string | undefined) => providers.find((p) => p.id === id)?.id;
  const edit = (key: number, patch: Partial<AgentRow>) => setAgents((cur) => cur.map((a) => (a.key === key ? { ...a, ...patch } : a)));

  const create = async (e: Event) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    const folder = cwd.trim();
    try {
      const s = await app.createRoom(projectId, {
        title: title.trim() || undefined,
        mode,
        workspace_id: ws,
        cwd: folder && folder !== '.' ? folder : undefined,
        permission_mode: permission,
        agents: agents.map((a) => ({ model: a.model, provider_id: allowed(a.provider_id), system_prompt: a.prompt.trim() || undefined })),
      });
      replaceHash(`#/s/${s.id}`);
    } catch (err) {
      setError((err as Error).message);
      setBusy(false);
    }
  };

  return (
    <section class={`page new-room ${phone ? 'is-phone' : ''}`}>
      <header class="bar">
        <a class="back icon-btn" href={`#/p/${projectId}`} aria-label="Back to sessions" title="Back" onClick={(e) => onBack(e, `#/p/${projectId}`)}>
          <IconBack />
        </a>
        <div class="bar-title">
          <h1 class="title">New room</h1>
          <div class="state">{project ? projectName(project) : ''}</div>
        </div>
      </header>
      <div class="page-scroll">
        <form class="page-body room-form" onSubmit={create}>
          <section class="ps-section">
            <label class="field">
              <span class="field-label">Title (optional)</span>
              <input class="input" name="title" value={title} onInput={(e) => setTitle((e.currentTarget as HTMLInputElement).value)} />
            </label>
            <fieldset class="room-modes">
              <legend class="field-label">Mode</legend>
              {ROOM_MODES.map((m) => (
                <label key={m} class={`room-mode ps-card room-mode-${m}`}>
                  <input type="radio" name="mode" value={m} checked={mode === m} onChange={() => setMode(m)} />
                  <span class="room-mode-text">
                    <span class="room-mode-name">{ROOM_MODE_LABELS[m]}</span>
                    <span class="muted small">{MODE_NOTES[m]}</span>
                  </span>
                </label>
              ))}
            </fieldset>
          </section>

          <section class="ps-section">
            <label class="field">
              <span class="field-label">Workspace</span>
              <select
                class="input"
                name="workspace"
                value={ws ?? ''}
                disabled={!list?.length}
                onChange={(e) => setWorkspace((e.currentTarget as HTMLSelectElement).value)}
              >
                {(list ?? []).map((w) => (
                  <option key={w.id} value={w.id}>
                    {`${workspaceName(w)}${w.id === project?.default_workspace_id ? ' (default)' : ''} — ${[app.workerName(w.worker_id), homePath(w.path)].filter(Boolean).join(':')}`}
                  </option>
                ))}
              </select>
            </label>
            <a class="small room-add-workspace" href={`#/p/${projectId}/settings`}>
              Add a workspace…
            </a>
            <label class="field">
              <span class="field-label">Folder inside the workspace</span>
              <input class="input mono" name="cwd" placeholder="." value={cwd} onInput={(e) => setCwd((e.currentTarget as HTMLInputElement).value)} />
            </label>
            <label class="field">
              <span class="field-label">Permissions</span>
              <select
                class="input"
                name="permission_mode"
                value={permission ?? ''}
                onChange={(e) => {
                  const el = e.currentTarget as HTMLSelectElement;
                  const m = (el.value || undefined) as PermissionMode | undefined;
                  // Auto and Allow all after a one-line confirmation of their own, as on a new
                  // session's chip; declined, the field keeps what it had.
                  const line = m && m !== permission ? modeConfirm(m) : undefined;
                  if (line && !confirm(line)) {
                    el.value = permission ?? '';
                    return;
                  }
                  setPermission(m);
                }}
              >
                {PERMISSIONS.map(([m, label]) => (
                  <option key={label} value={m ?? ''}>
                    {label}
                  </option>
                ))}
              </select>
            </label>
          </section>

          <section class="ps-section room-agents" aria-labelledby="room-agents">
            <h2 id="room-agents">Agents</h2>
            {agents.map((a, i) => (
              <div key={a.key} class="room-agent ps-card">
                <div class="room-agent-head">
                  <span class="room-agent-name">Agent {i + 1}</span>
                  <button
                    type="button"
                    class="btn btn-small btn-ghost btn-remove-agent"
                    disabled={agents.length < 2}
                    onClick={() => setAgents((cur) => cur.filter((x) => x.key !== a.key))}
                  >
                    Remove
                  </button>
                </div>
                <ModelField
                  value={a.model}
                  onChange={(model) => edit(a.key, { model, provider_id: undefined })}
                  providers={providers}
                  provider={allowed(a.provider_id)}
                  onProvider={(provider_id) => edit(a.key, { provider_id, model: undefined })}
                />
                <label class="field">
                  <span class="field-label">Prompt</span>
                  <textarea
                    class="input room-prompt"
                    name="system_prompt"
                    rows={3}
                    value={a.prompt}
                    onInput={(e) => edit(a.key, { prompt: (e.currentTarget as HTMLTextAreaElement).value })}
                  />
                </label>
              </div>
            ))}
            <div class="row">
              <button
                type="button"
                class="btn btn-small btn-add-agent"
                onClick={() => {
                  const key = nextKey.current++;
                  setAgents((cur) => [...cur, { key, prompt: '' }]);
                }}
              >
                <IconPlus size={13} />
                Add agent
              </button>
            </div>
          </section>

          {error && <div class="error-text">{error}</div>}
          <div class="row">
            <button type="submit" class="btn btn-primary btn-create-room" disabled={busy}>
              Create room
            </button>
          </div>
        </form>
      </div>
    </section>
  );
}
