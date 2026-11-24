// @vitest-environment happy-dom
// T66 (iteration 7, SPEC 0.235): Settings → MCP servers (the list, its statuses and tools, the
// add and edit form, Test, Delete) and the servers a project's settings list. Header values
// are write-only: a password field, cleared when the form closes, sent only when given.

import { afterEach, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { ApiError, type Api, type McpServerFields } from '../src/api';
import type { AppState } from '../src/app';
import type { McpServer, Project } from '../src/proto';
import { McpServersSection, ProjectMcpServers } from '../src/ui/McpServers';
import { Settings } from '../src/ui/Settings';
import { WEB_SEARCHES } from '../src/websearch';

let root: HTMLElement;
afterEach(() => {
  if (root) render(null, root);
  document.body.innerHTML = '';
  vi.unstubAllGlobals();
  localStorage.clear();
});

const P1 = { id: 'p1', name: 'kiln', is_personal: false } as Project;
const P2 = { id: 'p2', name: 'hqc', is_personal: false } as Project;

const GITHUB: McpServer = {
  id: 's1',
  name: 'github',
  url: 'https://api.githubcopilot.com/mcp/',
  header_names: ['Authorization'],
  timeout_s: 300,
  all_projects: true,
  project_ids: [],
  status: 'ok',
  auth: 'headers',
  signed_in: false,
  tools: ['get_me', 'create_issue'],
};
const KICAD: McpServer = {
  id: 's2',
  name: 'kicad',
  url: 'https://kicad.example/mcp',
  header_names: [],
  timeout_s: 600,
  all_projects: false,
  project_ids: ['p1'],
  status: 'failing',
  auth: 'headers',
  signed_in: false,
  last_error: 'the server answered 401',
  tools: Array.from({ length: 12 }, (_, i) => `tool_${i}`),
};
const NEW: McpServer = { ...KICAD, id: 's3', name: 'fresh', project_ids: ['p2'], status: 'unknown', tools: [], last_error: undefined };

function stubApp(api: Partial<Api>) {
  return {
    api,
    sortedProjects: () => [P1, P2],
    project: (id: string) => [P1, P2].find((p) => p.id === id),
  } as unknown as AppState;
}

async function mount(app: AppState, node = <McpServersSection app={app} />) {
  root = document.createElement('div');
  document.body.appendChild(root);
  await act(async () => render(node, root));
  await act(() => new Promise((r) => setTimeout(r, 0)));
}

const $ = <T extends Element = HTMLElement>(sel: string) => root.querySelector<T>(sel);
const $$ = <T extends Element = HTMLElement>(sel: string) => [...root.querySelectorAll<T>(sel)];
const rows = () => $$('.mcp-row');

async function type(el: HTMLInputElement, value: string) {
  await act(async () => {
    el.value = value;
    el.dispatchEvent(new Event('input', { bubbles: true }));
  });
}
async function click(el: Element | null) {
  await act(async () => (el as HTMLElement).click());
  await act(() => new Promise((r) => setTimeout(r, 0)));
}
const input = (name: string, i = 0) => $$<HTMLInputElement>(`input[name="${name}"]`)[i];
const save = () => click($('.btn-save-mcp'));

/** The add form filled with a good server. */
async function fillNew(name = 'fresh', url = 'https://kicad.example/mcp') {
  await click($('.btn-add-mcp'));
  await type(input('mcp_name'), name);
  await type(input('mcp_url'), url);
}

it('lists the servers with their statuses, tools and projects', async () => {
  const unknown = { ...NEW, project_ids: [] };
  await mount(stubApp({ mcpServers: vi.fn(async () => [GITHUB, KICAD, unknown]) }));
  const [gh, kc, fr] = rows();
  expect(gh.querySelector('.mcp-status')!.textContent).toBe('ok');
  expect(gh.textContent).toContain('get_me, create_issue');
  expect(gh.textContent).toContain('All projects');
  expect(gh.textContent).toContain('headers: Authorization');

  expect(kc.querySelector('.mcp-status')!.textContent).toBe('failing');
  expect(kc.querySelector('.mcp-error')!.textContent).toBe('the server answered 401');
  // Many tools fold under a count.
  expect(kc.querySelector('details.mcp-tools summary')!.textContent).toBe('12 tools');
  expect(kc.querySelector('.mcp-meta')!.textContent).toMatch(/^kiln · timeout 600 s/);

  expect(fr.querySelector('.mcp-status')!.textContent).toBe('not tested yet');
  expect(fr.querySelector('.mcp-error')).toBeNull();
  expect(fr.textContent).toContain('No project yet');
});

it('is a section of Settings', async () => {
  const app = {
    ...stubApp({ mcpServers: vi.fn(async () => [GITHUB]), workers: vi.fn(async () => []) }),
    me: { email: 'me@example.com' },
    theme: () => 'warm',
    lookName: () => 'spacious',
    newSessionProject: () => undefined,
    webSearch: WEB_SEARCHES[0],
    providers: [],
  } as unknown as AppState;
  await mount(app, <Settings app={app} />);
  expect($$('h2').map((h) => h.textContent)).toContain('MCP servers');
  expect(rows()).toHaveLength(1);
});

it('checks the name, the URL and the timeout before sending', async () => {
  // The hub refuses what passes here, so the form stays open.
  const createMcpServer = vi.fn(async () => {
    throw new ApiError(400, 'refused');
  });
  await mount(stubApp({ mcpServers: vi.fn(async () => []), createMcpServer }));
  await click($('.btn-add-mcp'));
  expect(input('mcp_timeout').value).toBe('300');
  expect(input('mcp_all_projects').checked).toBe(true);

  const problem = async (field: string, value: string) => {
    await type(input(field), value);
    await save();
    return $('.mcp-problem')?.textContent ?? null;
  };
  await type(input('mcp_url'), 'https://kicad.example/mcp');
  for (const bad of ['', 'GitHub', 'hqc', '_x', 'x_', 'a__b', 'a'.repeat(33), 'with space']) {
    expect(await problem('mcp_name', bad)).toMatch(/^Name:/);
  }
  expect(await problem('mcp_name', 'my-server_2')).toBeNull();
  // Not sent: the timeout field is fine, so the form went to the hub with a good name.
  expect(createMcpServer).toHaveBeenCalledTimes(1);
  createMcpServer.mockClear();

  expect(await problem('mcp_url', 'http://kicad.example/mcp')).toMatch(/^URL: https only/);
  expect(await problem('mcp_url', 'https://me:pw@kicad.example/mcp')).toMatch(/^URL: without a user/);
  expect(await problem('mcp_url', 'kicad.example')).toMatch(/^URL:/);
  expect(await problem('mcp_url', 'http://localhost:8080/mcp')).toBeNull();
  createMcpServer.mockClear();

  for (const bad of ['29', '1801', '60.5', 'abc']) {
    expect(await problem('mcp_timeout', bad)).toMatch(/^Timeout:/);
  }
  expect(createMcpServer).not.toHaveBeenCalled();
});

it('sends the headers of a new server and clears the values once saved', async () => {
  const createMcpServer = vi.fn(async (_: McpServerFields) => NEW);
  await mount(stubApp({ mcpServers: vi.fn(async () => [GITHUB]), createMcpServer }));
  await fillNew();
  await type(input('mcp_timeout'), '900');
  const value = input('mcp_header_value');
  expect(value.type).toBe('password');
  expect(value.getAttribute('autocomplete')).toBe('off');
  await type(input('mcp_header_name'), 'Authorization');
  await type(value, 'Bearer s3cret');
  await click($('.btn-add-header'));
  await type(input('mcp_header_name', 1), 'X-Team');
  await type(input('mcp_header_value', 1), 'core');
  // Only some projects.
  await click(input('mcp_all_projects'));
  await click($$<HTMLInputElement>('input[name="mcp_project"]').find((i) => i.value === 'p2')!);
  await save();

  expect(createMcpServer).toHaveBeenCalledWith({
    name: 'fresh',
    url: 'https://kicad.example/mcp',
    timeout_s: 900,
    all_projects: false,
    project_ids: ['p2'],
    headers: [
      { name: 'Authorization', value: 'Bearer s3cret' },
      { name: 'X-Team', value: 'core' },
    ],
  });
  // The form is gone with its values; the list has the server, without them.
  expect($('.mcp-form')).toBeNull();
  expect(rows().map((r) => r.querySelector('.mcp-name')!.textContent)).toEqual(['fresh', 'github']);
  expect(root.innerHTML).not.toContain('s3cret');
  expect(JSON.stringify({ ...localStorage })).not.toContain('s3cret');
  // A new form starts empty.
  await click($('.btn-add-mcp'));
  expect(input('mcp_header_value').value).toBe('');
  expect(input('mcp_name').value).toBe('');
});

it('sends no headers for a new server without them, and keeps the form when the hub refuses', async () => {
  const createMcpServer = vi.fn(async () => {
    throw new ApiError(409, 'you have a server named "fresh"', 'name_taken');
  });
  await mount(stubApp({ mcpServers: vi.fn(async () => []), createMcpServer }));
  await fillNew();
  await save();
  expect(createMcpServer.mock.calls[0]).toEqual([
    { name: 'fresh', url: 'https://kicad.example/mcp', timeout_s: 300, all_projects: true, project_ids: [] },
  ]);
  expect($('.mcp-section-error')!.textContent).toBe('you have a server named "fresh"');
  expect($('.mcp-form')).not.toBeNull();
});

it('says what to do when the hub has no HQC_MCP_KEY', async () => {
  const createMcpServer = vi.fn(async () => {
    throw new ApiError(422, 'no key', 'mcp_key_missing');
  });
  await mount(stubApp({ mcpServers: vi.fn(async () => []), createMcpServer }));
  await fillNew();
  await type(input('mcp_header_name'), 'Authorization');
  await type(input('mcp_header_value'), 'Bearer s3cret');
  await save();
  expect($('.mcp-section-error')!.textContent).toBe(
    'The hub keeps no tokens: set HQC_MCP_KEY on the hub to give a server headers.',
  );
  // Cancel clears what was typed.
  await click($('.btn-cancel-mcp'));
  expect($('.mcp-form')).toBeNull();
  expect($('.mcp-section-error')).toBeNull();
  await click($('.btn-add-mcp'));
  expect(input('mcp_header_value').value).toBe('');
});

it('an edit that leaves the headers alone sends no headers', async () => {
  const updateMcpServer = vi.fn(async (_: string, f: McpServerFields) => ({ ...KICAD, ...f }) as McpServer);
  await mount(stubApp({ mcpServers: vi.fn(async () => [GITHUB, KICAD]), updateMcpServer }));
  await click(rows()[0].querySelector('.btn-edit-mcp'));
  // The names of the kept headers, no value field.
  expect($('.mcp-headers-kept')!.textContent).toContain('Kept: Authorization');
  expect(input('mcp_header_value')).toBeUndefined();
  expect(input('mcp_name').value).toBe('github');
  await type(input('mcp_timeout'), '120');
  await save();
  expect(updateMcpServer).toHaveBeenCalledTimes(1);
  const [id, f] = updateMcpServer.mock.calls[0];
  expect(id).toBe('s1');
  expect(f).toEqual({
    name: 'github',
    url: 'https://api.githubcopilot.com/mcp/',
    timeout_s: 120,
    all_projects: true,
    project_ids: [],
  });
  expect('headers' in f).toBe(false);
  expect($('.mcp-form')).toBeNull();
});

it('"Replace headers" sends all of them again', async () => {
  const updateMcpServer = vi.fn(async (_: string, f: McpServerFields) => ({ ...GITHUB, header_names: f.headers!.map((h) => h.name) }));
  await mount(stubApp({ mcpServers: vi.fn(async () => [GITHUB]), updateMcpServer }));
  await click($('.btn-edit-mcp'));
  await click($('.btn-replace-headers'));
  // The kept names to fill in again, the values empty.
  expect(input('mcp_header_name').value).toBe('Authorization');
  expect(input('mcp_header_value').value).toBe('');
  expect(input('mcp_header_value').type).toBe('password');
  // A value is needed for each.
  await save();
  expect($('.mcp-problem')!.textContent).toBe('Header Authorization: a value is needed.');
  await type(input('mcp_header_value'), 'Bearer n3w');
  await click($('.btn-add-header'));
  await type(input('mcp_header_name', 1), 'X-Org');
  await type(input('mcp_header_value', 1), 'acme');
  await save();
  expect(updateMcpServer.mock.calls[0][1].headers).toEqual([
    { name: 'Authorization', value: 'Bearer n3w' },
    { name: 'X-Org', value: 'acme' },
  ]);
  expect($('.mcp-form')).toBeNull();
  expect(root.innerHTML).not.toContain('n3w');
  expect(rows()[0].textContent).toContain('headers: Authorization, X-Org');
});

it('deletes a server after a confirmation', async () => {
  const deleteMcpServer = vi.fn(async () => {});
  await mount(stubApp({ mcpServers: vi.fn(async () => [GITHUB, KICAD]), deleteMcpServer }));
  vi.stubGlobal('confirm', () => false);
  await click(rows()[1].querySelector('.btn-delete-mcp'));
  expect(deleteMcpServer).not.toHaveBeenCalled();
  expect(rows()).toHaveLength(2);
  const confirm = vi.fn(() => true);
  vi.stubGlobal('confirm', confirm);
  await click(rows()[1].querySelector('.btn-delete-mcp'));
  expect(confirm.mock.calls[0]).toEqual([expect.stringContaining('“kicad”')]);
  expect(deleteMcpServer).toHaveBeenCalledWith('s2');
  expect(rows().map((r) => r.querySelector('.mcp-name')!.textContent)).toEqual(['github']);
});

it('"Test" shows the fresh status and tools', async () => {
  const testMcpServer = vi.fn(async () => ({ ...KICAD, status: 'ok' as const, last_error: undefined, tools: ['open_board'] }));
  await mount(stubApp({ mcpServers: vi.fn(async () => [KICAD]), testMcpServer }));
  expect(rows()[0].querySelector('.mcp-status')!.textContent).toBe('failing');
  await click($('.btn-test-mcp'));
  expect(testMcpServer).toHaveBeenCalledWith('s2');
  const row = rows()[0];
  expect(row.querySelector('.mcp-status')!.textContent).toBe('ok');
  expect(row.querySelector('.mcp-error')).toBeNull();
  expect(row.querySelector('.mcp-tools')!.textContent).toBe('Tools: open_board');
});

it("a project's settings list the servers it gets", async () => {
  const app = stubApp({ mcpServers: vi.fn(async () => [GITHUB, KICAD, NEW]) });
  await mount(app, <ProjectMcpServers app={app} projectId="p1" />);
  expect($('h2')!.textContent).toBe('MCP servers');
  expect(rows().map((r) => r.querySelector('.mcp-name')!.textContent)).toEqual(['github', 'kicad']);
  expect(rows()[0].textContent).toContain('all projects');
  expect($('a[href="#/settings"]')!.textContent).toBe('Settings → MCP servers');
  // Read-only: nothing to change here.
  expect($$('button')).toHaveLength(0);
  render(null, root);

  await mount(app, <ProjectMcpServers app={app} projectId="p2" />);
  expect(rows().map((r) => r.querySelector('.mcp-name')!.textContent)).toEqual(['github', 'fresh']);
  render(null, root);

  // A project no server is given to.
  const other = stubApp({ mcpServers: vi.fn(async () => [KICAD]) });
  await mount(other, <ProjectMcpServers app={other} projectId="p2" />);
  expect($('.empty-inline')!.textContent).toBe('None for this project.');
});

it('an OAuth server: no headers sent, Sign in opens the page the hub gives, Sign out forgets it', async () => {
  const oauth = {
    id: 'm9',
    name: 'linear',
    url: 'https://mcp.linear.app/mcp',
    header_names: [],
    timeout_s: 300,
    all_projects: true,
    project_ids: [],
    status: 'sign_in' as const,
    tools: [],
    auth: 'oauth' as const,
    signed_in: false,
  };
  const api: Partial<Api> = {
    mcpServers: vi.fn(async () => [oauth]),
    signInMcpServer: vi.fn(async () => 'https://linear.app/oauth/authorize?x=1'),
    signOutMcpServer: vi.fn(async () => {}),
  };
  const app = stubApp(api);
  const tab = { opener: {} as unknown, location: { href: '' }, close: vi.fn() };
  const open = vi.spyOn(window, 'open').mockReturnValue(tab as unknown as Window);
  await mount(app);
  expect(root.querySelector('.mcp-status-sign-in')?.textContent).toBe('sign in');
  await act(async () => (root.querySelector('.btn-sign-in-mcp') as HTMLButtonElement).click());
  expect(open).toHaveBeenCalled();
  expect(api.signInMcpServer).toHaveBeenCalledWith('m9');
  expect(tab.location.href).toBe('https://linear.app/oauth/authorize?x=1');
  expect(tab.opener).toBeNull();
  open.mockRestore();
});
