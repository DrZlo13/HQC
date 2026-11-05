// A session's model and auto-compaction, in the new-session form and the chat's "More" menu.
// A field reports a value once it is chosen (a typed name or window on `change`); `undefined`
// is the runtime's default.

import { useEffect, useState } from 'preact/hooks';

/** The model aliases Claude Code takes; any other name is typed in. */
export const MODEL_ALIASES = ['opus', 'sonnet', 'haiku'] as const;

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

/** Auto-compaction: the runtime's default, off (0), or the context window in tokens it compacts
 *  in, typed in thousands (100–1000). `restarts`: the agent may be running, and a change starts
 *  it again after the current turn. */
export function AutocompactField({
  value,
  onChange,
  restarts,
}: {
  value?: number;
  onChange: (autocompact: number | undefined) => void;
  restarts?: boolean;
}) {
  const [windowed, setWindowed] = useState(!!value);
  const [thousands, setThousands] = useState(value ? String(value / 1000) : '');
  useEffect(() => {
    setWindowed(!!value);
    setThousands(value ? String(value / 1000) : '');
  }, [value]);
  return (
    <>
      <label class="field">
        <span class="field-label">Auto-compaction</span>
        <select
          class="input"
          name="autocompact"
          value={windowed ? 'window' : value === 0 ? 'off' : ''}
          onChange={(e) => {
            const v = (e.target as HTMLSelectElement).value;
            // A window waits for its size.
            setWindowed(v === 'window');
            if (v !== 'window') onChange(v === 'off' ? 0 : undefined);
          }}
        >
          <option value="">Claude Code's default</option>
          <option value="off">Off</option>
          <option value="window">At a context window of…</option>
        </select>
      </label>
      {windowed && (
        <label class="field">
          <span class="field-label">Context window, thousand tokens (100–1000)</span>
          <input
            class="input"
            type="number"
            name="autocompact_thousands"
            min={100}
            max={1000}
            step={1}
            value={thousands}
            onInput={(e) => setThousands((e.target as HTMLInputElement).value)}
            onChange={(e) => {
              const n = Math.round(Number((e.target as HTMLInputElement).value) * 1000);
              if (n > 0 && n !== value) onChange(n);
            }}
          />
        </label>
      )}
      {restarts && <span class="field-label field-hint">A change restarts the agent after the current turn.</span>}
    </>
  );
}
