// T69: the mock's skills (SPEC 0.237) through the client's Api: the multipart upload of a folder
// and of a zip, a replacement by name, refusals, the projects and delete.

import { afterAll, beforeAll, expect, it } from 'vitest';
// @ts-expect-error plain JS module without types
import { startMock } from '../mock/server.mjs';
import { Api, ApiError } from '../src/api';

let mock: { url: string; close(): Promise<void> };
let api: Api;

beforeAll(async () => {
  mock = await startMock({ speed: 0.02 });
  api = new Api(mock.url, fetch);
});
afterAll(() => mock?.close());

const SKILL_MD = '---\nname: deploy\ndescription: Deploys the site.\nallowed-tools: Bash, Read\n---\n# Deploy\n';

it('lists the sample skills', async () => {
  const list = await api.skills();
  expect(list.map((s) => s.name)).toEqual(['bench', 'release-notes']);
  expect(list[0].allowed_tools.length).toBeGreaterThan(0);
});

it('uploads a folder, then replaces it by name, keeping its projects', async () => {
  const files = [
    { file: new Blob([SKILL_MD]), path: 'SKILL.md' },
    { file: new Blob(['echo hi\n']), path: 'scripts/run.sh' },
  ];
  const first = await api.uploadSkill({ name: 'folder-name', files });
  expect(first.replaced).toBe(false);
  expect(first.skill).toMatchObject({ name: 'deploy', description: 'Deploys the site.', allowed_tools: ['Bash', 'Read'], files: 2 });
  expect((await api.skillFiles(first.skill.id)).map((f) => f.path)).toEqual(['SKILL.md', 'scripts/run.sh']);

  const projectId = (await api.projects())[0].id;
  await api.updateSkill(first.skill.id, { all_projects: false, project_ids: [projectId] });
  const again = await api.uploadSkill({ name: 'deploy', files: files.slice(0, 1) });
  expect(again.replaced).toBe(true);
  expect(again.skill).toMatchObject({ id: first.skill.id, files: 1, all_projects: false, project_ids: [projectId] });

  await api.deleteSkill(first.skill.id);
  expect((await api.skills()).some((s) => s.name === 'deploy')).toBe(false);
});

it('uploads a zip named by the name part', async () => {
  const r = await api.uploadSkill({ name: 'zipped', zip: new Blob(['PK\x03\x04']) });
  expect(r).toMatchObject({ replaced: false, skill: { name: 'zipped' } });
});

it('refuses a folder without SKILL.md, and hooks', async () => {
  const err = (p: Promise<unknown>) => p.then(() => null, (e: ApiError) => [e.status, e.code]);
  expect(await err(api.uploadSkill({ name: 'x', files: [{ file: new Blob(['a']), path: 'README.md' }] }))).toEqual([400, 'bad_request']);
  const hooks = '---\nname: evil\nhooks: {}\n---\n';
  expect(await err(api.uploadSkill({ name: 'x', files: [{ file: new Blob([hooks]), path: 'SKILL.md' }] }))).toEqual([422, 'skill_hooks']);
});
