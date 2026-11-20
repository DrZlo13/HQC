// A new room (`#/p/<project>/new-room`): a chat with several agents, each with its own model and
// prompt, opened from "New room…" in the session column head's "⋯". It is a chat window as
// a new session is: the chips over the input choose the room's mode, where its agents work (the
// workspace, the folder and the permissions, the chips of a new session) and the agents, each a
// chip that opens its name, model and prompt; the first message makes the room and goes in as
// its first message.

import type { ComponentChildren } from 'preact';
import { useLayoutEffect, useRef, useState } from 'preact/hooks';
import type { AppState } from '../app';
import type { ModelProvider, RoomMode } from '../proto';
import { ModelField } from './agentSettings';
import { ContextMenu } from './ContextMenu';
import { useSubscription } from './hooks';
import { IconPlus } from './icons';
import { modelName } from './labels';
import { ChipButton, NewChatWindow, NewSessionChips, newSessionWhere, type ChipAt, type ChipValues } from './NewSession';
import type { SessionsToggle } from './Chat';

/** An agent of the window; `key` keeps its fields with it when an agent before it is removed. */
interface AgentRow {
  key: number;
  /** What the room's feed calls the agent; empty: the hub names it. */
  name: string;
  model?: string;
  /** A provider instead of a Claude model, as on a new session. */
  provider_id?: string;
  prompt: string;
}

export function NewRoom({
  app,
  projectId,
  phone = false,
  sessions,
}: {
  app: AppState;
  projectId: string;
  phone?: boolean;
  sessions?: SessionsToggle;
}) {
  useSubscription((fn) => app.onChange(fn), [app]);
  const [mode, setMode] = useState<RoomMode>('conversation');
  const [chosen, setChosen] = useState<Pick<ChipValues, 'workspace_id' | 'cwd' | 'permission_mode'>>({ cwd: '' });
  const [agents, setAgents] = useState<AgentRow[]>([
    { key: 0, name: '', prompt: '' },
    { key: 1, name: '', prompt: '' },
  ]);
  const nextKey = useRef(2);
  // As for a new session: the one picked, else the project's default, else its first.
  const draft = { projectId, ...chosen };
  const ws = app.newSessionWorkspace(draft);
  // The providers its worker allows, in each agent's model menu.
  const providers = app.providersFor(app.workspaces.get(projectId)?.find((w) => w.id === ws)?.worker_id);
  // An agent's provider that this workspace's worker does not allow is neither shown nor sent,
  // as on a new session.
  const allowed = (id: string | undefined) => providers.find((p) => p.id === id)?.id;
  // What the hub titles each agent left unnamed: its provider's label, else its model as typed,
  // else "default", numbered among the unnamed agents that have the same one.
  const label = (id: string | undefined, m: string | undefined) => providers.find((p) => p.id === allowed(id))?.label ?? (m?.trim() || 'default');
  const titles = agents.map((a, i) => {
    const mine = label(a.provider_id, a.model);
    const before = agents.slice(0, i).filter((x) => !x.name.trim() && label(x.provider_id, x.model) === mine).length;
    return before ? `${mine} ${before + 1}` : mine;
  });
  const edit = (key: number, patch: Partial<AgentRow>) => setAgents((cur) => cur.map((a) => (a.key === key ? { ...a, ...patch } : a)));

  const create = () => {
    const folder = chosen.cwd.trim();
    return app.createRoom(projectId, {
      mode,
      workspace_id: ws,
      cwd: folder && folder !== '.' ? folder : undefined,
      permission_mode: chosen.permission_mode,
      agents: agents.map((a) => ({ name: a.name.trim() || undefined, model: a.model, provider_id: allowed(a.provider_id), system_prompt: a.prompt.trim() || undefined })),
    });
  };

  return (
    <NewChatWindow
      app={app}
      projectId={projectId}
      phone={phone}
      sessions={sessions}
      kind="room"
      where={newSessionWhere(app, draft)}
      hint="Mode, agents, workspace, folder and permissions are above the box; an agent's name, model and prompt open from its chip."
      create={create}
      chips={(disabled) => (
        <>
          <AgentChips
            agents={agents}
            titles={titles}
            models={agents.map((a) => label(a.provider_id, a.model && modelName(a.model)))}
            providers={providers}
            allowed={allowed}
            edit={edit}
            onAdd={() => {
              const key = nextKey.current++;
              setAgents((cur) => [...cur, { key, name: '', prompt: '' }]);
              return key;
            }}
            onRemove={(key) => setAgents((cur) => cur.filter((x) => x.key !== key))}
            phone={phone}
            disabled={disabled}
          />
          <NewSessionChips
            app={app}
            projectId={projectId}
            values={chosen}
            onChange={(patch) => setChosen((cur) => ({ ...cur, ...patch }))}
            mode={{ value: mode, onChange: setMode }}
            phone={phone}
            disabled={disabled}
          />
        </>
      )}
    />
  );
}

/** The agents' chips over the input, one each: its name and model, opening its menu (name,
 *  model, prompt, "Remove agent"); "+ Agent" adds one and opens its menu. */
function AgentChips({
  agents,
  titles,
  models,
  providers,
  allowed,
  edit,
  onAdd,
  onRemove,
  phone,
  disabled,
}: {
  agents: AgentRow[];
  /** What the hub would title each agent, for the name's placeholder and the chip. */
  titles: string[];
  /** The model each agent's chip shows: its provider's label, else the model's name. */
  models: string[];
  providers: ModelProvider[];
  allowed: (id: string | undefined) => string | undefined;
  edit: (key: number, patch: Partial<AgentRow>) => void;
  /** Adds an agent; its key. */
  onAdd: () => number;
  onRemove: (key: number) => void;
  phone: boolean;
  disabled: boolean;
}) {
  // The agent whose menu is open, and the chip it stands on: its own, or "+ Agent" that added it.
  const [open, setOpen] = useState<(ChipAt & { key: number; added?: boolean }) | null>(null);
  const close = () => setOpen(null);
  const shown = open ? agents.findIndex((a) => a.key === open.key) : -1;
  let menu: ComponentChildren = null;
  if (open && shown >= 0) {
    const a = agents[shown];
    menu = (
      <ContextMenu x={open.x} y={open.y} above anchor={open.anchor} backdrop={phone} onClose={close} label="Agent">
        <div class="menu-agent">
          <label class="field">
            <span class="field-label">Name</span>
            <input
              class="input"
              name="agent_name"
              placeholder={titles[shown]}
              data-focus
              value={a.name}
              onInput={(e) => edit(a.key, { name: (e.currentTarget as HTMLInputElement).value })}
            />
          </label>
          <ModelField
            value={a.model}
            onChange={(model) => edit(a.key, { model, provider_id: undefined })}
            providers={providers}
            provider={allowed(a.provider_id)}
            onProvider={(provider_id) => edit(a.key, { provider_id, model: undefined })}
          />
          <label class="field">
            <span class="field-label">Prompt</span>
            <PromptBox value={a.prompt} onInput={(prompt) => edit(a.key, { prompt })} />
          </label>
          <button
            type="button"
            class="btn btn-small btn-ghost btn-remove-agent"
            disabled={agents.length < 2}
            onClick={() => {
              close();
              onRemove(a.key);
            }}
          >
            Remove agent
          </button>
        </div>
      </ContextMenu>
    );
  }
  return (
    <div class="new-chips-row">
      {agents.map((a, i) => (
        <ChipButton
          key={a.key}
          cls="btn-chip-agent"
          open={open?.key === a.key && !open.added}
          label={`Agent: ${a.name.trim() || titles[i]}`}
          disabled={disabled}
          onOpen={(chipAt) => setOpen({ ...chipAt, key: a.key })}
          onClose={close}
        >
          <span class="new-chip-text">
            {a.name.trim() ? (
              <>
                <b>{a.name.trim()}</b> · {models[i]}
              </>
            ) : (
              // Its title says the model already.
              <b>{titles[i]}</b>
            )}
          </span>
        </ChipButton>
      ))}
      <ChipButton
        cls="btn-add-agent"
        open={!!open?.added}
        label="Add an agent"
        disabled={disabled}
        onOpen={(chipAt) => setOpen({ ...chipAt, key: onAdd(), added: true })}
        onClose={close}
      >
        <IconPlus size={13} />
        <span class="new-chip-text">Agent</span>
      </ChipButton>
      {menu}
    </div>
  );
}

/** An agent's prompt: at least 8 rows, as tall as its text up to 45vh (`.room-prompt`), scrolling
 *  past that. The height follows the text as it is typed and when the menu opens with a prompt
 *  in it; a height dragged by hand holds until the next keystroke. */
function PromptBox({ value, onInput }: { value: string; onInput: (value: string) => void }) {
  const box = useRef<HTMLTextAreaElement>(null);
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    // Shrunk first: `scrollHeight` is not less than the box's own height.
    el.style.height = 'auto';
    el.style.height = `${el.scrollHeight}px`;
  }, [value]);
  return <textarea ref={box} class="input room-prompt" name="system_prompt" rows={8} value={value} onInput={(e) => onInput((e.currentTarget as HTMLTextAreaElement).value)} />;
}
