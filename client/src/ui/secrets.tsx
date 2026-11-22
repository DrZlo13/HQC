// Secrets and ssh keys for a session's agent (SPEC 0.147, docs/ITERATION-6.md T59): the form
// of the composer's "Secret" and of the agent's `hqc_secret` card, and a workspace's
// remembered keys. A value lives only in the form's state while it is open: never in the
// draft, the feed or any storage; the agent is told only `note`, the path or the fingerprint.

import { useEffect, useRef, useState } from 'preact/hooks';
import type { SecretFields, SecretGiven, WorkspaceKey } from '../api';
import type { SecretAsked } from '../proto';
import { useAction } from './hooks';
import { IconKey } from './icons';
import { Loading } from './Loading';

/** A name as the worker takes it: letters, digits, `_`, `-` and `.`, not starting with `.`
 *  or `-`, 1 to 64 characters. */
export function secretNameError(name: string): string | null {
  if (!name) return 'A name is required';
  if (name.length > 64) return 'At most 64 characters';
  if (/^[.-]/.test(name)) return 'Not starting with . or -';
  if (!/^[A-Za-z0-9_.-]+$/.test(name)) return 'Letters, digits, _, - and . only';
  return null;
}

/** The fields of a secret. `asked`: the agent's `hqc_secret`, whose name and kind are fixed.
 *  `onGive` resolves once the worker has it; its error stays on the form, which keeps what was
 *  typed until it is closed. */
export function SecretForm({
  asked,
  onGive,
  onCancel,
  giveLabel = 'Give',
  cancelLabel = 'Cancel',
  escapeCancels = true,
}: {
  asked?: SecretAsked;
  onGive: (f: SecretFields) => Promise<void>;
  onCancel: () => void;
  giveLabel?: string;
  cancelLabel?: string;
  /** Esc means Cancel (the composer's form); not where Cancel declines an agent's request. */
  escapeCancels?: boolean;
}) {
  const [kind, setKind] = useState<SecretFields['kind']>(asked?.kind ?? 'secret');
  const [name, setName] = useState(asked?.name ?? '');
  const [value, setValue] = useState('');
  const [passphrase, setPassphrase] = useState('');
  const [remember, setRemember] = useState(false);
  const { busy, error, run } = useAction();
  const first = useRef<HTMLInputElement>(null);
  const valueField = useRef<HTMLInputElement & HTMLTextAreaElement>(null);
  useEffect(() => {
    (asked ? valueField : first).current?.focus();
  }, []);
  const key = kind === 'ssh_key';
  const nameError = name ? secretNameError(name) : null;
  const canGive = !busy && !secretNameError(name) && value.trim() !== '';
  const give = () => {
    if (!canGive) return;
    const f: SecretFields = { kind, name, value };
    if (key) {
      if (passphrase) f.passphrase = passphrase;
      if (remember) f.remember = true;
    }
    run(() => onGive(f));
  };
  return (
    <form
      class="secret-form"
      aria-label={asked ? `Give ${asked.name}` : 'Secret for the agent'}
      autocomplete="off"
      onSubmit={(e) => {
        e.preventDefault();
        give();
      }}
      onKeyDown={(e) => {
        if (e.key === 'Escape' && escapeCancels) {
          e.preventDefault();
          e.stopPropagation();
          onCancel();
        }
      }}
    >
      {!asked && (
        <div class="secret-row">
          <div class="seg-group secret-kind" role="radiogroup" aria-label="Kind">
            {(
              [
                ['secret', 'Password'],
                ['ssh_key', 'SSH key'],
              ] as const
            ).map(([k, label]) => (
              <button
                key={k}
                type="button"
                role="radio"
                aria-checked={kind === k}
                class={`seg ${kind === k ? "is-on" : ""}`}
                disabled={busy}
                onClick={() => setKind(k)}
              >
                {label}
              </button>
            ))}
          </div>
          <input
            ref={first}
            class={`input mono secret-name ${nameError ? 'is-invalid' : ''}`}
            aria-label="Name"
            placeholder={key ? 'pi-lab' : 'WIFI_PASS'}
            value={name}
            disabled={busy}
            autocapitalize="off"
            spellcheck={false}
            onInput={(e) => setName((e.currentTarget as HTMLInputElement).value.trim())}
          />
        </div>
      )}
      {nameError && <div class="error-text small">{nameError}</div>}
      {key ? (
        <textarea
          ref={valueField}
          class="input mono secret-key"
          aria-label="Private key"
          placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
          rows={4}
          value={value}
          disabled={busy}
          autocomplete="off"
          autocapitalize="off"
          spellcheck={false}
          onInput={(e) => setValue((e.currentTarget as HTMLTextAreaElement).value)}
        />
      ) : (
        <input
          ref={valueField}
          type="password"
          class="input secret-value"
          aria-label="Password"
          // The name a password manager fills (1Password, the iOS keyboard).
          name="password"
          autocomplete="current-password"
          placeholder="Password"
          value={value}
          disabled={busy}
          onInput={(e) => setValue((e.currentTarget as HTMLInputElement).value)}
        />
      )}
      {key && (
        <>
          <input
            type="password"
            class="input secret-passphrase"
            aria-label="Passphrase"
            autocomplete="off"
            placeholder="Passphrase, if the key has one"
            value={passphrase}
            disabled={busy}
            onInput={(e) => setPassphrase((e.currentTarget as HTMLInputElement).value)}
          />
          <label class="secret-remember">
            <input
              type="checkbox"
              checked={remember}
              disabled={busy}
              onChange={(e) => setRemember((e.currentTarget as HTMLInputElement).checked)}
            />
            Remember for this workspace
          </label>
        </>
      )}
      <div class="secret-hint muted small">
        {key
          ? 'The key goes into the agent’s ssh-agent; the agent never sees it.'
          : 'The agent gets a file with it, not the value: it stays out of the chat and the transcript.'}
      </div>
      {error && <div class="error-text">{error}</div>}
      <div class="secret-actions">
        <button type="submit" class="btn btn-primary btn-give" disabled={!canGive}>
          {giveLabel}
        </button>
        <button type="button" class="btn btn-ghost" disabled={busy} onClick={onCancel}>
          {cancelLabel}
        </button>
      </div>
    </form>
  );
}

/** The composer's "Secret": the form over the field; `onNote` gets what the agent should be
 *  told, for the message being written. */
export function ComposerSecret({
  give,
  onNote,
  onClose,
}: {
  give: (f: SecretFields) => Promise<SecretGiven>;
  onNote: (note: string) => void;
  onClose: () => void;
}) {
  return (
    <div class="composer-secret">
      <div class="composer-secret-title">
        <IconKey size={15} /> Secret for the agent
      </div>
      <SecretForm
        onCancel={onClose}
        onGive={async (f) => {
          const r = await give(f);
          onNote(r.note);
          onClose();
        }}
      />
    </div>
  );
}

/** The ssh keys a workspace's worker keeps for it (SPEC 0.147), with Delete. */
export function WorkspaceKeys({
  load,
  remove,
}: {
  load: () => Promise<WorkspaceKey[]>;
  remove: (name: string) => Promise<void>;
}) {
  const [keys, setKeys] = useState<WorkspaceKey[] | null | undefined>(undefined);
  const [failed, setFailed] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<string | null>(null);
  const { busy, error, run } = useAction();
  const reload = () =>
    load().then(
      (k) => {
        setKeys(k);
        setFailed(null);
      },
      (e: Error) => {
        setKeys(null);
        setFailed(e.message);
      },
    );
  useEffect(() => {
    void reload();
  }, []);
  return (
    <section class="ws-keys" aria-label="SSH keys">
      <h3>SSH keys</h3>
      <p class="muted small">
        Keys remembered for this workspace: every session here gets them in its ssh-agent. Added from a chat with
        “Remember for this workspace” or made by the agent with hqc_ssh_keygen.
      </p>
      {keys === undefined && <Loading />}
      {keys === null && <div class="muted ws-keys-off">Not available: {failed}</div>}
      {keys && keys.length === 0 && <div class="muted ws-keys-none">No keys</div>}
      {keys && keys.length > 0 && (
        <ul class="ws-keys-list">
          {keys.map((k) => (
            <li key={k.name} class="ws-key">
              <span class="ws-key-name mono">{k.name}</span>
              <span class="ws-key-fp mono muted">{k.fingerprint}</span>
              {confirm === k.name ? (
                <span class="ws-key-confirm">
                  <button
                    type="button"
                    class="btn btn-danger btn-small"
                    disabled={busy}
                    onClick={() =>
                      run(async () => {
                        await remove(k.name);
                        setConfirm(null);
                        await reload();
                      })
                    }
                  >
                    Delete {k.name}
                  </button>
                  <button type="button" class="btn btn-ghost btn-small" disabled={busy} onClick={() => setConfirm(null)}>
                    Keep
                  </button>
                </span>
              ) : (
                <button
                  type="button"
                  class="btn btn-ghost btn-small"
                  aria-label={`Delete ${k.name}`}
                  onClick={() => setConfirm(k.name)}
                >
                  Delete
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
      {error && <div class="error-text">{error}</div>}
    </section>
  );
}
