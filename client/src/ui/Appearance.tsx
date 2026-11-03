import { useEffect, useRef, useState } from 'preact/hooks';
import type { AppState } from '../app';
import type { Project, ProjectColor, Theme } from '../proto';
import { BASES, PROJECT_COLORS, STATUS, THEMES, alpha, colorHex, themeVars } from '../theme';
import { IconCross } from './icons';
import { projectInitials } from './sessions';

export function projectName(p: { name: string; is_personal: boolean }): string {
  return p.is_personal ? 'Personal' : p.name;
}

/** What a project tile shows (T49): its emoji, letters or image; without an icon the first
 *  letters of the name. */
export function ProjectGlyph({ p }: { p: Project }) {
  const i = p.icon;
  if (i?.kind === 'image') return <img class="glyph-img" src={i.url} alt="" />;
  if (i) return <span class={`glyph glyph-${i.kind}`}>{i.value}</span>;
  return <span class="glyph glyph-letters">{projectInitials(projectName(p), p.is_personal)}</span>;
}

/** The background of another project's tile: its color at ~24 %. */
export function tileBackground(p: Project): string {
  return alpha(colorHex(p.color), 0.24);
}

const EMOJIS = ['🔌', '⚡', '📡', '🛰️', '🤖', '🧪', '⚛️', '🎮', '🔧', '🧠', '🐙', '🌿', '🔥', '🧩', '🦊', '📟'];
const MODES = [
  { id: 'emoji', label: 'Emoji' },
  { id: 'letters', label: 'Letters' },
  { id: 'image', label: 'Image' },
] as const;
type Mode = (typeof MODES)[number]['id'];

const ICON_TYPES = ['image/png', 'image/jpeg', 'image/webp'];
const ICON_MAX = 256 * 1024;

/** The client's own check before the upload; the hub checks the content and the size in
 *  pixels again (`422 invalid_icon`). */
export function iconFileError(f: File): string | null {
  if (!ICON_TYPES.includes(f.type)) return 'PNG, JPEG or WebP only';
  if (f.size > ICON_MAX) return 'At most 256 KB';
  return null;
}

/** "Project appearance" (T49, `Appearance.dc.html`): icon, project color, app theme, with a
 *  live preview of the project bar and the sidebar. Nothing is saved before "Save"; a phone
 *  shows it as a full-screen sheet. */
export function AppearanceDialog({
  app,
  project,
  phone,
  onClose,
}: {
  app: AppState;
  project: Project;
  phone?: boolean;
  onClose: () => void;
}) {
  const icon = project.icon;
  const initials = projectInitials(projectName(project), project.is_personal);
  const [mode, setMode] = useState<Mode>(icon?.kind ?? 'letters');
  const [emoji, setEmoji] = useState(icon?.kind === 'emoji' ? icon.value : EMOJIS[0]);
  const [letters, setLetters] = useState(icon?.kind === 'letters' ? icon.value : initials);
  const [file, setFile] = useState<File | null>(null);
  const [fileUrl, setFileUrl] = useState<string | null>(null);
  const [title, setTitle] = useState(project.name);
  const [color, setColor] = useState<ProjectColor>(project.color);
  const [base, setBase] = useState<Theme>(app.theme());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const box = useRef<HTMLElement>(null);

  useEffect(() => {
    box.current?.querySelector<HTMLElement>('button[aria-pressed="true"]')?.focus();
    const key = (e: KeyboardEvent) => e.key === 'Escape' && onClose();
    addEventListener('keydown', key);
    return () => removeEventListener('keydown', key);
  }, []);
  useEffect(() => () => void (fileUrl && URL.revokeObjectURL(fileUrl)), [fileUrl]);

  const image = fileUrl ?? (icon?.kind === 'image' ? icon.url : null);
  const lettersValue = letters.trim().toUpperCase();
  const glyph =
    mode === 'image' ? (
      image ? (
        <img class="glyph-img" src={image} alt="" />
      ) : (
        <span class="glyph glyph-letters">{initials}</span>
      )
    ) : (
      <span class={`glyph glyph-${mode}`}>{mode === 'emoji' ? emoji : lettersValue || initials}</span>
    );

  const pick = (f: File | undefined) => {
    if (!f) return;
    const bad = iconFileError(f);
    setError(bad);
    if (bad) return;
    setFile(f);
    setFileUrl(typeof URL.createObjectURL === 'function' ? URL.createObjectURL(f) : null);
  };

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      const newName = title.trim();
      if (!project.is_personal && !newName) throw new Error('The project needs a name');
      if (mode === 'image') {
        if (file) await app.uploadProjectIcon(project.id, file);
        else if (icon?.kind !== 'image') throw new Error('Choose an image first');
      }
      const patch: Parameters<AppState['setAppearance']>[1] = {};
      if (color !== project.color) patch.color = color;
      if (mode === 'emoji' && !(icon?.kind === 'emoji' && icon.value === emoji)) patch.icon = { kind: 'emoji', value: emoji };
      if (mode === 'letters') {
        if (!lettersValue) throw new Error('Letters: 1 or 2 characters');
        if (!(icon?.kind === 'letters' && icon.value === lettersValue)) patch.icon = { kind: 'letters', value: lettersValue };
      }
      if (patch.color || patch.icon) await app.setAppearance(project.id, patch);
      if (!project.is_personal && newName !== project.name) await app.renameProject(project.id, newName);
      await app.setTheme(base);
      onClose();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const name = projectName(project);
  const others = app.sortedProjects().filter((p) => p.id !== project.id).slice(0, 2);
  return (
    <div class={`modal-scrim ${phone ? 'modal-phone' : ''}`} onClick={(e) => e.target === e.currentTarget && onClose()}>
      <section
        ref={box}
        class="appearance"
        role="dialog"
        aria-modal="true"
        aria-label="Project appearance"
        style={themeVars(colorHex(color), base)}
      >
        <header class="appearance-head">
          <div class="appearance-title">
            <div class="title">Project appearance</div>
            <div class="sub">{name} · icon and color show in the project bar, sidebar and buttons</div>
          </div>
          <button type="button" class="icon-btn" aria-label="Close" onClick={onClose}>
            <IconCross size={13} />
          </button>
        </header>
        <div class="appearance-body">
          <div class="appearance-form">
            {/* The personal project keeps its name (SPEC §3 Project). */}
            {!project.is_personal && (
              <label class="ap-section">
                <span class="ap-label">Name</span>
                <input
                  class="input"
                  name="project-name"
                  value={title}
                  onInput={(e) => setTitle((e.currentTarget as HTMLInputElement).value)}
                />
              </label>
            )}
            <div class="ap-section">
              <div class="ap-row">
                <span class="ap-label">Icon</span>
                <div class="seg-group seg-3" role="group" aria-label="Icon type">
                  {MODES.map((m) => (
                    <button
                      type="button"
                      key={m.id}
                      class={`seg ${mode === m.id ? 'is-on' : ''}`}
                      aria-pressed={mode === m.id}
                      onClick={() => setMode(m.id)}
                    >
                      {m.label}
                    </button>
                  ))}
                </div>
              </div>
              {mode === 'emoji' && (
                <div class="emoji-grid">
                  {EMOJIS.map((e) => (
                    <button
                      type="button"
                      key={e}
                      class={`emoji-pick ${emoji === e ? 'is-on' : ''}`}
                      aria-label={`Use ${e}`}
                      aria-pressed={emoji === e}
                      onClick={() => setEmoji(e)}
                    >
                      {e}
                    </button>
                  ))}
                </div>
              )}
              {mode === 'letters' && (
                <label class="letters-field">
                  Letters
                  <input
                    class="input"
                    name="letters"
                    maxLength={2}
                    aria-label="Project letters"
                    value={letters}
                    onInput={(e) => setLetters((e.currentTarget as HTMLInputElement).value)}
                  />
                  <span class="muted">1–2 characters</span>
                </label>
              )}
              {mode === 'image' && (
                <label
                  class="drop-zone"
                  onDragOver={(e) => e.preventDefault()}
                  onDrop={(e) => {
                    e.preventDefault();
                    pick(e.dataTransfer?.files[0]);
                  }}
                >
                  <input
                    type="file"
                    name="icon"
                    accept={ICON_TYPES.join(',')}
                    onChange={(e) => pick((e.currentTarget as HTMLInputElement).files?.[0])}
                  />
                  <span class="drop-main">
                    {file ? file.name : 'Drop an image or '}
                    {!file && <span class="link">choose a file</span>}
                  </span>
                  <span class="muted">PNG, JPEG or WebP · square, 128 to 1024 px, up to 256 KB</span>
                </label>
              )}
            </div>

            <div class="ap-section">
              <span class="ap-label">Project color</span>
              <div class="color-picks">
                {PROJECT_COLORS.map((c) => (
                  <button
                    type="button"
                    key={c.id}
                    class={`color-pick ${color === c.id ? 'is-on' : ''}`}
                    aria-label={c.label}
                    title={c.label}
                    aria-pressed={color === c.id}
                    style={{ background: c.hex }}
                    onClick={() => setColor(c.id)}
                  />
                ))}
              </div>
              <span class="ap-note">
                Tints the project bar and sidebar, and colors buttons and the running indicator. Waiting stays amber in
                every project.
              </span>
            </div>

            <div class="ap-section">
              <div class="ap-row-base">
                <span class="ap-label">App theme</span>
                <span class="ap-note">for all projects</span>
              </div>
              <div class="base-picks">
                {THEMES.map((t) => (
                  <button
                    type="button"
                    key={t.id}
                    class={`base-pick ${base === t.id ? 'is-on' : ''}`}
                    aria-pressed={base === t.id}
                    onClick={() => setBase(t.id)}
                  >
                    <span
                      class="base-swatch"
                      style={{
                        background: `linear-gradient(90deg, ${BASES[t.id].bg} 0 60%, ${t.id === 'contrast' ? BASES[t.id].border : BASES[t.id].surface} 60% 100%)`,
                      }}
                    />
                    <span>{t.label}</span>
                  </button>
                ))}
              </div>
            </div>
          </div>

          <div class="appearance-preview" aria-hidden="true">
            <span class="ap-caption">Preview</span>
            <div class="preview-frame">
              <div class="preview-rail">
                {others[0] && (
                  <span class="preview-tile" style={{ background: tileBackground(others[0]) }}>
                    <ProjectGlyph p={others[0]} />
                  </span>
                )}
                <span class="preview-tile is-current">
                  <span class="preview-pill" />
                  {glyph}
                </span>
                {others[1] && (
                  <span class="preview-tile" style={{ background: tileBackground(others[1]) }}>
                    <ProjectGlyph p={others[1]} />
                  </span>
                )}
              </div>
              <div class="preview-side">
                <div class="preview-head">
                  <span class="preview-name">{title.trim() || name}</span>
                  <span class="preview-new">+ New</span>
                </div>
                <PreviewRow on dot="run" title="A running session" sub="Running" />
                <PreviewRow dot="wait" title="Asks for permission" sub="Waiting for permission" />
                <PreviewRow dot="idle" title="An idle session" sub="Idle" />
              </div>
            </div>
          </div>
        </div>
        {error && (
          <div class="error-text appearance-error" role="alert">
            {error}
          </div>
        )}
        <footer class="appearance-foot">
          <button type="button" class="btn btn-ghost" onClick={onClose}>
            Cancel
          </button>
          <button type="button" class="btn btn-primary btn-save-appearance" disabled={busy} onClick={() => void save()}>
            Save
          </button>
        </footer>
      </section>
    </div>
  );
}

function PreviewRow({ on, dot, title, sub }: { on?: boolean; dot: 'run' | 'wait' | 'idle'; title: string; sub: string }) {
  return (
    <div class={`preview-row ${on ? 'is-on' : ''}`}>
      <span class={`dot dot-${dot}`} />
      <span class="preview-text">
        <span class="preview-title">{title}</span>
        <span class="preview-sub" style={dot === 'wait' ? { color: STATUS.waiting } : undefined}>
          {sub}
        </span>
      </span>
    </div>
  );
}
