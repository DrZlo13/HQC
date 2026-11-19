// A session's model, in the chat's "More" menu (a new session chooses it on a chip). The field
// reports a value once it is chosen (a typed name on `change`); `undefined` is the runtime's
// default. A provider the worker allows (SPEC 0.144) is one more choice.

import { useEffect, useState } from 'preact/hooks';
import type { ModelProvider } from '../proto';
import type { MenuEntry } from './ContextMenu';
import { modelName } from './labels';

/** The `<option>` value of a provider: its id after this prefix. */
const PROVIDER = 'provider:';

/** The model aliases Claude Code takes; any other name is typed in. */
export const MODEL_ALIASES = ['opus', 'sonnet', 'haiku', 'fable'] as const;

/** The versions the menus list (SPEC 0.168), newest first in each family: Claude Code's own
 *  catalog gives a subscription only the aliases. */
export const MODEL_VERSIONS = [
  'claude-opus-5-5',
  'claude-opus-5',
  'claude-opus-4-8',
  'claude-opus-4-7',
  'claude-opus-4-6',
  'claude-sonnet-5-5',
  'claude-sonnet-5',
  'claude-sonnet-4-6',
  'claude-haiku-5-5',
  'claude-haiku-4-5',
  'claude-fable-5-2',
  'claude-fable-5-1',
  'claude-fable-5',
] as const;

/** A name typed in: neither Claude Code's default nor an alias or a version the menus list. */
export const isTypedModel = (m: string | undefined): boolean =>
  m !== undefined && !(MODEL_ALIASES as readonly string[]).includes(m) && !(MODEL_VERSIONS as readonly string[]).includes(m);

/** What a model menu picks: a Claude model (`undefined`: Claude Code's default) or a provider. */
export type ModelChoice = { model: string | undefined } | { provider: string };

/** The items of a model menu, the one now (`model`, `provider`) checked: Claude Code's default
 *  (left out with `withDefault: false`), the aliases, the versions, "Another model…" (`onAnother`:
 *  the menu asks for the name next), then `providers`. */
export function modelMenuItems({
  model,
  provider,
  providers,
  withDefault = true,
  onPick,
  onAnother,
}: {
  model?: string;
  provider?: string;
  providers: ModelProvider[];
  withDefault?: boolean;
  onPick: (choice: ModelChoice) => void;
  onAnother: () => void;
}): MenuEntry[] {
  const claude = !provider;
  const named = (m: string) => ({
    label: modelName(m),
    cls: `btn-model-${m}`,
    checked: claude && model === m,
    onSelect: () => onPick({ model: m }),
  });
  const items: MenuEntry[] = [
    ...(withDefault
      ? [
          {
            label: "Claude Code's default",
            cls: 'btn-model-default',
            checked: claude && model === undefined,
            onSelect: () => onPick({ model: undefined }),
          },
        ]
      : []),
    ...MODEL_ALIASES.map(named),
    'sep',
    ...MODEL_VERSIONS.map(named),
    'sep',
    // Another model waits for its name, in the menu.
    {
      label: 'Another model…',
      cls: 'btn-model-custom',
      checked: claude && isTypedModel(model),
      keepOpen: true,
      onSelect: onAnother,
    },
  ];
  if (providers.length) {
    items.push('sep');
    for (const p of providers) {
      items.push({
        label: p.label,
        detail: p.model,
        cls: 'btn-model-provider',
        checked: provider === p.id,
        onSelect: () => onPick({ provider: p.id }),
      });
    }
  }
  return items;
}

/** The model of a session: Claude Code's default, an alias, a version (SPEC 0.168), a typed
 *  name, or (SPEC 0.144) one of `providers`, the ones the session's worker allows; `provider` is
 *  the one it runs on. */
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
  const typed = !provider && isTypedModel(value);
  const [custom, setCustom] = useState(typed);
  const [name, setName] = useState(typed ? value : '');
  useEffect(() => {
    const t = !provider && isTypedModel(value);
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
          <optgroup label="Versions">
            {MODEL_VERSIONS.map((m) => (
              <option key={m} value={m}>
                {modelName(m)}
              </option>
            ))}
          </optgroup>
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
            placeholder="claude-sonnet-4-5"
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
