// "Models" of Settings (SPEC 0.144): the owner's model providers, an Anthropic-compatible
// endpoint with one model each, which a session picks in its model menu. The hub keeps these
// descriptions; a worker runs one only if its worker.toml allows the name with the same
// base_url, and keeps the key, so each row says which workers allow it and what to add.

import { useState } from 'preact/hooks';
import type { ProviderFields } from '../api';
import type { AppState } from '../app';
import type { ModelProvider } from '../proto';

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

function ProviderForm({
  initial,
  onSave,
  onCancel,
}: {
  initial: ProviderFields;
  onSave: (f: ProviderFields) => void;
  onCancel: () => void;
}) {
  const [f, setF] = useState<ProviderFields>(initial);
  const field = (key: 'name' | 'label' | 'base_url' | 'model', label: string, placeholder: string) => (
    <label class="field">
      <span class="field-label">{label}</span>
      <input
        class={`input ${key === 'label' ? '' : 'mono'}`}
        name={`provider_${key}`}
        placeholder={placeholder}
        value={f[key]}
        onInput={(e) => {
          const v = (e.currentTarget as HTMLInputElement).value;
          // From the state at the time: browser autofill sets several fields before a render.
          setF((cur) => ({ ...cur, [key]: v }));
        }}
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
      {field('label', 'Label', 'Qwen 3.8 (local)')}
      {field('name', 'Name, as in [providers.<name>]', 'qwen-local')}
      {field('base_url', 'Base URL', 'http://192.168.1.229:8082')}
      {field('model', 'Model', 'qwen3.8-27b')}
      <label class="field">
        <span class="field-label">Context window, tokens (optional)</span>
        <input
          class="input mono"
          name="provider_context_tokens"
          inputMode="numeric"
          placeholder="200000"
          value={f.context_tokens ?? ''}
          onInput={(e) => {
            const v = (e.currentTarget as HTMLInputElement).value.trim();
            setF((cur) => ({ ...cur, context_tokens: v ? Number(v) : undefined }));
          }}
        />
      </label>
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
