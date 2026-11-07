// A session's model, in the new-session form and the chat's "More" menu. The field reports a
// value once it is chosen (a typed name on `change`); `undefined` is the runtime's default.

import { useEffect, useState } from 'preact/hooks';

/** The model aliases Claude Code takes; any other name is typed in. */
export const MODEL_ALIASES = ['opus', 'sonnet', 'haiku', 'fable'] as const;

const isAlias = (m: string) => (MODEL_ALIASES as readonly string[]).includes(m);

export function ModelField({ value, onChange }: { value?: string; onChange: (model: string | undefined) => void }) {
  const typed = value !== undefined && !isAlias(value);
  const [custom, setCustom] = useState(typed);
  const [name, setName] = useState(typed ? value : '');
  useEffect(() => {
    const t = value !== undefined && !isAlias(value);
    setCustom(t);
    setName(t ? value : '');
  }, [value]);
  return (
    <>
      <label class="field">
        <span class="field-label">Model</span>
        <select
          class="input"
          name="model"
          value={custom ? 'custom' : (value ?? '')}
          onChange={(e) => {
            const v = (e.target as HTMLSelectElement).value;
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
