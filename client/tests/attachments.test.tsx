// @vitest-environment happy-dom
// T16: attachments (SPEC §4, §6, §12) — picking files in the composer, upload before the
// input, signed links renewed on 403/410, rendering of images, diffs, files and HTML, and
// paths of the working copy in markdown that copy themselves (t11 #1).

import { afterEach, describe, expect, it, vi } from 'vitest';
import { render } from 'preact';
import { act } from 'preact/test-utils';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ArtifactStore, formatSize } from '../src/artifacts';
import { parseEvent, type Artifact, type ArtifactUrl, type HqcEvent } from '../src/proto';
import { ChatSession, type OutgoingFile } from '../src/session';
import { Timeline } from '../src/timeline';
import { ArtifactViewer } from '../src/ui/attachments';
import { ChatItems, type ChatActions } from '../src/ui/cards';
import { Composer, toOutgoing } from '../src/ui/Chat';
import { Markdown, diffClass, filePath } from '../src/ui/Markdown';
import type { Api } from '../src/api';
import type { HubSocket } from '../src/ws';

const WIRE = join(import.meta.dirname, '../../crates/hqc-proto/examples/wire');
const ev = (name: string, patch: (e: any) => void = () => {}): HqcEvent => {
  const e = JSON.parse(readFileSync(join(WIRE, `${name}.json`), 'utf8'));
  patch(e);
  return parseEvent(e);
};

let root: HTMLElement | undefined;
afterEach(() => {
  if (root) render(null, root);
  root = undefined;
  document.body.innerHTML = '';
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function mount(node: preact.ComponentChild): HTMLElement {
  root = document.createElement('div');
  document.body.appendChild(root);
  act(() => render(node, root!));
  return root;
}

const flush = () => act(async () => new Promise((r) => setTimeout(r, 0)));

const meta = (id: string, kind: Artifact['kind'], name: string, size = 10): Artifact => ({
  id,
  kind,
  name,
  mime: '',
  size,
  sha256: '',
  created_at: '',
});

/** An `Api` half for the store: metadata from `metas`, links `/artifacts/<id>?n=<count>`. */
function fakeSource(metas: Artifact[], base = '') {
  let n = 0;
  return {
    links: 0,
    artifact: vi.fn(async (id: string) => {
      const m = metas.find((x) => x.id === id);
      if (!m) throw new Error('404');
      return m;
    }),
    artifactLink: vi.fn(async (id: string): Promise<ArtifactUrl> => {
      n++;
      return { url: `${base}/artifacts/${id}?exp=1&sig=s${n}`, expires_at: new Date(Date.now() + 600_000).toISOString() };
    }),
  };
}

describe('ArtifactStore', () => {
  it('caches links until close to expiry and renews them on 403/410', async () => {
    let now = 1_000_000;
    const src = {
      artifact: vi.fn(),
      artifactLink: vi.fn(async (id: string) => ({
        url: `/artifacts/${id}?sig=${src.artifactLink.mock.calls.length}`,
        expires_at: new Date(now + 600_000).toISOString(),
      })),
    };
    const statuses = [410, 200];
    const fetchImpl = vi.fn(async (url: string) => {
      const status = statuses.shift()!;
      return new Response(status === 200 ? `body of ${url}` : 'gone', { status });
    });
    const store = new ArtifactStore(src as never, fetchImpl as never, () => now, 'https://hqc.example.com');
    expect(await store.url('a')).toBe('/artifacts/a?sig=1');
    expect(await store.url('a')).toBe('/artifacts/a?sig=1');
    now += 590_000; // 10 s left: renewed
    expect(await store.url('a')).toBe('/artifacts/a?sig=2');
    expect(await store.downloadUrl('a')).toBe('/artifacts/a?sig=2&dl=1');
    // The host says the link expired: one renewal, then the content.
    expect(await store.text('a')).toBe('body of /artifacts/a?sig=3');
    expect(fetchImpl).toHaveBeenCalledTimes(2);
    // Another error is not retried.
    fetchImpl.mockResolvedValueOnce(new Response('x', { status: 500 }));
    await expect(store.text('a')).rejects.toThrow('HTTP 500');

    expect(store.isSeparateOrigin('/artifacts/a')).toBe(false);
    expect(store.isSeparateOrigin('https://hqc.example.com/artifacts/a')).toBe(false);
    expect(store.isSeparateOrigin('https://artifacts.example.com/artifacts/a')).toBe(true);
  });

  it('metadata is fetched once; a failure is not cached', async () => {
    const src = fakeSource([meta('a', 'text', 'a.txt')]);
    const store = new ArtifactStore(src as never);
    await Promise.all([store.meta('a'), store.meta('a')]);
    expect(src.artifact).toHaveBeenCalledTimes(1);
    await expect(store.meta('b')).rejects.toThrow();
    await expect(store.meta('b')).rejects.toThrow();
    expect(src.artifact).toHaveBeenCalledTimes(3);
  });

  it('sizes', () => {
    expect([formatSize(12), formatSize(48213), formatSize(5 * 1024 * 1024)]).toEqual(['12 B', '47 KB', '5.0 MB']);
  });
});

describe('sending files', () => {
  function session(api: Partial<Api>) {
    const socket = { subscribe: () => () => {} } as unknown as HubSocket;
    return new ChatSession(api as Api, socket, 's1');
  }

  it('uploads first, then sends the input with the artifact ids; a resend does not upload again', async () => {
    const uploads: string[] = [];
    let failInput = true;
    const api = {
      uploadArtifact: vi.fn(async (_: Blob, name: string) => {
        uploads.push(name);
        return meta(`id-${name}`, 'file', name);
      }),
      input: vi.fn(async (_s: string, text: string, op: string, ids: string[]) => {
        if (failInput) throw new Error('offline');
        return ev('event.user.message', (e) => {
          e.client_op_id = op;
          e.payload = { text, artifact_ids: ids };
        });
      }),
    };
    const chat = session(api);
    const files: OutgoingFile[] = [
      { file: new Blob(['png']), name: 'shot.png', preview: 'data:image/png;base64,eA==' },
      { file: new Blob(['abc']), name: 'notes.txt' },
    ];
    const op = chat.send('', files);
    const pending = chat.timeline.items().find((i) => i.kind === 'pending')!;
    expect(pending.kind === 'pending' && pending.pending.attachments).toEqual([
      { name: 'shot.png', size: 3, preview: 'data:image/png;base64,eA==' },
      { name: 'notes.txt', size: 3, preview: undefined },
    ]);
    await flush();
    expect(api.input).toHaveBeenLastCalledWith('s1', '', op, ['id-shot.png', 'id-notes.txt']);
    expect(chat.timeline.items()[0]).toMatchObject({ kind: 'pending', pending: { status: 'failed' } });

    failInput = false;
    chat.resend(op);
    await flush();
    expect(uploads).toEqual(['shot.png', 'notes.txt']);
    expect(api.input).toHaveBeenCalledTimes(2);
    const stored = chat.timeline.items();
    expect(stored).toHaveLength(1);
    expect(stored[0].kind).toBe('event');
  });

  it('a failed upload fails the message with the file name and sends no input', async () => {
    const api = {
      uploadArtifact: vi.fn(async () => {
        throw new Error('an artifact is at most 100000000 bytes');
      }),
      input: vi.fn(),
    };
    const chat = session(api);
    chat.send('see', [{ file: new Blob(['x']), name: 'big.bin' }]);
    await flush();
    expect(api.input).not.toHaveBeenCalled();
    expect(chat.timeline.items()[0]).toMatchObject({
      kind: 'pending',
      pending: { status: 'failed', error: 'big.bin: an artifact is at most 100000000 bytes' },
    });
  });
});

describe('a picked file', () => {
  it('is read into memory when picked: the upload sends those bytes, not the picked file again', async () => {
    const txt = new File(['hello'], 'notes.txt', { type: 'text/plain' });
    const f = await toOutgoing(txt);
    expect(f.file).not.toBe(txt);
    expect([f.name, f.file.type, await f.file.text()]).toEqual(['notes.txt', 'text/plain', 'hello']);
  });

  it('a HEIC becomes a JPEG where the browser decodes it, and stays HEIC where it does not', async () => {
    const heic = new File([new Uint8Array([0, 0, 0, 24, 102, 116, 121, 112])], 'HQC.heic', { type: 'image/heic' });
    // Chrome on a computer does not decode HEIC: the file is sent as it is.
    const reject = vi.spyOn(HTMLImageElement.prototype, 'decode').mockRejectedValue(new DOMException('', 'EncodingError'));
    try {
      expect((await toOutgoing(heic)).name).toBe('HQC.heic');
    } finally {
      reject.mockRestore();
    }
    const jpeg = new Blob(['jpeg'], { type: 'image/jpeg' });
    const draw = vi.fn();
    const spies = [
      vi.spyOn(HTMLImageElement.prototype, 'decode').mockResolvedValue(undefined),
      vi.spyOn(HTMLImageElement.prototype, 'naturalWidth', 'get').mockReturnValue(8000),
      vi.spyOn(HTMLImageElement.prototype, 'naturalHeight', 'get').mockReturnValue(6000),
      vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue({ drawImage: draw } as unknown as CanvasRenderingContext2D),
      vi.spyOn(HTMLCanvasElement.prototype, 'toBlob').mockImplementation((cb) => cb(jpeg)),
    ];
    try {
      const f = await toOutgoing(heic);
      expect([f.name, f.file]).toEqual(['HQC.jpg', jpeg]);
      expect(f.preview).toMatch(/^data:image\/jpeg;base64,/);
      // A 48-megapixel photo is drawn within the 16 million pixels iOS draws.
      const [, , , w, h] = draw.mock.calls[0] as number[];
      expect(w * h).toBeLessThanOrEqual(16_000_000);
      expect(w / h).toBeCloseTo(8000 / 6000, 2);
    } finally {
      for (const s of spies) s.mockRestore();
    }
  });
});

describe('composer', () => {
  it('picks several files, previews images, removes one and sends files without text', async () => {
    const png = new File([new Uint8Array([137, 80, 78, 71])], 'shot.png', { type: 'image/png' });
    const txt = new File(['hello'], 'notes.txt', { type: 'text/plain' });
    const picked = await Promise.all([png, txt].map(toOutgoing));
    expect(picked[0].preview).toMatch(/^data:image\/png;base64,/);
    expect(picked[1].preview).toBeUndefined();

    const onSend = vi.fn();
    const onRemove = vi.fn();
    const onFiles = vi.fn();
    const el = mount(<Composer files={picked} onSend={onSend} onFiles={onFiles} onRemove={onRemove} />);
    expect(el.querySelector<HTMLImageElement>('.composer-files img')!.src).toMatch(/^data:image\/png/);
    expect(el.querySelector('.composer-files .attachment-chip')!.textContent).toContain('notes.txt');
    const send = el.querySelector<HTMLButtonElement>('button[type=submit]')!;
    expect(send.disabled).toBe(false);
    await act(async () => send.click());
    expect(onSend).toHaveBeenCalledWith('');
    await act(async () => el.querySelectorAll<HTMLButtonElement>('.composer-file-remove')[1].click());
    expect(onRemove).toHaveBeenCalledWith(1);

    // The paperclip opens the hidden picker; picked files go to onFiles.
    const input = el.querySelector<HTMLInputElement>('input[type=file]')!;
    expect(input.multiple).toBe(true);
    const click = vi.spyOn(input, 'click');
    await act(async () => el.querySelector<HTMLButtonElement>('.btn-attach')!.click());
    expect(click).toHaveBeenCalled();
    Object.defineProperty(input, 'files', { value: [txt], configurable: true });
    await act(async () => void input.dispatchEvent(new Event('change', { bubbles: true })));
    expect(onFiles).toHaveBeenCalledWith([txt]);
  });

  it('without text and files nothing can be sent', () => {
    const el = mount(<Composer files={[]} onSend={() => {}} />);
    expect(el.querySelector<HTMLButtonElement>('button[type=submit]')!.disabled).toBe(true);
  });
});

describe('rendering attachments', () => {
  function chatWith(events: HqcEvent[], store?: ArtifactStore, over: Partial<ChatActions> = {}) {
    const t = new Timeline();
    t.applyAll(events);
    const actions: ChatActions = {
      timeline: t,
      approve: vi.fn(async () => {}),
      answer: vi.fn(async () => {}),
      resend: vi.fn(),
      discard: vi.fn(),
      openArtifact: vi.fn(),
      artifacts: store,
      ...over,
    };
    return { el: mount(<ChatItems items={t.items()} actions={actions} />), actions };
  }

  const DIFF = '--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n same';

  it('an image inline, a diff as a diff, text and file as cards, from user.message and attachment', async () => {
    const metas = [
      meta('img', 'image', 'screen.png'),
      meta('dif', 'diff', 'fix.patch'),
      meta('txt', 'text', 'notes.txt', 2048),
      meta('bin', 'file', 'data.bin', 5),
    ];
    const src = fakeSource(metas);
    const fetchImpl = vi.fn(async () => new Response(DIFF));
    const store = new ArtifactStore(src as never, fetchImpl as never);
    const { el, actions } = chatWith(
      [
        ev('event.user.message', (e) => {
          e.seq = 1;
          e.payload = { text: 'Look', artifact_ids: ['img', 'dif', 'txt'] };
        }),
        ev('event.attachment', (e) => {
          e.seq = 2;
          e.payload = { artifact_id: 'bin', kind: 'file', name: 'data.bin', caption: 'dump' };
        }),
      ],
      store,
    );
    await flush();
    await flush();
    const img = el.querySelector<HTMLImageElement>('.msg-user .attachment-image img')!;
    expect(img.getAttribute('src')).toBe('/artifacts/img?exp=1&sig=s1');
    expect(img.alt).toBe('screen.png');

    const diff = el.querySelector('.msg-user .md-code-diff')!;
    expect(diff.querySelector('.md-code-lang')!.textContent).toBe('fix.patch');
    expect(diff.querySelector('.diff-add')!.textContent).toBe('+new\n');
    expect(diff.querySelector('.diff-del')!.textContent).toBe('-old\n');
    expect(diff.querySelector('.diff-hunk')!.textContent).toContain('@@');
    expect(diff.querySelector('pre')!.textContent).toBe(DIFF);

    const cards = [...el.querySelectorAll('.card-attachment')];
    expect(cards.map((c) => c.querySelector('.card-title')!.textContent)).toEqual([
      'notes.txt text · 2.0 KB',
      'data.bin file · 5 B',
    ]);
    expect(el.querySelector('.attachment-caption')!.textContent).toBe('dump');
    // A file has only "download"; text can be opened too.
    const buttons = (c: Element) => [...c.querySelectorAll('button')].map((b) => b.textContent);
    expect(buttons(cards[0])).toEqual(['Open', 'Download']);
    expect(buttons(cards[1])).toEqual(['Download']);
    await act(async () => cards[1].querySelector('button')!.click());
    expect(actions.openArtifact).toHaveBeenLastCalledWith('bin', true);
    await act(async () => img.parentElement!.click());
    expect(actions.openArtifact).toHaveBeenLastCalledWith('img');
  });

  it('a diff body waits for its size; large text and diffs only download (T22)', async () => {
    const MB = 1024 * 1024;
    let release!: (m: Artifact) => void;
    const slow = new Promise<Artifact>((r) => (release = r));
    const metas = [meta('big', 'diff', 'huge.patch', 3 * MB), meta('mid', 'diff', 'mid.patch', MB), meta('txt', 'text', 'log.txt', 3 * MB)];
    const src = fakeSource(metas);
    const plain = src.artifact.getMockImplementation()!;
    src.artifact.mockImplementation((id: string) => (id === 'slow' ? slow : plain(id)));
    const fetchImpl = vi.fn(async () => new Response(DIFF));
    const store = new ArtifactStore(src as never, fetchImpl as never);
    const { el } = chatWith(
      [
        ev('event.user.message', (e) => {
          e.seq = 1;
          e.payload = { text: 'Look', artifact_ids: ['slow', 'big', 'mid', 'txt'] };
        }),
      ],
      store,
    );
    await flush();
    await flush();
    // Nothing is fetched: one size is unknown, the others are too large to show.
    expect(fetchImpl).not.toHaveBeenCalled();
    expect(el.querySelector('.md-code-diff')).toBeNull();
    const buttons = (name: string) => {
      const card = [...el.querySelectorAll('.card-attachment')].find((c) => c.textContent!.includes(name))!;
      return [...card.querySelectorAll('button')].map((b) => b.textContent);
    };
    expect(buttons('huge.patch')).toEqual(['Download']);
    expect(buttons('log.txt')).toEqual(['Download']);
    // Over the inline limit but under 2 MB: a card that still opens.
    expect(buttons('mid.patch')).toEqual(['Open', 'Download']);

    await act(async () => release(meta('slow', 'diff', 'small.patch', 40)));
    await flush();
    await flush();
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(el.querySelector('.md-code-diff .md-code-lang')!.textContent).toBe('small.patch');
  });

  it('a body announced over the limit is not read', async () => {
    const src = fakeSource([meta('d', 'diff', 'd.patch')]);
    const fetchImpl = vi.fn(async () => new Response('x', { headers: { 'content-length': String(600 * 1024) } }));
    const store = new ArtifactStore(src as never, fetchImpl as never);
    await expect(store.text('d')).rejects.toThrow('too large');
  });

  it('an image shown again has its link in the first render, without asking the hub', async () => {
    const src = fakeSource([meta('img', 'image', 'screen.png')]);
    const store = new ArtifactStore(src as never);
    const first = chatWith([ev('event.attachment', (e) => (e.payload.artifact_id = 'img'))], store);
    await flush();
    expect(first.el.querySelector('.attachment-image img')!.getAttribute('src')).toBe('/artifacts/img?exp=1&sig=s1');
    // The chat opened again: the same link at once, so the browser has the image in its cache.
    const again = chatWith([ev('event.attachment', (e) => (e.payload.artifact_id = 'img'))], store);
    expect(again.el.querySelector('.attachment-image img')!.getAttribute('src')).toBe('/artifacts/img?exp=1&sig=s1');
    await flush();
    expect(src.artifactLink).toHaveBeenCalledTimes(1);
  });

  it('a broken image link is renewed once, then the image becomes a card', async () => {
    const src = fakeSource([meta('img', 'image', 'screen.png')]);
    const store = new ArtifactStore(src as never);
    const { el } = chatWith([ev('event.attachment', (e) => (e.payload.artifact_id = 'img'))], store);
    await flush();
    let img = el.querySelector<HTMLImageElement>('.attachment-image img')!;
    await act(async () => void img.dispatchEvent(new Event('error')));
    await flush();
    img = el.querySelector<HTMLImageElement>('.attachment-image img')!;
    expect(img.getAttribute('src')).toBe('/artifacts/img?exp=1&sig=s2');
    await act(async () => void img.dispatchEvent(new Event('error')));
    await flush();
    expect(el.querySelector('.attachment-image')).toBeNull();
    expect(el.querySelector('.card-attachment')!.textContent).toContain('screen.png');
  });

  it('HTML is a link on the hub domain and a sandboxed iframe on the artifact domain', async () => {
    const html = meta('h', 'html', 'report.html');
    const onHub = chatWith([ev('event.attachment', (e) => (e.payload = { artifact_id: 'h', kind: 'html', name: 'report.html' }))], new ArtifactStore(fakeSource([html]) as never));
    await flush();
    expect(onHub.el.querySelector('iframe')).toBeNull();
    expect(onHub.el.querySelector('.card-attachment button')!.textContent).toBe('Open');
    render(null, root!);

    const separate = new ArtifactStore(fakeSource([html], 'https://artifacts.example.com') as never, undefined, undefined, 'https://hqc.example.com');
    const { el } = chatWith([ev('event.attachment', (e) => (e.payload = { artifact_id: 'h', kind: 'html', name: 'report.html' }))], separate);
    await flush();
    const frame = el.querySelector('iframe')!;
    expect(frame.getAttribute('src')).toBe('https://artifacts.example.com/artifacts/h?exp=1&sig=s1');
    expect(frame.getAttribute('sandbox')).toBe('allow-scripts');
  });

  it("the agent's hqc_attach (T31): the files render as a user's do, after the tool call", async () => {
    const src = fakeSource([meta('img', 'image', 'shot.png'), meta('dif', 'diff', 'fix.patch')]);
    const fetchImpl = vi.fn(async () => new Response(DIFF));
    const store = new ArtifactStore(src as never, fetchImpl as never);
    const call = (seq: number, id: string) =>
      ev('event.tool.call', (e) => {
        e.seq = seq;
        e.payload = { call_id: id, tool: 'mcp__hqc__hqc_attach', args_preview: '{"path":"shot.png"}' };
      });
    const result = (seq: number, id: string) =>
      ev('event.tool.result', (e) => {
        e.seq = seq;
        e.payload = { call_id: id, result_preview: '{"event_id": "x"}', error: false };
      });
    // The hub writes the attachment while the call is open: between the call and its result.
    const { el } = chatWith(
      [
        call(1, 't1'),
        ev('event.attachment', (e) => {
          e.seq = 2;
          e.payload = { artifact_id: 'img', kind: 'image', name: 'shot.png', caption: 'The layout' };
        }),
        result(3, 't1'),
        call(4, 't2'),
        ev('event.attachment', (e) => {
          e.seq = 5;
          e.payload = { artifact_id: 'dif', kind: 'diff', name: 'fix.patch' };
        }),
        result(6, 't2'),
      ],
      store,
    );
    await flush();
    await flush();
    expect(el.querySelectorAll('.tool-group').length).toBe(2);
    expect(el.querySelectorAll('.tool-group.tool-running').length).toBe(0);
    const img = el.querySelector<HTMLImageElement>('.attachment-image img')!;
    expect(img.getAttribute('src')).toBe('/artifacts/img?exp=1&sig=s1');
    expect(el.querySelector('.attachment-caption')!.textContent).toBe('The layout');
    expect(el.querySelector('.md-code-diff .diff-add')!.textContent).toBe('+new\n');
    // The order of the feed: the call, the image, the call, the diff.
    const order = [...el.querySelectorAll('.tool-group, .attachment')].map((n) =>
      n.classList.contains('tool-group') ? 'call' : n.className.replace('attachment attachment-kind-', ''),
    );
    expect(order).toEqual(['call', 'image', 'call', 'diff']);
  });

  it('a message from another session shows the files it handed over (T41)', async () => {
    const src = fakeSource([meta('img', 'image', 'board.png'), meta('txt', 'text', 'notes.txt')]);
    const store = new ArtifactStore(src as never);
    const { el } = chatWith(
      [
        ev('event.peer.message', (e) => {
          e.payload.text = 'Here is the board';
          e.payload.artifact_ids = ['img', 'txt'];
        }),
        ev('event.peer.message', (e) => {
          e.event_id = 'no-files';
          e.seq = 43;
        }),
      ],
      store,
    );
    await flush();
    await flush();
    const cards = [...el.querySelectorAll('.relay')];
    expect(cards.length).toBe(2);
    expect(cards[0].querySelector<HTMLImageElement>('.attachment-image img')!.alt).toBe('board.png');
    expect(cards[0].querySelector('.card-attachment .card-title')!.textContent).toContain('notes.txt');
    expect(cards[1].querySelector('.relay-files')).toBeNull();
  });

  it('with the viewer an image and a Markdown text open in it, other text in a tab', async () => {
    const metas = [
      meta('img', 'image', 'screen.png'),
      meta('md', 'text', 'report.md'),
      meta('plan', 'text', 'PLAN.Markdown'),
      { ...meta('typed', 'text', 'summary'), mime: 'text/markdown; charset=utf-8' },
      meta('txt', 'text', 'notes.txt'),
    ];
    const store = new ArtifactStore(fakeSource(metas) as never);
    const viewArtifact = vi.fn();
    const { el, actions } = chatWith(
      [
        ev('event.user.message', (e) => {
          e.seq = 1;
          e.payload = { text: 'Look', artifact_ids: metas.map((m) => m.id) };
        }),
      ],
      store,
      { viewArtifact },
    );
    await flush();
    await flush();
    await act(async () => el.querySelector<HTMLButtonElement>('.attachment-image')!.click());
    expect(viewArtifact).toHaveBeenLastCalledWith({ id: 'img', name: 'screen.png', kind: 'image' });
    const open = (name: string) =>
      [...el.querySelectorAll('.card-attachment')].find((c) => c.textContent!.includes(name))!.querySelector<HTMLButtonElement>('button')!;
    for (const m of metas.slice(1, 4)) {
      await act(async () => open(m.name).click());
      expect(viewArtifact).toHaveBeenLastCalledWith({ id: m.id, name: m.name, kind: 'markdown' });
    }
    expect(actions.openArtifact).not.toHaveBeenCalled();
    await act(async () => open('notes.txt').click());
    expect(actions.openArtifact).toHaveBeenLastCalledWith('txt');
    expect(viewArtifact).toHaveBeenCalledTimes(4);
  });

  it('without a store an attachment is a card from the event alone', () => {
    const { el } = chatWith([ev('event.attachment')]);
    expect(el.querySelector('.card-attachment .card-title')!.textContent).toContain('screen.png');
  });

  it('a pending message shows its files', () => {
    const t = new Timeline();
    t.addPending('op', '', [
      { name: 'a.png', size: 3, preview: 'data:image/png;base64,eA==' },
      { name: 'b.txt', size: 2048 },
    ]);
    const el = mount(
      <ChatItems
        items={t.items()}
        actions={{ timeline: t, approve: vi.fn(), answer: vi.fn(), resend: vi.fn(), discard: vi.fn(), openArtifact: vi.fn() }}
      />,
    );
    expect(el.querySelector<HTMLImageElement>('.msg-pending img')!.alt).toBe('a.png');
    expect(el.querySelector('.msg-pending .attachment-chip')!.textContent).toBe('b.txt 2.0 KB');
  });
});

describe('the viewer over the chat (SPEC §12)', () => {
  const viewer = (kind: 'image' | 'markdown', store: ArtifactStore, onClose = vi.fn(), onDownload = vi.fn()) =>
    mount(<ArtifactViewer id="a" name={kind === 'image' ? 'screen.png' : 'report.md'} kind={kind} store={store} onClose={onClose} onDownload={onDownload} />);

  it('renders a Markdown text as the feed does; Escape closes it and no key reaches the page', async () => {
    const fetchImpl = vi.fn(async () => new Response('# Report\n\n| Step | Days |\n|---|---|\n| Design | 2 |\n'));
    const store = new ArtifactStore(fakeSource([]) as never, fetchImpl as never);
    const onClose = vi.fn();
    const onDownload = vi.fn();
    // The request card takes Enter and Escape on `document`.
    const heard = vi.fn();
    document.addEventListener('keydown', heard);
    try {
      const el = viewer('markdown', store, onClose, onDownload);
      const dialog = el.querySelector('[role=dialog]')!;
      expect(dialog.getAttribute('aria-modal')).toBe('true');
      expect(dialog.querySelector('.viewer-body')!.textContent).toBe('loading…');
      await flush();
      expect(fetchImpl).toHaveBeenCalledWith('/artifacts/a?exp=1&sig=s1');
      expect(dialog.querySelector('.md h1')!.textContent).toBe('Report');
      expect([...dialog.querySelectorAll('.md-table th')].map((c) => c.textContent)).toEqual(['Step', 'Days']);
      expect([...dialog.querySelectorAll('.md-table td')].map((c) => c.textContent)).toEqual(['Design', '2']);
      // The text has the focus, so Space and the arrows scroll it instead of pressing "Close".
      const body = dialog.querySelector<HTMLElement>('.viewer-body')!;
      expect(document.activeElement).toBe(body);
      // A browser focuses a <div> only with a tabindex (happy-dom focuses any element).
      expect(body.getAttribute('tabindex')).toBe('-1');

      await act(async () => void body.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })));
      expect(onClose).not.toHaveBeenCalled();
      await act(async () => void body.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })));
      expect(onClose).toHaveBeenCalledTimes(1);
      expect(heard).not.toHaveBeenCalled();
      await act(async () => [...dialog.querySelectorAll('button')].find((b) => b.textContent === 'Download')!.click());
      expect(onDownload).toHaveBeenCalledTimes(1);
    } finally {
      document.removeEventListener('keydown', heard);
    }
  });

  it('a text that fails to load says why', async () => {
    const fetchImpl = vi.fn(async () => new Response('x', { status: 500 }));
    const el = viewer('markdown', new ArtifactStore(fakeSource([]) as never, fetchImpl as never));
    await flush();
    expect(el.querySelector('.viewer-body .error-text')!.textContent).toContain('HTTP 500');
  });

  it('an image from its signed link, renewed once when refused', async () => {
    const el = viewer('image', new ArtifactStore(fakeSource([]) as never));
    await flush();
    let img = el.querySelector<HTMLImageElement>('.viewer-body img')!;
    expect(img.getAttribute('src')).toBe('/artifacts/a?exp=1&sig=s1');
    expect(img.alt).toBe('screen.png');
    await act(async () => void img.dispatchEvent(new Event('error')));
    await flush();
    img = el.querySelector<HTMLImageElement>('.viewer-body img')!;
    expect(img.getAttribute('src')).toBe('/artifacts/a?exp=1&sig=s2');
  });

  it('a click outside it or on its close button closes it, a click inside or a selection ending outside does not', async () => {
    const onClose = vi.fn();
    const el = viewer('image', new ArtifactStore(fakeSource([]) as never), onClose);
    await flush();
    const img = el.querySelector<HTMLElement>('.viewer-body img')!;
    const scrim = el.querySelector<HTMLElement>('.modal-scrim')!;
    const down = (on: HTMLElement) => act(async () => void on.dispatchEvent(new Event('pointerdown', { bubbles: true })));
    await down(img);
    await act(async () => img.click());
    expect(onClose).not.toHaveBeenCalled();
    // A selection started in the panel and let go on the scrim: the browser clicks the scrim.
    await down(img);
    await act(async () => scrim.click());
    expect(onClose).not.toHaveBeenCalled();
    await down(scrim);
    await act(async () => scrim.click());
    expect(onClose).toHaveBeenCalledTimes(1);
    await act(async () => el.querySelector<HTMLButtonElement>('button[aria-label=Close]')!.click());
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});

describe('paths of the working copy', () => {
  it('recognises paths', () => {
    for (const p of ['src/main.rs', 'src/main.rs:42', 'src/a.ts:3:7', './a/b/', '../x.ts', 'main.rs', 'docs/SPEC.md#L10', '/Users/x/y.txt', 'crates/hqc-hub']) {
      expect(filePath(p), p).toBe(p);
    }
    for (const p of ['https://x.io/a.rs', 'npm run build', 'foo.bar', 'self.method', '1/2', '//host/x', '', 'a//b', '.env', 'mailto:a@b.c']) {
      expect(filePath(p), p).toBeNull();
    }
    expect(filePath('/api/me', true)).toBeNull();
    expect(filePath('src/main.rs', true)).toBe('src/main.rs');
    expect(diffClass('+x')).toBe('diff-add');
    expect(diffClass(' x')).toBeUndefined();
  });

  it('a path in inline code or a relative link copies itself and says so', async () => {
    const writeText = vi.fn(async (_: string) => {});
    vi.stubGlobal('isSecureContext', true);
    Object.defineProperty(window.navigator, 'clipboard', { value: { writeText }, configurable: true });
    const el = mount(<Markdown text={'See `src/main.rs:42`, [the spec](docs/SPEC.md) and `cargo test`.'} />);
    const paths = [...el.querySelectorAll<HTMLButtonElement>('.md-path')];
    expect(paths.map((p) => p.textContent)).toEqual(['src/main.rs:42', 'the spec']);
    expect(el.querySelectorAll('code')).toHaveLength(2);
    await act(async () => paths[0].click());
    await flush();
    expect(writeText).toHaveBeenLastCalledWith('src/main.rs:42');
    expect(paths[0].querySelector('.md-path-note')!.textContent).toBe('path copied');
    await act(async () => paths[1].click());
    expect(writeText).toHaveBeenLastCalledWith('docs/SPEC.md');
    // No link to the hub is made of it.
    expect(el.querySelector('a')).toBeNull();
  });

  it('a diff code block in a message is coloured', () => {
    const el = mount(<Markdown text={'```diff\n-a\n+b\n```'} />);
    expect(el.querySelector('.md-code-diff .diff-del')!.textContent).toBe('-a\n');
    expect(el.querySelector('.md-code-diff .diff-add')!.textContent).toBe('+b');
  });

  it('only a pair of --- and +++ lines is a file header; one alone is a removed or added line', () => {
    const el = mount(<Markdown text={'```diff\n--- a/x.md\n+++ b/x.md\n@@ -1,2 +1,1 @@\n----\n-title\n+++ fast\n```'} />);
    expect([...el.querySelectorAll('.diff-file')].map((l) => l.textContent)).toEqual(['--- a/x.md\n', '+++ b/x.md\n']);
    expect([...el.querySelectorAll('.diff-del')].map((l) => l.textContent)).toEqual(['----\n', '-title\n']);
    expect([...el.querySelectorAll('.diff-add')].map((l) => l.textContent)).toEqual(['+++ fast']);
  });
});
