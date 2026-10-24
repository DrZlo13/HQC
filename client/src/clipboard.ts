// Copy to the clipboard: the async Clipboard API where the browser gives it (secure
// context), otherwise a hidden textarea and execCommand('copy') — the dev client is often
// opened over plain http on a LAN address, where navigator.clipboard is undefined.

/** Copies `text` as is. `doc` is the document of the element that was clicked. */
export async function copyText(text: string, doc: Document = document): Promise<void> {
  const win = doc.defaultView;
  const clipboard = win?.isSecureContext !== false ? win?.navigator.clipboard : undefined;
  if (clipboard?.writeText) {
    try {
      await clipboard.writeText(text);
      return;
    } catch {
      // Denied permission or no focus: try the old way.
    }
  }
  copyWithTextarea(text, doc);
}

function copyWithTextarea(text: string, doc: Document) {
  const area = doc.createElement('textarea');
  area.value = text;
  area.setAttribute('readonly', '');
  area.setAttribute('aria-hidden', 'true');
  area.className = 'copy-buffer';
  const sel = doc.getSelection();
  const saved = sel && sel.rangeCount > 0 ? sel.getRangeAt(0) : null;
  doc.body.appendChild(area);
  let ok = false;
  try {
    area.select();
    area.setSelectionRange(0, text.length); // iOS ignores select()
    ok = doc.execCommand('copy');
  } finally {
    area.remove();
    if (saved && sel) {
      sel.removeAllRanges();
      sel.addRange(saved);
    }
  }
  if (!ok) throw new Error('clipboard unavailable');
}
