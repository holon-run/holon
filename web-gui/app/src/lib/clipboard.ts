/** Keep the legacy path inside the click gesture when the async API is absent (HTTP). */
export async function writeClipboardText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch { /* Permission denied: try the selection-based fallback. */ }

  const active = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const input = active instanceof HTMLInputElement || active instanceof HTMLTextAreaElement ? active : null;
  const start = input?.selectionStart;
  const end = input?.selectionEnd;
  const direction = input?.selectionDirection;
  const selection = window.getSelection();
  const ranges = selection ? Array.from({ length: selection.rangeCount }, (_, i) => selection.getRangeAt(i).cloneRange()) : [];
  const field = document.createElement("textarea");
  field.value = text;
  field.readOnly = true;
  field.tabIndex = -1;
  field.style.cssText = "position:fixed;left:-9999px;top:0;opacity:0;font-size:16px";
  // A modal makes body siblings inert; keep the fallback inside the active dialog.
  const host = active?.closest("dialog[open]") ?? document.body;
  try {
    host.appendChild(field);
    field.focus({ preventScroll: true });
    field.select();
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    field.remove();
    active?.focus({ preventScroll: true });
    if (selection) {
      selection.removeAllRanges();
      for (const range of ranges) selection.addRange(range);
    }
    if (input && start != null && end != null) input.setSelectionRange(start, end, direction ?? undefined);
  }
}
