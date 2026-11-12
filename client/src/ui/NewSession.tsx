// A new session (SPEC §6): the "+" of the session column opens it at once as a chat window
// (`#/p/<project>/new`), with nothing on the hub yet. Chips over the input choose the
// workspace, the folder inside it, the permission mode and the model, the defaults already
// picked; the first message creates the session and the window becomes its chat. Left unsent,
// the draft is gone.

import type { ComponentChildren } from 'preact';
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'preact/hooks';
import type { AppState, NewSessionDraft } from '../app';
import { onBack, replaceHash } from '../nav';
import type { PermissionMode, Workspace } from '../proto';
import type { OutgoingFile } from '../session';
import { Timeline, type Item } from '../timeline';
import { MODEL_ALIASES } from './agentSettings';
import { ChatItems, type ChatActions } from './cards';
import { Composer, SessionsButton, toOutgoing, type SessionsToggle } from './Chat';
import { ContextMenu, type MenuEntry } from './ContextMenu';
import { useSubscription } from './hooks';
import { IconBack, IconChevronUp, IconFolder, IconModel, IconPlus, IconShield, IconWorkspace } from './icons';
import { AUTO_CONFIRM, homePath, modelName } from './labels';
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

/** The window of a new session: the header says "Draft" and where it will run, the feed what
 *  to do, the chips over the input what it starts with. The first message creates it with the
 *  chips' choices (`AppState.startNewSession`) and goes in as its first input; until then the
 *  input sends nothing more ("Starting…"), and what is typed and attached meanwhile stays.
 *  Once the chat's first page is in, the address becomes the session's and its chat replaces
 *  this window: the message in its feed, the text and files not sent in its input, the caret
 *  there. A create the hub refuses keeps the draft, the text back in the input and the files
 *  with it. Files dropped on the window are attached, as on a chat. `sessions`: a window
 *  900–1079 px wide, "Sessions" at the left of the header. */
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
  const [notice, setNotice] = useState<string | null>(null);
  const list = app.workspaces.get(projectId);
  useEffect(() => {
    app.loadWorkspaces(projectId).catch((e: Error) => setNotice(`Workspaces not loaded: ${e.message}`));
  }, [app, projectId]);
  const { worker, path } = newSessionWhere(app, draft);

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
  // The first message, while the session is being created.
  const [first, setFirst] = useState<Outgoing | null>(null);
  // The input is made again, with the text of the message, when the session was not created.
  const [input, setInput] = useState({ text: '', n: 0 });
  const typed = useRef('');
  const left = useRef(false);
  useEffect(
    () => () => {
      left.current = true;
    },
    [],
  );

  const start = async (m: Outgoing) => {
    try {
      const s = await app.startNewSession();
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
        app.setListNotice(`Not started: ${reason} — your message was not sent`);
        return;
      }
      const text = [m.text, typed.current].filter((t) => t.trim()).join('\n\n');
      typed.current = text;
      setFirst(null);
      setFiles((cur) => [...m.files, ...cur]);
      setInput((cur) => ({ text, n: cur.n + 1 }));
      setNotice(`Not started: ${reason}`);
    }
  };
  // The first message starts the session; the input sends nothing more until the chat is here.
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
          <h1 class="chat-title">New session</h1>
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
                      <span class="mono">{homePath(path)}</span>
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
                <div class="new-empty-title">The project has no workspaces: a session needs a folder on a worker.</div>
                <a href={`#/p/${projectId}/settings`}>Add a workspace</a>
              </>
            ) : (
              <>
                <div class="new-empty-title">Write the first message — the session starts with it</div>
                <div>Workspace, folder, permissions and model are above the box; the defaults are already picked.</div>
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
      <NewSessionChips app={app} draft={draft} list={list} phone={phone} disabled={!!first} />
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

type Chip = 'workspace' | 'folder' | 'permissions' | 'model';

const PERMISSIONS: [PermissionMode | undefined, string][] = [
  [undefined, "The worker's default"],
  ['ask', 'Ask'],
  ['auto', 'Auto'],
];

/** The chips over a new session's input: the workspace (each of the project's with its worker
 *  and whole path, the default marked, "Add a workspace…" under them), the folder inside it
 *  (typed), the permissions (the worker's default, Ask, Auto) and the model (Claude Code's
 *  default, an alias, another name typed). Each opens its menu upwards; a pick gives the caret
 *  back to the input. */
function NewSessionChips({
  app,
  draft,
  list,
  phone,
  disabled,
}: {
  app: AppState;
  draft: NewSessionDraft;
  list?: Workspace[];
  phone: boolean;
  /** The first message is on its way: the session takes what was chosen then. */
  disabled: boolean;
}) {
  const [open, setOpen] = useState<{ chip: Chip; x: number; y: number; anchor: HTMLElement; custom?: boolean } | null>(null);
  const project = app.project(draft.projectId);
  const id = app.newSessionWorkspace(draft);
  const workspace = list?.find((w) => w.id === id);
  const worker = workspace && app.workerName(workspace.worker_id);
  const custom = draft.model !== undefined && !(MODEL_ALIASES as readonly string[]).includes(draft.model);
  const close = () => setOpen(null);
  // Enter in a menu's field closes it: the field has set its value already.
  const enterCloses = (e: KeyboardEvent) => {
    if (e.key !== 'Enter' || e.isComposing) return;
    e.preventDefault();
    close();
  };

  const chip = (which: Chip, icon: ComponentChildren, face: ComponentChildren, label?: string) => (
    <button
      type="button"
      class={`new-chip btn-chip-${which} ${open?.chip === which ? 'is-open' : ''}`}
      aria-label={label}
      title={label}
      aria-haspopup="menu"
      aria-expanded={open?.chip === which}
      disabled={disabled}
      // A mouse press leaves the caret in the input: the menu gives the focus back there.
      onMouseDown={(e) => e.preventDefault()}
      onClick={(e) => {
        if (open?.chip === which) return close();
        const el = e.currentTarget as HTMLElement;
        const r = el.getBoundingClientRect();
        setOpen({ chip: which, x: r.left, y: r.top - 6, anchor: el });
      }}
    >
      {icon}
      {face}
    </button>
  );

  let menu: ComponentChildren = null;
  if (open) {
    const at = { x: open.x, y: open.y, above: true, anchor: open.anchor, backdrop: phone, onClose: close };
    if (open.chip === 'workspace') {
      const items: MenuEntry[] = (list ?? []).map((w) => ({
        label: `${workspaceName(w)}${w.id === project?.default_workspace_id ? ' (default)' : ''}`,
        detail: [app.workerName(w.worker_id), homePath(w.path)].filter(Boolean).join(':'),
        cls: 'btn-pick-workspace',
        checked: w.id === id,
        onSelect: () => app.setNewSession({ workspace_id: w.id }),
      }));
      if (items.length) items.push('sep');
      items.push({ label: 'Add a workspace…', icon: <IconPlus />, cls: 'btn-add-workspace', href: `#/p/${draft.projectId}/settings` });
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
              value={draft.cwd}
              onInput={(e) => app.setNewSession({ cwd: (e.currentTarget as HTMLInputElement).value })}
              onKeyDown={enterCloses}
            />
          </label>
        </ContextMenu>
      );
    } else if (open.chip === 'permissions') {
      const items: MenuEntry[] = PERMISSIONS.map(([mode, label]) => ({
        label,
        cls: `btn-permissions-${mode ?? 'default'}`,
        checked: draft.permission_mode === mode,
        onSelect: () => {
          // Auto after a one-line confirmation; Ask and the default at once.
          if (mode === 'auto' && draft.permission_mode !== 'auto' && !confirm(AUTO_CONFIRM)) return;
          app.setNewSession({ permission_mode: mode });
        },
      }));
      menu = <ContextMenu {...at} label="Permissions" items={items} />;
    } else if (open.custom) {
      menu = (
        <ContextMenu {...at} label="Model">
          <ModelName value={custom ? draft.model! : ''} onChange={(model) => app.setNewSession({ model })} onKeyDown={enterCloses} />
        </ContextMenu>
      );
    } else {
      const items: MenuEntry[] = [
        { label: "Claude Code's default", cls: 'btn-model-default', checked: draft.model === undefined, onSelect: () => app.setNewSession({ model: undefined }) },
        ...MODEL_ALIASES.map((m) => ({
          label: modelName(m),
          cls: `btn-model-${m}`,
          checked: draft.model === m,
          onSelect: () => app.setNewSession({ model: m }),
        })),
        // Another model waits for its name, in the menu.
        { label: 'Another model…', cls: 'btn-model-custom', checked: custom, keepOpen: true, onSelect: () => setOpen({ ...open, custom: true }) },
      ];
      menu = <ContextMenu {...at} label="Model" items={items} />;
    }
  }

  const mode = PERMISSIONS.find(([m]) => m === draft.permission_mode)?.[1];
  return (
    <div class="new-chips">
      <div class="new-chips-row">
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
            Folder <b>{draft.cwd.trim() || '.'}</b>
          </span>,
        )}
        {chip(
          'permissions',
          <IconShield size={14} />,
          <span class="new-chip-text">
            Permissions: <b>{draft.permission_mode ? mode : "worker's default"}</b>
          </span>,
        )}
        {chip(
          'model',
          <IconModel />,
          <span class="new-chip-text">
            Model: <b>{draft.model ? modelName(draft.model) : 'default'}</b>
          </span>,
        )}
      </div>
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
