// "Skills" of Settings (SPEC 0.237, iteration 8 T69): the owner's skills, which the agents of
// the projects they are given to get as `hqc:<name>` on workers with `hub_skills = true`. A
// skill is uploaded as a folder or a `.zip`; one with a name the owner has replaces it.

import { useEffect, useRef, useState } from 'preact/hooks';
import { ApiError, type SkillUpload } from '../api';
import type { AppState } from '../app';
import { formatSize } from '../artifacts';
import type { Skill } from '../proto';
import { projectName } from './Appearance';

/** Paths of a picked folder the hub drops anyway: not sent. */
const JUNK = new Set(['.DS_Store', '__MACOSX', '.git']);

/** A picked folder's files as the hub takes them: each path without the folder's own name,
 *  the junk left out; `name`: the folder's name. */
export function folderUpload(files: File[]): SkillUpload {
  let name = '';
  const out: { file: Blob; path: string }[] = [];
  for (const f of files) {
    const parts = (f.webkitRelativePath || f.name).split('/');
    if (parts.length > 1) name ||= parts[0];
    const path = parts.length > 1 ? parts.slice(1) : parts;
    if (path.some((p) => JUNK.has(p))) continue;
    out.push({ file: f, path: path.join('/') });
  }
  return { files: out, name };
}

/** The skills project `projectId` gets. */
export function skillsOf(list: Skill[], projectId: string): Skill[] {
  return list.filter((s) => s.all_projects || s.project_ids.includes(projectId));
}

/** The skills from the hub; a stub without the call: none. */
function loadSkills(app: AppState): Promise<Skill[]> {
  return Promise.resolve().then(() => app.api.skills());
}

function nameOfProject(app: AppState, id: string): string {
  const p = app.sortedProjects().find((x) => x.id === id) ?? app.project?.(id);
  return p ? projectName(p) : 'a deleted project';
}

function SkillInfo({ s }: { s: Skill }) {
  return (
    <>
      {s.description && <span class="mcp-meta skill-description">{s.description}</span>}
      <span class="mcp-meta skill-size">
        {formatSize(s.size)} · {s.files} {s.files === 1 ? 'file' : 'files'}
      </span>
      {s.allowed_tools.length > 0 && (
        <span class="mcp-tools skill-allowed">
          Runs without asking: <span class="mono">{s.allowed_tools.join(', ')}</span>
        </span>
      )}
    </>
  );
}

export function SkillsSection({ app }: { app: AppState }) {
  const [skills, setSkills] = useState<Skill[] | null>(null);
  // The skill whose projects are being chosen.
  const [picking, setPicking] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const folder = useRef<HTMLInputElement>(null);
  const zip = useRef<HTMLInputElement>(null);

  useEffect(() => {
    loadSkills(app).then(setSkills, (e) => {
      setSkills([]);
      if (e instanceof ApiError) setError(e.message);
    });
  }, [app]);

  const put = (s: Skill) =>
    setSkills((cur) => [...(cur ?? []).filter((x) => x.id !== s.id), s].sort((a, b) => a.name.localeCompare(b.name)));

  const upload = async (u: SkillUpload) => {
    setError(null);
    setDone(null);
    setBusy(true);
    try {
      const { skill, replaced } = await app.api.uploadSkill(u);
      put(skill);
      setDone(`${replaced ? 'Replaced' : 'Added'} ${skill.name}`);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const picked = (input: HTMLInputElement, asZip: boolean) => {
    const files = [...(input.files ?? [])];
    input.value = '';
    if (!files.length) return;
    if (asZip) void upload({ zip: files[0], name: files[0].name.replace(/\.zip$/i, '') });
    else void upload(folderUpload(files));
  };

  const remove = async (s: Skill) => {
    if (!confirm(`Delete the skill “${s.name}”? Agents get it no more from their next start.`)) return;
    setError(null);
    setDone(null);
    try {
      await app.api.deleteSkill(s.id);
      setSkills((cur) => (cur ?? []).filter((x) => x.id !== s.id));
    } catch (e) {
      setError((e as Error).message);
    }
  };

  const saveProjects = async (s: Skill, all: boolean, ids: string[]) => {
    setError(null);
    try {
      put(await app.api.updateSkill(s.id, { all_projects: all, project_ids: all ? [] : ids }));
      setPicking(null);
    } catch (e) {
      setError((e as Error).message);
    }
  };

  return (
    <>
      <h2>Skills</h2>
      <p class="muted">
        Skills the agents of your projects get on every worker whose environment has <code>hub_skills = true</code>; a change
        reaches an agent at its next start.
      </p>
      {skills && skills.length > 0 && (
        <ul class="plain-list mcp-list skill-list">
          {skills.map((s) => (
            <li key={s.id} class="mcp-row skill-row">
              <div class="mcp-head">
                <span class="mcp-name mono skill-name">{s.name}</span>
              </div>
              <SkillInfo s={s} />
              {picking === s.id ? (
                <ProjectPicker app={app} skill={s} onSave={(all, ids) => saveProjects(s, all, ids)} onCancel={() => setPicking(null)} />
              ) : (
                <>
                  <span class="mcp-meta skill-projects">
                    {s.all_projects
                      ? 'All projects'
                      : s.project_ids.length
                        ? s.project_ids.map((id) => nameOfProject(app, id)).join(', ')
                        : 'No project yet'}
                  </span>
                  <div class="row mcp-buttons">
                    <button type="button" class="btn btn-small btn-skill-projects" onClick={() => setPicking(s.id)}>
                      Projects
                    </button>
                    <button type="button" class="btn btn-small btn-revoke btn-delete-skill" onClick={() => remove(s)}>
                      Delete
                    </button>
                  </div>
                </>
              )}
            </li>
          ))}
        </ul>
      )}
      <div class="row mcp-buttons">
        <button type="button" class="btn btn-upload-skill-folder" disabled={busy} onClick={() => folder.current?.click()}>
          Upload folder
        </button>
        <button type="button" class="btn btn-upload-skill-zip" disabled={busy} onClick={() => zip.current?.click()}>
          Upload .zip
        </button>
      </div>
      <input
        ref={folder}
        type="file"
        name="skill_folder"
        hidden
        multiple
        {...{ webkitdirectory: '' }}
        onChange={(e) => picked(e.currentTarget as HTMLInputElement, false)}
      />
      <input
        ref={zip}
        type="file"
        name="skill_zip"
        hidden
        accept=".zip,application/zip"
        onChange={(e) => picked(e.currentTarget as HTMLInputElement, true)}
      />
      {busy && <div class="muted skill-status">Uploading…</div>}
      {done && (
        <div class="muted skill-status" role="status">
          {done}
        </div>
      )}
      {error && <div class="error-text skill-error">{error}</div>}
    </>
  );
}

function ProjectPicker({
  app,
  skill,
  onSave,
  onCancel,
}: {
  app: AppState;
  skill: Skill;
  onSave: (all: boolean, ids: string[]) => void;
  onCancel: () => void;
}) {
  const [all, setAll] = useState(skill.all_projects);
  const [ids, setIds] = useState<string[]>(skill.project_ids);
  return (
    <form
      class="mcp-form skill-projects-form"
      onSubmit={(e) => {
        e.preventDefault();
        onSave(all, ids);
      }}
    >
      <fieldset class="mcp-projects">
        <legend class="field-label">Projects</legend>
        <label class="mcp-all">
          <input type="checkbox" name="skill_all_projects" checked={all} onChange={(e) => setAll((e.currentTarget as HTMLInputElement).checked)} />
          <span>All projects</span>
        </label>
        {!all &&
          app.sortedProjects().map((p) => (
            <label key={p.id} class="mcp-project">
              <input
                type="checkbox"
                name="skill_project"
                value={p.id}
                checked={ids.includes(p.id)}
                onChange={(e) => {
                  const on = (e.currentTarget as HTMLInputElement).checked;
                  setIds((cur) => (on ? [...cur.filter((x) => x !== p.id), p.id] : cur.filter((x) => x !== p.id)));
                }}
              />
              <span>{projectName(p)}</span>
            </label>
          ))}
      </fieldset>
      <div class="actions">
        <button type="submit" class="btn btn-primary btn-small btn-save-skill">
          Save
        </button>
        <button type="button" class="btn btn-ghost btn-small btn-cancel-skill" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}

/** A project's settings: the skills its agents get, read-only (they are uploaded and given to
 *  projects in Settings → Skills). Nothing at all when the owner has none. */
export function ProjectSkills({ app, projectId }: { app: AppState; projectId: string }) {
  const [skills, setSkills] = useState<Skill[] | null>(null);
  useEffect(() => {
    loadSkills(app).then(setSkills, () => setSkills(null));
  }, [app]);
  if (!skills || skills.length === 0) return null;
  const mine = skillsOf(skills, projectId);
  return (
    <section class="ps-section skill-project-skills" aria-labelledby="ps-skills">
      <h2 id="ps-skills">Skills</h2>
      <p class="ps-text">
        Skills this project’s agents get, as hqc:&lt;name&gt;. They are uploaded and given to projects in{' '}
        <a href="#/settings">Settings → Skills</a>.
      </p>
      {mine.length === 0 ? (
        <p class="empty-inline">None for this project.</p>
      ) : (
        mine.map((s) => (
          <div key={s.id} class="mcp-row ps-card skill-row">
            <div class="mcp-head">
              <span class="mcp-name mono skill-name">{s.name}</span>
              {s.all_projects && <span class="muted small">all projects</span>}
            </div>
            <SkillInfo s={s} />
          </div>
        ))
      )}
    </section>
  );
}
