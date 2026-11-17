// A new session (SPEC §6): the "+" of the session column opens it at once as a chat window
// (`#/p/<project>/new`), with nothing on the hub yet. Chips over the input choose the
// workspace, the folder inside it, the permission mode and the model, the defaults already
// picked; the first message creates the session and the window becomes its chat. Left unsent,
// the draft is gone.

import type { ComponentChildren } from 'preact';
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import type { AppState, NewSessionDraft } from '../app';
import { onBack, replaceHash } from '../nav';
import { ROOM_MODES, type PermissionMode, type RoomMode, type SessionInfo } from '../proto';
import type { OutgoingFile } from '../session';
import { Timeline, type Item } from '../timeline';
import { MODEL_ALIASES } from './agentSettings';
import { ChatItems, type ChatActions } from './cards';
import { Composer, SessionsButton, toOutgoing, type SessionsToggle } from './Chat';
import { ContextMenu, type MenuEntry } from './ContextMenu';
import { useSubscription } from './hooks';
import { IconBack, IconChevronUp, IconFolder, IconModel, IconPlus, IconRoom, IconShield, IconWorkspace } from './icons';
import { ROOM_MODE_LABELS, ROOM_MODE_NOTES, homePath, modeConfirm, modelName } from './labels';
import { workspaceName } from './ProjectSettings';

/** The first message: shown as being sent until the session exists. */
interface Outgoing {
  text: string;
  files: OutgoingFile[];
}

/** The draft's worker and folder, as its header and its row in the list name them. */
export function newSessionWhere(app: AppState, d: NewSessionDraft): { worker?: string; path?: string } {
  const id = app.newSessionWorkspace(d);
  const w = id ? app.workspaces.get(d.projectId)?.find((x) => x.id === id) : undefined;
  if (!w) return {};
  return { worker: app.workerName(w.worker_id), path: app.sessionPath({ project_id: d.projectId, workspace_id: w.id, cwd: d.cwd.trim() }) };
}

/** The window of a new session: the chips are `NewSessionChips` over the draft the app keeps
 *  (`AppState.newSession`), and the first message creates the session
 *  (`AppState.startNewSession`). `sessions`: a window 900–1079 px wide, "Sessions" at the left
 *  of the header. */
export function NewSessionChat({
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
  // The draft lives while its window does: one at a time, gone when the window is left.
  useLayoutEffect(() => {
    app.openNewSession(projectId);
    return () => app.closeNewSession();
  }, [app, projectId]);
  const draft: NewSessionDraft = app.newSession?.projectId === projectId ? app.newSession : { projectId, cwd: '' };
  return (
    <NewChatWindow
      app={app}
      projectId={projectId}
      phone={phone}
      sessions={sessions}
      kind="session"
      where={newSessionWhere(app, draft)}
      hint="Workspace, folder, permissions and model are above the box; the defaults are already picked."
      create={() => app.startNewSession()}
      chips={(disabled) => (
        <NewSessionChips app={app} projectId={projectId} values={draft} onChange={(patch) => app.setNewSession(patch)} withModel disabled={disabled} phone={phone} />
      )}
    />
  );
}

/** The window of a new session or room: the header says "Draft" and where it will run, the
 *  feed what to do, the `chips` over the input what it starts with. The first message makes it
 *  with `create` and goes in as its first input; until then the input sends nothing more
 *  ("Starting…"), and what is typed and attached meanwhile stays. Once the chat's first page is
 *  in, the address becomes its own and its chat replaces this window: the message in its feed,
 *  the text and files not sent in its input, the caret there. A create the hub refuses keeps
 *  everything, the text back in the input and the files with it. Files dropped on the window
 *  are attached, as on a chat. */
export function NewChatWindow({
  app,
  projectId,
  phone,
  sessions,
  kind,
  where: { worker, path },
  hint,
  create,
  chips,
}: {
  app: AppState;
  projectId: string;
  phone: boolean;
  sessions?: SessionsToggle;
  kind: 'session' | 'room';
  /** The worker and the folder it will run in, for the header. */
  where: { worker?: string; path?: string };
  /** Under the feed's "Write the first message", what the chips hold. */
  hint: string;
  create: () => Promise<SessionInfo>;
  /** The chips over the input; `disabled` once the first message is on its way. */
  chips: (disabled: boolean) => ComponentChildren;
}) {
  const [notice, setNotice] = useState<string | null>(null);
  const list = app.workspaces.get(projectId);
  useEffect(() => {
    app.loadWorkspaces(projectId).catch((e: Error) => setNotice(`Workspaces not loaded: ${e.message}`));
  }, [app, projectId]);

  const [files, setFiles] = useState<OutgoingFile[]>([]);
  // As the create ends: the files attached while the session was being created.
  const filesNow = useRef(files);
  filesNow.current = files;
  // Files picked and not read yet: a message sent meanwhile would go without them.
  const [reading, setReading] = useState(0);
  // Their reading: the create waits for it before it hands the files over to the chat.
  const reads = useRef<Promise<unknown>>(Promise.resolve());
  const addFiles = (list: FileList | File[]) => {
    // A copy now: the input empties the list it handed over right after.
    const picked = [...list];
    if (!picked.length) return;
    setReading((n) => n + picked.length);
    // One at a time: each is read into memory, and a HEIC decoded into a JPEG; each shows
    // as soon as it is read.
    const done = (async () => {
      for (const p of picked) {
        const f = await toOutgoing(p);
        // At once, not at the next render: the create may hand the files over right after.
        filesNow.current = [...filesNow.current, f];
        setFiles((cur) => [...cur, f]);
        setReading((n) => n - 1);
      }
    })();
    reads.current = Promise.all([reads.current, done.catch(() => {})]);
  };
  const [dragging, setDragging] = useState(false);
  // The first message, while it is being created.
  const [first, setFirst] = useState<Outgoing | null>(null);
  // The input is made again, with the text of the message, when it was not created.
  const [input, setInput] = useState({ text: '', n: 0 });
  const typed = useRef('');
  const left = useRef(false);
  useEffect(
    () => () => {
      left.current = true;
    },
    [],
  );

  const done = kind === 'room' ? 'created' : 'started';
  const start = async (m: Outgoing) => {
    try {
      const s = await create();
      const chat = app.chat(s.id);
      chat.send(m.text, m.files);
      // The chat opens with its first page (the line of its start) and the message, as this
      // window shows them: no "Loading…" in between. A failed load is tried again there.
      await chat.load().catch(() => {});
      // A file attached meanwhile and still being read goes to the chat too: a HEIC libheif
      // decodes takes a second or more, and one attached during that wait is waited for as well.
      for (let r = reads.current; ; r = reads.current) {
        await r.catch(() => {});
        if (r === reads.current) break;
      }
      // Left meanwhile: the session is in the list, what was typed is its draft.
      if (left.current) {
        app.setDraft(s.id, typed.current);
        return;
      }
      app.handOver(s.id, typed.current, filesNow.current);
      replaceHash(`#/s/${s.id}`);
    } catch (e) {
      const reason = (e as Error).message;
      // Left meanwhile: nothing keeps the message, the list says so.
      if (left.current) {
        app.setListNotice(`Not ${done}: ${reason} — your message was not sent`);
        return;
      }
      const text = [m.text, typed.current].filter((t) => t.trim()).join('\n\n');
      typed.current = text;
      setFirst(null);
      setFiles((cur) => [...m.files, ...cur]);
      setInput((cur) => ({ text, n: cur.n + 1 }));
      setNotice(`Not ${done}: ${reason}`);
    }
  };
  // The first message makes it; the input sends nothing more until the chat is here.
  const send = (text: string) => {
    const m = { text, files };
    // The input clears it: what is typed from here on stays for the chat.
    typed.current = '';
    setFiles([]);
    setNotice(null);
    setFirst(m);
    void start(m);
  };

  const shown = useMemo(() => new Timeline(), []);
  const actions: ChatActions = {
    timeline: shown,
    approve: async () => {},
    answer: async () => {},
    resend: () => {},
    discard: () => {},
    openArtifact: () => {},
  };
  const items: Item[] = first
    ? [
        {
          kind: 'pending',
          key: 'p-first',
          pending: {
            client_op_id: 'first',
            text: first.text,
            status: 'sending',
            ...(first.files.length ? { attachments: first.files.map((f) => ({ name: f.name, size: f.file.size, preview: f.preview })) } : {}),
          },
        },
      ]
    : [];
  const none = list?.length === 0;

  return (
    <section
      class={`chat chat-new ${dragging ? 'chat-dragging' : ''} ${phone ? 'chat-phone' : ''}`}
      onDragOver={(e) => {
        if (!e.dataTransfer?.types.includes('Files')) return;
        e.preventDefault();
        setDragging(true);
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target) setDragging(false);
      }}
      onDrop={(e) => {
        setDragging(false);
        if (!e.dataTransfer?.files.length) return;
        e.preventDefault();
        addFiles(e.dataTransfer.files);
      }}
    >
      <header class="chat-head">
        {phone && (
          <a
            class="icon-btn icon-btn-round back"
            href={`#/p/${projectId}`}
            aria-label="Back to sessions"
            onClick={(e) => onBack(e, `#/p/${projectId}`)}
          >
            <IconBack />
          </a>
        )}
        {sessions && <SessionsButton sessions={sessions} />}
        <div class="chat-head-main">
          <h1 class="chat-title">New {kind}</h1>
          <div class="chat-meta">
            <span class="pill">Draft</span>
            {phone ? (
              <span class="chat-where mono">{[worker, path && homePath(path).split('/').pop()].filter(Boolean).join(' · ')}</span>
            ) : (
              <>
                {worker && <span>{worker}</span>}
                {path && (
                  <>
                    {worker && <span class="sep">/</span>}
                    <span class="chat-path">
                      <span class="mono" title={homePath(path)}>
                        {homePath(path)}
                      </span>
                    </span>
                  </>
                )}
              </>
            )}
          </div>
        </div>
      </header>
      <div class="timeline">
        {first ? (
          <div class="feed">
            <ChatItems items={items} actions={actions} />
          </div>
        ) : (
          <div class="new-empty">
            {none ? (
              <>
                <div class="new-empty-title">The project has no workspaces: a {kind} needs a folder on a worker.</div>
                <a href={`#/p/${projectId}/settings`}>Add a workspace</a>
              </>
            ) : (
              <>
                <div class="new-empty-title">Write the first message — the {kind} starts with it</div>
                <div>{hint}</div>
              </>
            )}
          </div>
        )}
      </div>
      {notice && (
        <div class="notice" role="alert" onClick={() => setNotice(null)}>
          {notice}
        </div>
      )}
      <div class="new-chips">{chips(!!first)}</div>
      <Composer
        key={input.n}
        initial={input.text}
        onDraft={(t) => (typed.current = t)}
        autoFocus
        wait={first ? 'Starting…' : undefined}
        phone={phone}
        files={files}
        reading={reading}
        onFiles={addFiles}
        onRemove={(i) => setFiles((cur) => cur.filter((_, j) => j !== i))}
        onSend={send}
      />
    </section>
  );
}

type Chip = 'mode' | 'workspace' | 'folder' | 'permissions' | 'model';

/** The permission modes a new session or room starts in. */
export const PERMISSIONS: [PermissionMode | undefined, string][] = [
  [undefined, "The worker's default"],
  ['ask', 'Ask'],
  ['auto', 'Auto'],
  ['allow_all', 'Allow all'],
];

/** What the chips choose, as a new session's draft keeps it. */
export type ChipValues = Pick<NewSessionDraft, 'workspace_id' | 'cwd' | 'permission_mode' | 'model' | 'provider_id'>;

/** Where a chip's menu opens: its top left corner, and the chip that toggles it. */
export interface ChipAt {
  x: number;
  y: number;
  anchor: HTMLElement;
}

/** A chip over a new session's or room's input: a button that toggles its menu (`open`), which
 *  opens upwards from the chip. A mouse press leaves the caret in the input. */
export function ChipButton({
  cls,
  open,
  label,
  disabled,
  onOpen,
  onClose,
  children,
}: {
  cls: string;
  open: boolean;
  label?: string;
  disabled: boolean;
  onOpen: (at: ChipAt) => void;
  onClose: () => void;
  children: ComponentChildren;
}) {
  return (
    <button
      type="button"
      class={`new-chip ${cls} ${open ? 'is-open' : ''}`}
      aria-label={label}
      title={label}
      aria-haspopup="menu"
      aria-expanded={open}
      disabled={disabled}
      // A mouse press leaves the caret in the input: the menu gives the focus back there.
      onMouseDown={(e) => e.preventDefault()}
      onClick={(e) => {
        if (open) return onClose();
        const el = e.currentTarget as HTMLElement;
        const r = el.getBoundingClientRect();
        onOpen({ x: r.left, y: r.top - 6, anchor: el });
      }}
    >
      {children}
    </button>
  );
}

/** The chips over a new session's or room's input: the workspace (each of the project's with
 *  its worker and whole path, the default marked, "Add a workspace…" under them), the folder
 *  inside it (typed), the permissions (the worker's default, Ask, Auto, Allow all) and, with
 *  `withModel`, the model (Claude Code's default, an alias, another name typed); with `mode`,
 *  a room's mode first. Each opens its menu upwards; a pick gives the caret back to the input.
 *  `values` are what is chosen, `onChange` takes what a pick changes. */
export function NewSessionChips({
  app,
  projectId,
  values,
  onChange,
  withModel,
  mode: room,
  phone,
  disabled,
}: {
  app: AppState;
  projectId: string;
  values: ChipValues;
  onChange: (patch: Partial<ChipValues>) => void;
  withModel?: boolean;
  /** A room's: Conversation or Council, with what each does. */
  mode?: { value: RoomMode; onChange: (mode: RoomMode) => void };
  phone: boolean;
  /** The first message is on its way: it takes what was chosen then. */
  disabled: boolean;
}) {
  const [open, setOpen] = useState<(ChipAt & { chip: Chip; custom?: boolean }) | null>(null);
  const project = app.project(projectId);
  const list = app.workspaces.get(projectId);
  const id = app.newSessionWorkspace({ projectId, ...values });
  const workspace = list?.find((w) => w.id === id);
  const worker = workspace && app.workerName(workspace.worker_id);
  const custom = values.model !== undefined && !(MODEL_ALIASES as readonly string[]).includes(values.model);
  // A provider the draft chose that this workspace's worker does not allow is not shown.
  const chosenProvider = app.providersFor(workspace?.worker_id).find((p) => p.id === values.provider_id);
  const close = () => setOpen(null);
  // Enter in a menu's field closes it: the field has set its value already.
  const enterCloses = (e: KeyboardEvent) => {
    if (e.key !== 'Enter' || e.isComposing) return;
    e.preventDefault();
    close();
  };

  const chip = (which: Chip, icon: ComponentChildren, face: ComponentChildren, label?: string) => (
    <ChipButton cls={`btn-chip-${which}`} open={open?.chip === which} label={label} disabled={disabled} onOpen={(at) => setOpen({ ...at, chip: which })} onClose={close}>
      {icon}
      {face}
    </ChipButton>
  );

  let menu: ComponentChildren = null;
  if (open) {
    const at = { x: open.x, y: open.y, above: true, anchor: open.anchor, backdrop: phone, onClose: close };
    if (open.chip === 'mode' && room) {
      const items: MenuEntry[] = ROOM_MODES.map((m) => ({
        label: ROOM_MODE_LABELS[m],
        detail: ROOM_MODE_NOTES[m],
        cls: `btn-mode-${m}`,
        checked: room.value === m,
        onSelect: () => room.onChange(m),
      }));
      menu = <ContextMenu {...at} label="Mode" items={items} />;
    } else if (open.chip === 'workspace') {
      const items: MenuEntry[] = (list ?? []).map((w) => ({
        label: `${workspaceName(w)}${w.id === project?.default_workspace_id ? ' (default)' : ''}`,
        detail: [app.workerName(w.worker_id), homePath(w.path)].filter(Boolean).join(':'),
        cls: 'btn-pick-workspace',
        checked: w.id === id,
        onSelect: () => onChange({ workspace_id: w.id }),
      }));
      if (items.length) items.push('sep');
      items.push({ label: 'Add a workspace…', icon: <IconPlus />, cls: 'btn-add-workspace', href: `#/p/${projectId}/settings` });
      menu = <ContextMenu {...at} label="Workspace" heading="Workspace" items={items} />;
    } else if (open.chip === 'folder') {
      menu = (
        <ContextMenu {...at} label="Folder">
          <label class="menu-field">
            <span class="field-label">Folder inside the workspace</span>
            <input
              class="input mono"
              name="cwd"
              placeholder="."
              data-focus
              value={values.cwd}
              onInput={(e) => onChange({ cwd: (e.currentTarget as HTMLInputElement).value })}
              onKeyDown={enterCloses}
            />
          </label>
        </ContextMenu>
      );
    } else if (open.chip === 'permissions') {
      const items: MenuEntry[] = PERMISSIONS.map(([mode, label]) => ({
        label,
        cls: `btn-permissions-${mode ?? 'default'}`,
        checked: values.permission_mode === mode,
        onSelect: () => {
          // Auto and Allow all after a one-line confirmation of their own; Ask and the default
          // at once.
          const line = mode && mode !== values.permission_mode ? modeConfirm(mode) : undefined;
          if (line && !confirm(line)) return;
          onChange({ permission_mode: mode });
        },
      }));
      menu = <ContextMenu {...at} label="Permissions" items={items} />;
    } else if (open.custom) {
      menu = (
        <ContextMenu {...at} label="Model">
          <ModelName value={custom ? values.model! : ''} onChange={(model) => onChange({ model })} onKeyDown={enterCloses} />
        </ContextMenu>
      );
    } else if (open.chip === 'model') {
      const claude = !values.provider_id;
      const items: MenuEntry[] = [
        {
          label: "Claude Code's default",
          cls: 'btn-model-default',
          checked: claude && values.model === undefined,
          onSelect: () => onChange({ model: undefined, provider_id: undefined }),
        },
        ...MODEL_ALIASES.map((m) => ({
          label: modelName(m),
          cls: `btn-model-${m}`,
          checked: claude && values.model === m,
          onSelect: () => onChange({ model: m, provider_id: undefined }),
        })),
        // Another model waits for its name, in the menu.
        {
          label: 'Another model…',
          cls: 'btn-model-custom',
          checked: claude && custom,
          keepOpen: true,
          onSelect: () => {
            onChange({ provider_id: undefined });
            setOpen({ ...open, custom: true });
          },
        },
      ];
      // The providers the workspace's worker allows (SPEC 0.144).
      const providers = app.providersFor(workspace?.worker_id);
      if (providers.length) {
        items.push('sep');
        for (const p of providers) {
          items.push({
            label: p.label,
            detail: p.model,
            cls: 'btn-model-provider',
            checked: values.provider_id === p.id,
            onSelect: () => onChange({ provider_id: p.id }),
          });
        }
      }
      menu = <ContextMenu {...at} label="Model" items={items} />;
    }
  }

  const permission = PERMISSIONS.find(([m]) => m === values.permission_mode)?.[1];
  return (
    <div class="new-chips-row">
      {room &&
        chip(
          'mode',
          <IconRoom size={14} />,
          <span class="new-chip-text">
            Mode: <b>{ROOM_MODE_LABELS[room.value]}</b>
          </span>,
        )}
      {chip(
        'workspace',
        <IconWorkspace />,
        <>
          {workspace ? (
            <span class="new-chip-text">
              <b>{workspaceName(workspace)}</b>
              {worker && ` · ${worker}`}
            </span>
          ) : (
            <span class="new-chip-text">{list?.length === 0 ? 'No workspace' : 'Workspace'}</span>
          )}
          <IconChevronUp />
        </>,
        // The chip shows the name only: the label names what it is, with the whole path.
        workspace
          ? `Workspace: ${workspaceName(workspace)} — ${[worker, homePath(workspace.path)].filter(Boolean).join(':')}`
          : `Workspace: ${list?.length === 0 ? 'none' : 'loading'}`,
      )}
      {chip(
        'folder',
        <IconFolder />,
        <span class="new-chip-text">
          Folder <b>{values.cwd.trim() || '.'}</b>
        </span>,
      )}
      {chip(
        'permissions',
        <IconShield size={14} />,
        <span class="new-chip-text">
          Permissions: <b>{values.permission_mode ? permission : "worker's default"}</b>
        </span>,
      )}
      {withModel &&
        chip(
          'model',
          <IconModel />,
          <span class="new-chip-text">
            Model: <b>{chosenProvider ? chosenProvider.label : values.model ? modelName(values.model) : 'default'}</b>
          </span>,
        )}
      {menu}
    </div>
  );
}

/** The name of another model, typed into the model's menu: each change goes at once, an
 *  empty name is the runtime's default. */
function ModelName({ value, onChange, onKeyDown }: { value: string; onChange: (model: string | undefined) => void; onKeyDown: (e: KeyboardEvent) => void }) {
  // Its own text: a name that becomes an alias as it is typed ("opus") stays in the field.
  const [name, setName] = useState(value);
  return (
    <label class="menu-field">
      <span class="field-label">Model name</span>
      <input
        class="input mono"
        name="model_name"
        placeholder="claude-sonnet-5-5"
        data-focus
        value={name}
        onInput={(e) => {
          const v = (e.currentTarget as HTMLInputElement).value;
          setName(v);
          onChange(v.trim() || undefined);
        }}
        onKeyDown={onKeyDown}
      />
    </label>
  );
}
