// "Models" of Settings (SPEC 0.144): the owner's model providers, an Anthropic-compatible
// endpoint with one model each, which a session picks in its model menu. The hub keeps these
// descriptions; a worker runs one only if its worker.toml allows the name with the same
// base_url, and keeps the key, so each row says which workers allow it and what to add.

import { useMemo, useRef, useState } from 'preact/hooks';
import { ApiError, type ProviderFields, type ProviderProbe } from '../api';
import type { AppState } from '../app';
import type { ModelProvider } from '../proto';
import { useUrlProbe } from './urlprobe';

const EMPTY: ProviderFields = { name: '', label: '', base_url: '', model: '' };

export function ProvidersSection({ app }: { app: AppState }) {
  // The provider being edited: its id, `new` for one being added, or none.
  const [editing, setEditing] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const act = async (fn: () => Promise<void>) => {
    setError(null);
    try {
      await fn();
    } catch (e) {
      setError((e as Error).message);
    }
  };
  const remove = (p: ModelProvider) => {
    if (!confirm(`Delete the provider “${p.label}”?`)) return;
    void act(() => app.deleteProvider(p.id));
  };
  return (
    <>
      <h2>Models</h2>
      <p class="muted">
        Providers: an Anthropic-compatible endpoint with one model, picked in a session's model menu. A worker offers one only when its
        worker.toml allows it with the same base_url; a key, if the endpoint needs one, goes there too and never to the hub.
      </p>
      {app.providers.length > 0 && (
        <ul class="plain-list provider-list">
          {app.providers.map((p) =>
            editing === p.id ? (
              <li key={p.id}>
                <ProviderForm
                  app={app}
                  initial={p}
                  onCancel={() => setEditing(null)}
                  onSave={(f) => act(async () => {
                    await app.saveProvider(p.id, f);
                    setEditing(null);
                  })}
                />
              </li>
            ) : (
              <li key={p.id} class="provider-row">
                <span class="provider-label">{p.label}</span>
                <span class="provider-meta mono">
                  {p.model} · {p.base_url}
                </span>
                <span class="provider-workers">
                  {p.workers.length ? (
                    `on ${p.workers.map((w) => app.workerName(w) ?? w).join(', ')}`
                  ) : (
                    <>
                      no worker allows it yet: add to a worker.toml{' '}
                      <code class="mono">
                        [providers.{p.name}] base_url = "{p.base_url}"
                      </code>
                    </>
                  )}
                </span>
                <button type="button" class="btn btn-small btn-edit-provider" onClick={() => setEditing(p.id)}>
                  Edit
                </button>
                <button type="button" class="btn btn-small btn-revoke btn-delete-provider" onClick={() => remove(p)}>
                  Delete
                </button>
              </li>
            ),
          )}
        </ul>
      )}
      {editing === 'new' ? (
        <ProviderForm
          app={app}
          initial={EMPTY}
          onCancel={() => setEditing(null)}
          onSave={(f) => act(async () => {
            await app.saveProvider(undefined, f);
            setEditing(null);
          })}
        />
      ) : (
        <button type="button" class="btn btn-add-provider" onClick={() => setEditing('new')}>
          Add a provider
        </button>
      )}
      {error && <div class="error-text">{error}</div>}
    </>
  );
}

type Key = 'name' | 'label' | 'base_url' | 'model' | 'context_tokens';

/** A provider name from a model id: `Qwen3.8 27B` -> `qwen3-8-27b`. */
export function providerNameOf(model: string): string {
  return model
    .toLowerCase()
    .replace(/[^a-z0-9_-]/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 64)
    .replace(/-+$/, '');
}

type Probe = { state: 'busy' } | { state: 'done'; text: string; error?: boolean };

/** The probe's answer for the line under the base URL. */
function probeText(r: ProviderProbe): Probe {
  if (r.models.length) return { state: 'done', text: r.models.length === 1 ? '1 model' : `${r.models.length} models` };
  return { state: 'done', text: r.error ?? 'No models listed', error: true };
}

function ProviderForm({
  app,
  initial,
  onSave,
  onCancel,
}: {
  app: AppState;
  initial: ProviderFields;
  onSave: (f: ProviderFields) => void;
  onCancel: () => void;
}) {
  const [f, setF] = useState<ProviderFields>(initial);
  const [models, setModels] = useState<ProviderProbe['models']>([]);
  const [probe, setProbe] = useState<Probe | null>(null);
  // The fields the owner typed in: a probe fills only the others. An edit starts with all.
  const touched = useRef(new Set<Key>(initial === EMPTY ? [] : ['name', 'label', 'base_url', 'model', 'context_tokens']));
  const listId = useMemo(() => `provider-models-${Math.random().toString(36).slice(2)}`, []);

  /** The fields that follow a model, those not typed in. */
  const follow = (cur: ProviderFields, model: string, list: ProviderProbe['models']): ProviderFields => {
    const next = { ...cur };
    const t = touched.current;
    const m = list.find((x) => x.id === model);
    if (!t.has('context_tokens')) next.context_tokens = m?.context_tokens;
    if (!t.has('name')) next.name = providerNameOf(model);
    if (!t.has('label')) next.label = model;
    return next;
  };

  const urlProbe = useUrlProbe(f.base_url, (url, current) => {
    setProbe({ state: 'busy' });
    const name = f.name.trim() || undefined;
    // Through a promise: an api without the call (a stub) is a failed check, not a throw.
    Promise.resolve(url).then((x) => app.api.probeProvider(x, name)).then(
      (r) => {
        if (!current()) return;
        setModels(r.models);
        setProbe(probeText(r));
        if (!touched.current.has('model') && r.models.length) {
          const model = r.models[0].id;
          setF((cur) => follow({ ...cur, model }, model, r.models));
        }
      },
      (e) => {
        if (!current()) return;
        setModels([]);
        setProbe({
          state: 'done',
          error: true,
          text: e instanceof ApiError && e.code === 'worker_offline' ? 'No worker is online to read it' : (e as Error).message,
        });
      },
    );
  });

  const set = (key: Key, v: string) => {
    touched.current.add(key);
    // From the state at the time: browser autofill sets several fields before a render.
    if (key === 'context_tokens') setF((cur) => ({ ...cur, context_tokens: v.trim() ? Number(v.trim()) : undefined }));
    else if (key === 'model') setF((cur) => follow({ ...cur, model: v }, v.trim(), models));
    else setF((cur) => ({ ...cur, [key]: v }));
  };

  const field = (key: 'name' | 'label' | 'base_url' | 'model', label: string, placeholder: string, extra = {}) => (
    <label class="field">
      <span class="field-label">{label}</span>
      <input
        class={`input ${key === 'label' ? '' : 'mono'}`}
        name={`provider_${key}`}
        placeholder={placeholder}
        autocomplete="off"
        value={f[key]}
        onInput={(e) => set(key, (e.currentTarget as HTMLInputElement).value)}
        {...extra}
      />
    </label>
  );
  return (
    <form
      class="provider-form"
      onSubmit={(e) => {
        e.preventDefault();
        onSave(f);
      }}
    >
      {field('base_url', 'Base URL', 'http://192.168.1.229:8082', urlProbe)}
      {probe && (
        <span class={`small provider-probe ${probe.state === 'done' && probe.error ? 'error-text' : 'muted'}`} role="status">
          {probe.state === 'busy' ? 'Checking…' : probe.text}
        </span>
      )}
      {field('model', 'Model', 'qwen3.8-27b', { list: listId })}
      <datalist id={listId}>
        {models.map((m) => (
          <option key={m.id} value={m.id} />
        ))}
      </datalist>
      <label class="field">
        <span class="field-label">Context window, tokens (optional)</span>
        <input
          class="input mono"
          name="provider_context_tokens"
          inputMode="numeric"
          placeholder="200000"
          value={f.context_tokens ?? ''}
          onInput={(e) => set('context_tokens', (e.currentTarget as HTMLInputElement).value)}
        />
      </label>
      {field('name', 'Name, as in [providers.<name>]', 'qwen-local')}
      {field('label', 'Label', 'Qwen 3.8 (local)')}
      <div class="actions">
        <button type="submit" class="btn btn-primary btn-save-provider">
          Save
        </button>
        <button type="button" class="btn btn-ghost" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}
