import { createContext, useCallback, useContext, useEffect, useId, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { writeClipboardText } from "../lib/clipboard";

const ClipboardContext = createContext<((text: string) => Promise<boolean>) | null>(null);
export function useCopyText() {
  const copy = useContext(ClipboardContext);
  if (!copy) throw new Error("ClipboardProvider is required");
  return copy;
}

/** One visible result for every copy entry point, including manual recovery. */
export function ClipboardProvider({ children }: { children: ReactNode }) {
  const { t } = useTranslation();
  const [feedback, setFeedback] = useState<{ text: string; copied: boolean; attempt: number }>();
  const attempt = useRef(0);
  const dialog = useRef<HTMLDialogElement>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  const titleId = useId();
  const hintId = useId();
  useEffect(() => () => { attempt.current++; }, []);
  const copy = useCallback(async (text: string) => {
    const current = ++attempt.current;
    const copied = await writeClipboardText(text);
    if (current === attempt.current) setFeedback({ text: copied ? "" : text, copied, attempt: current });
    return copied;
  }, []);
  useEffect(() => {
    if (!feedback) return;
    if (feedback.copied) {
      const timer = window.setTimeout(() => setFeedback(undefined), 2500);
      return () => window.clearTimeout(timer);
    }
    dialog.current?.showModal();
    field.current?.focus();
    field.current?.select();
  }, [feedback]);
  return <ClipboardContext.Provider value={copy}>
    {children}
    {feedback?.copied ? <div className="clipboard-toast" role="status">{t("clipboard.copied")}</div> : null}
    {feedback && !feedback.copied ? <dialog ref={dialog} className="clipboard-dialog" aria-labelledby={titleId} aria-describedby={hintId}
      onClose={() => setFeedback(undefined)}>
      <h2 id={titleId}>{t("clipboard.failed")}</h2>
      <p id={hintId}>{t("clipboard.manual")}</p>
      <textarea ref={field} readOnly value={feedback.text} aria-label={t("clipboard.content")} onFocus={(event) => event.currentTarget.select()} />
      <form method="dialog"><button type="submit">{t("clipboard.close")}</button></form>
    </dialog> : null}
  </ClipboardContext.Provider>;
}
