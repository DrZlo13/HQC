import { useEffect, useState } from 'preact/hooks';
import { IconPlus } from './icons';

/** A list of quick messages in the order it is kept, with a form to add one and to edit one in
 *  place: the project's list in project settings, the user's own in Settings. `list` is the one on
 *  show; `read` reads the hub's list and returns it (and, where the page has it, puts it where
 *  `list` comes from); a save works on what `read` returns, not on `list`. `store` sends a whole
 *  list. Every save reads the hub's list first and sends it whole with the one change made to it,
 *  so what another device added meanwhile stays. */
export function QuickMessagesEditor({
  id,
  list,
  read,
  store,
}: {
  id: string;
  list: string[];
  read: () => Promise<string[]>;
  store: (list: string[]) => Promise<void>;
}) {
  // The open form: `from` is the text an edit started from, `null` for a new message.
  const [editing, setEditing] = useState<{ from: string | null } | null>(null);
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    read().catch(() => {});
  }, [id]);

  const open = (from: string | null) => {
    setEditing({ from });
    setDraft(from ?? '');
    setError(null);
  };

  /** `change` makes the new list out of the hub's current one; `null`: nothing to send. */
  const save = async (change: (fresh: string[]) => string[] | null) => {
    setBusy(true);
    setError(null);
    try {
      const next = change(await read());
      if (next) await store(next);
      setEditing(null);
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  const submit = (e: Event) => {
    e.preventDefault();
    if (!editing) return;
    const { from } = editing;
    save((fresh) => {
      if (from === null) return [...fresh, draft];
      // The message the edit started from, wherever it is now; gone: the text is added.
      const at = fresh.indexOf(from);
      return at < 0 ? [...fresh, draft] : fresh.map((m, i) => (i === at ? draft : m));
    });
  };

  const remove = (text: string) =>
    save((fresh) => {
      const at = fresh.indexOf(text);
      return at < 0 ? null : fresh.filter((_, i) => i !== at);
    });

  const form = (
    <form class="add-workspace quick-form" onSubmit={submit}>
      <textarea
        class="input quick-input"
        name="quick-message"
        rows={4}
        aria-label="Quick message"
        value={draft}
        onInput={(e) => setDraft((e.target as HTMLTextAreaElement).value)}
      />
      <div class="row">
        <button type="submit" class="btn btn-small btn-primary btn-quick-save" disabled={busy || !draft.trim()}>
          Save
        </button>
        <button
          type="button"
          class="btn btn-small btn-quick-cancel"
          disabled={busy}
          onClick={() => {
            setEditing(null);
            setError(null);
          }}
        >
          Cancel
        </button>
      </div>
    </form>
  );
  // An open form holds the rest: a delete or another edit would close it and lose its text.
  const held = busy || editing !== null;
  // The row an edit stands in place of, found by its text as the save finds it: the list may
  // change under the form (another device). Gone from the list: the form stands at the end.
  const at = editing?.from != null ? list.indexOf(editing.from) : -1;

  return (
    <>
      {list.map((m, i) =>
        i === at ? (
          <div key={i}>{form}</div>
        ) : (
          <div key={i} class="quick-row ps-card">
            <div class="quick-text">{m}</div>
            <div class="row ps-buttons">
              <button type="button" class="btn btn-small btn-quick-edit" disabled={held} onClick={() => open(m)}>
                Edit
              </button>
              <button type="button" class="btn btn-small btn-ghost btn-quick-delete" disabled={held} onClick={() => remove(m)}>
                Delete
              </button>
            </div>
          </div>
        ),
      )}
      {error && <div class="error-text">{error}</div>}
      {editing && at < 0 ? (
        form
      ) : (
        <div class="row ps-buttons">
          <button type="button" class="btn btn-primary btn-quick-add" disabled={held} onClick={() => open(null)}>
            <IconPlus size={15} />
            Add a quick message
          </button>
        </div>
      )}
    </>
  );
}
