// A session's model, in the chat's "More" menu (a new session chooses it on a chip). The field
// reports a value once it is chosen (a typed name on `change`); `undefined` is the runtime's
// default. A provider the worker allows (SPEC 0.144) is one more choice.

import { useEffect, useState } from 'preact/hooks';
import type { ModelProvider } from '../proto';

/** The `<option>` value of a provider: its id after this prefix. */
const PROVIDER = 'provider:';

/** The model aliases Claude Code takes; any other name is typed in. */
export const MODEL_ALIASES = ['opus', 'sonnet', 'haiku', 'fable'] as const;

const isAlias = (m: string) => (MODEL_ALIASES as readonly string[]).includes(m);

/** The model of a session: Claude Code's default, an alias, a typed name, or (SPEC 0.144) one
 *  of `providers`, the ones the session's worker allows; `provider` is the one it runs on. */
export function ModelField({
  value,
  onChange,
  providers = [],
  provider,
  onProvider,
}: {
  value?: string;
  onChange: (model: string | undefined) => void;
  providers?: ModelProvider[];
  provider?: string;
  onProvider?: (id: string) => void;
}) {
  const typed = !provider && value !== undefined && !isAlias(value);
  const [custom, setCustom] = useState(typed);
  const [name, setName] = useState(typed ? value : '');
  useEffect(() => {
    const t = !provider && value !== undefined && !isAlias(value);
    setCustom(t);
    setName(t ? value : '');
  }, [value, provider]);
  return (
    <>
      <label class="field">
        <span class="field-label">Model</span>
        <select
          class="input"
          name="model"
          value={provider ? `${PROVIDER}${provider}` : custom ? 'custom' : (value ?? '')}
          onChange={(e) => {
            const v = (e.target as HTMLSelectElement).value;
            if (v.startsWith(PROVIDER)) return onProvider?.(v.slice(PROVIDER.length));
            // Another model waits for its name.
            setCustom(v === 'custom');
            if (v !== 'custom') onChange(v || undefined);
          }}
        >
          <option value="">Claude Code's default</option>
          <option value="opus">Opus</option>
          <option value="sonnet">Sonnet</option>
          <option value="haiku">Haiku</option>
          <option value="fable">Fable</option>
          <option value="custom">Another model…</option>
          {providers.length > 0 && (
            <optgroup label="Providers">
              {providers.map((p) => (
                <option key={p.id} value={`${PROVIDER}${p.id}`}>
                  {p.label}
                </option>
              ))}
            </optgroup>
          )}
        </select>
      </label>
      {custom && (
        <label class="field">
          <span class="field-label">Model name</span>
          <input
            class="input mono"
            name="model_name"
            placeholder="claude-sonnet-5-5"
            value={name}
            onInput={(e) => setName((e.target as HTMLInputElement).value)}
            onChange={(e) => {
              const m = (e.target as HTMLInputElement).value.trim();
              if (m && m !== value) onChange(m);
            }}
          />
        </label>
      )}
    </>
  );
}
