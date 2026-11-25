// @vitest-environment happy-dom
// T69 (iteration 8, SPEC 0.245): Settings → Skills (the list, the upload of a folder or a
// `.zip`, the projects, Delete) and the skills a project's settings list. The chat's `/` menu
// matching `hqc:<name>` by `<name>`: chat.test.tsx.

import { afterEach, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { Api, ApiError, type SkillUpload } from '../src/api';
import type { AppState } from '../src/app';
import type { Project, Skill } from '../src/proto';
import { ProjectSkills, SkillsSection, folderUpload } from '../src/ui/Skills';
import { Settings } from '../src/ui/Settings';
import { defaultKey, type KeyAction } from '../src/keys';
import { WEB_SEARCHES } from '../src/websearch';

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
  vi.unstubAllGlobals();
});

const P1 = { id: 'p1', name: 'kiln', is_personal: false } as Project;
const P2 = { id: 'p2', name: 'hqc', is_personal: false } as Project;

const BENCH: Skill = {
  id: 'k1',
  name: 'bench',
  description: 'Runs the benchmarks.',
  allowed_tools: ['Bash(cargo bench:*)', 'Read'],
  sha256: 'a'.repeat(64),
  size: 5300,
  files: 2,
  all_projects: true,
  project_ids: [],
  updated_at: '2026-11-25T10:00:00Z',
};
const NOTES: Skill = {
  ...BENCH,
  id: 'k2',
  name: 'release-notes',
  description: undefined,
  allowed_tools: [],
  size: 900,
  files: 1,
  all_projects: false,
  project_ids: ['p1'],
};

function stubApp(api: Partial<Api>) {
  return {
    api,
    sortedProjects: () => [P1, P2],
    project: (id: string) => [P1, P2].find((p) => p.id === id),
  } as unknown as AppState;
}

async function mount(app: AppState, node = <SkillsSection app={app} />) {
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(node, root));
  await act(() => new Promise((r) => setTimeout(r, 0)));
}

const $ = <T extends Element = HTMLElement>(sel: string) => root.querySelector<T>(sel);
const $$ = <T extends Element = HTMLElement>(sel: string) => [...root.querySelectorAll<T>(sel)];
const rows = () => $$('.skill-row');
const names = () => rows().map((r) => r.querySelector('.skill-name')!.textContent);

async function click(el: Element | null) {
  await act(async () => (el as HTMLElement).click());
  await act(() => new Promise((r) => setTimeout(r, 0)));
}

/** A file picked in a folder: `rel` is its `webkitRelativePath`. */
function fileAt(rel: string, text = 'x'): File {
  const f = new File([text], rel.split('/').pop()!);
  Object.defineProperty(f, 'webkitRelativePath', { value: rel });
  return f;
}

/** Picks `files` in the file input `name`, as the browser's dialog does. */
async function pick(name: string, files: File[]) {
  const input = $<HTMLInputElement>(`input[name="${name}"]`)!;
  Object.defineProperty(input, 'files', { value: files, configurable: true });
  await act(async () => void input.dispatchEvent(new Event('change', { bubbles: true })));
  await act(() => new Promise((r) => setTimeout(r, 0)));
}

it('lists the skills with their size, files, allowed tools and projects', async () => {
  await mount(stubApp({ skills: vi.fn(async () => [BENCH, NOTES]) }));
  expect($('h2')!.textContent).toBe('Skills');
  expect($('p.muted')!.textContent).toContain('hub_skills = true');
  expect(names()).toEqual(['bench', 'release-notes']);
  const [b, n] = rows();
  expect(b.querySelector('.skill-description')!.textContent).toBe('Runs the benchmarks.');
  expect(b.querySelector('.skill-size')!.textContent).toBe('5.2 KB · 2 files');
  expect(b.querySelector('.skill-allowed')!.textContent).toBe('Runs without asking: Bash(cargo bench:*), Read');
  expect(b.querySelector('.skill-projects')!.textContent).toBe('All projects');
  expect(n.querySelector('.skill-description')).toBeNull();
  expect(n.querySelector('.skill-allowed')).toBeNull();
  expect(n.querySelector('.skill-size')!.textContent).toBe('900 B · 1 file');
  expect(n.querySelector('.skill-projects')!.textContent).toBe('kiln');
  // The two ways to upload.
  expect($<HTMLInputElement>('input[name="skill_folder"]')!.hasAttribute('webkitdirectory')).toBe(true);
  expect($<HTMLInputElement>('input[name="skill_zip"]')!.accept).toBe('.zip,application/zip');
});

it('is a section of Settings, after MCP servers', async () => {
  const app = {
    ...stubApp({ skills: vi.fn(async () => [BENCH]), mcpServers: vi.fn(async () => []), workers: vi.fn(async () => []) }),
    me: { email: 'me@example.com' },
    theme: () => 'warm',
    lookName: () => 'spacious',
    newSessionProject: () => undefined,
    webSearch: WEB_SEARCHES[0],
    providers: [],
    deviceUser: () => undefined,
    forgetDevice: async () => {},
    keyFor: (a: KeyAction) => defaultKey(a),
    setKey: () => {},
  } as unknown as AppState;
  await mount(app, <Settings app={app} />);
  const heads = $$('h2').map((h) => h.textContent);
  expect(heads.indexOf('Skills')).toBe(heads.indexOf('MCP servers') + 1);
  expect(names()).toEqual(['bench']);
});

it('a folder goes as its files, paths without the folder, the junk left out', async () => {
  const uploadSkill = vi.fn(async (_: SkillUpload) => ({ skill: { ...BENCH, id: 'k3', name: 'deploy' }, replaced: false }));
  await mount(stubApp({ skills: vi.fn(async () => [BENCH]), uploadSkill }));
  await pick('skill_folder', [
    fileAt('deploy/SKILL.md'),
    fileAt('deploy/.DS_Store'),
    fileAt('deploy/scripts/run.sh'),
    fileAt('deploy/scripts/.DS_Store'),
    fileAt('deploy/.git/HEAD'),
    fileAt('deploy/__MACOSX/._SKILL.md'),
  ]);
  expect(uploadSkill).toHaveBeenCalledTimes(1);
  const u = uploadSkill.mock.calls[0][0] as Extract<SkillUpload, { files: unknown }>;
  expect(u.name).toBe('deploy');
  expect(u.files.map((f) => f.path)).toEqual(['SKILL.md', 'scripts/run.sh']);
  expect($('.skill-status')!.textContent).toBe('Added deploy');
  expect(names()).toEqual(['bench', 'deploy']);
});

it('a .zip goes as one part, named after the file; a skill of that name is replaced', async () => {
  const uploadSkill = vi.fn(async (_: SkillUpload) => ({ skill: { ...BENCH, size: 7000 }, replaced: true }));
  await mount(stubApp({ skills: vi.fn(async () => [BENCH, NOTES]), uploadSkill }));
  const zip = new File(['PK'], 'bench.ZIP', { type: 'application/zip' });
  await pick('skill_zip', [zip]);
  const u = uploadSkill.mock.calls[0][0] as Extract<SkillUpload, { zip: unknown }>;
  expect(u.zip).toBe(zip);
  expect(u.name).toBe('bench');
  expect($('.skill-status')!.textContent).toBe('Replaced bench');
  expect(names()).toEqual(['bench', 'release-notes']);
  expect(rows()[0].querySelector('.skill-size')!.textContent).toBe('6.8 KB · 2 files');
});

it("shows the hub's reason when an upload is refused", async () => {
  const uploadSkill = vi.fn(async () => {
    throw new ApiError(422, 'SKILL.md has hooks, which would run outside the deny', 'skill_hooks');
  });
  await mount(stubApp({ skills: vi.fn(async () => []), uploadSkill }));
  await pick('skill_folder', [fileAt('x/SKILL.md')]);
  expect($('.skill-error')!.textContent).toBe('SKILL.md has hooks, which would run outside the deny');
  expect($('.skill-status')).toBeNull();
  expect(rows()).toHaveLength(0);
});

it('the Api sends a folder as `file` parts and a zip as one `zip` part, 200 is a replacement', async () => {
  const sent: FormData[] = [];
  let status = 201;
  const fetchImpl = vi.fn(async (_url: string, init?: RequestInit) => {
    sent.push(init!.body as FormData);
    return new Response(JSON.stringify(BENCH), { status });
  });
  const api = new Api('', fetchImpl as unknown as typeof fetch);
  const r = await api.uploadSkill({ name: 'bench', files: [{ file: new File(['a'], 'run.sh'), path: 'scripts/run.sh' }, { file: new File(['b'], 'SKILL.md'), path: 'SKILL.md' }] });
  expect(r.replaced).toBe(false);
  expect(fetchImpl.mock.calls[0][0]).toBe('/api/skills');
  expect(fetchImpl.mock.calls[0][1]!.method).toBe('POST');
  expect((sent[0].getAll('file') as File[]).map((f) => f.name)).toEqual(['scripts/run.sh', 'SKILL.md']);
  expect(sent[0].get('name')).toBe('bench');
  expect(sent[0].has('zip')).toBe(false);

  status = 200;
  const z = await api.uploadSkill({ name: 'bench', zip: new Blob(['PK']) });
  expect(z.replaced).toBe(true);
  expect(sent[1].getAll('zip')).toHaveLength(1);
  expect(sent[1].has('file')).toBe(false);
});

it('folderUpload takes the name from the folder and keeps nested paths', () => {
  const u = folderUpload([fileAt('my-skill/a/b/c.md'), fileAt('my-skill/SKILL.md')]) as Extract<SkillUpload, { files: unknown }>;
  expect(u.name).toBe('my-skill');
  expect(u.files.map((f) => f.path)).toEqual(['a/b/c.md', 'SKILL.md']);
});

it('the project picker sends the chosen projects', async () => {
  const updateSkill = vi.fn(async (_: string, f: { all_projects?: boolean; project_ids?: string[] }) => ({ ...BENCH, ...f }) as Skill);
  await mount(stubApp({ skills: vi.fn(async () => [BENCH]), updateSkill }));
  await click($('.btn-skill-projects'));
  const all = $<HTMLInputElement>('input[name="skill_all_projects"]')!;
  expect(all.checked).toBe(true);
  await click(all);
  await click($$<HTMLInputElement>('input[name="skill_project"]').find((i) => i.value === 'p2')!);
  await click($('.btn-save-skill'));
  expect(updateSkill).toHaveBeenCalledWith('k1', { all_projects: false, project_ids: ['p2'] });
  expect($('.skill-projects-form')).toBeNull();
  expect(rows()[0].querySelector('.skill-projects')!.textContent).toBe('hqc');

  // Back to all projects.
  await click($('.btn-skill-projects'));
  await click($('input[name="skill_all_projects"]'));
  await click($('.btn-save-skill'));
  expect(updateSkill).toHaveBeenLastCalledWith('k1', { all_projects: true, project_ids: [] });
  expect(rows()[0].querySelector('.skill-projects')!.textContent).toBe('All projects');
});

it('deletes a skill after a confirmation', async () => {
  const deleteSkill = vi.fn(async () => {});
  await mount(stubApp({ skills: vi.fn(async () => [BENCH, NOTES]), deleteSkill }));
  vi.stubGlobal('confirm', () => false);
  await click(rows()[1].querySelector('.btn-delete-skill'));
  expect(deleteSkill).not.toHaveBeenCalled();
  const confirm = vi.fn(() => true);
  vi.stubGlobal('confirm', confirm);
  await click(rows()[1].querySelector('.btn-delete-skill'));
  expect(confirm.mock.calls[0]).toEqual([expect.stringContaining('“release-notes”')]);
  expect(deleteSkill).toHaveBeenCalledWith('k2');
  expect(names()).toEqual(['bench']);
});

it("a project's settings list the skills it gets", async () => {
  const app = stubApp({ skills: vi.fn(async () => [BENCH, NOTES]) });
  await mount(app, <ProjectSkills app={app} projectId="p1" />);
  expect($('h2')!.textContent).toBe('Skills');
  expect(names()).toEqual(['bench', 'release-notes']);
  expect(rows()[0].textContent).toContain('all projects');
  expect($('a[href="#/settings"]')!.textContent).toBe('Settings → Skills');
  expect($$('button')).toHaveLength(0);
  render(null, root);

  await mount(app, <ProjectSkills app={app} projectId="p2" />);
  expect(names()).toEqual(['bench']);
  render(null, root);

  const other = stubApp({ skills: vi.fn(async () => [NOTES]) });
  await mount(other, <ProjectSkills app={other} projectId="p2" />);
  expect($('.empty-inline')!.textContent).toBe('None for this project.');
  render(null, root);

  // No skills at all: no section.
  const none = stubApp({ skills: vi.fn(async () => []) });
  await mount(none, <ProjectSkills app={none} projectId="p2" />);
  expect($('section')).toBeNull();
});
