import { useEffect, useState } from "react";
import { useRuntimeStore } from "../../runtime/runtime-store";
import type { WorkspaceFileIdentity } from "../../runtime/types";
import { filePreviewLocation, filePreviewUrl, fragmentOf, type FileTarget } from "../../components/file-references/references";
import { useFileIdentity } from "../../components/file-references/use-references";
import { FileBrowserPanel } from "./FileBrowserPanel";

/** Standalone, authenticated Explorer. Its location is independent of the active agent. */
export function FilePreviewPage() {
  const identity = useFileIdentity();
  const [url, setUrl] = useState(() => window.location.href);
  const [attempt, setAttempt] = useState(0);
  const [state, setState] = useState<{ key: string; file?: WorkspaceFileIdentity; error?: string }>();
  const key = JSON.stringify([identity, url]);
  useEffect(() => { setUrl(window.location.href); }, [identity]);
  useEffect(() => {
    const changed = () => setUrl(window.location.href);
    window.addEventListener("popstate", changed);
    window.addEventListener("hashchange", changed);
    return () => { window.removeEventListener("popstate", changed); window.removeEventListener("hashchange", changed); };
  }, []);
  useEffect(() => {
    let cancelled = false;
    const locator = filePreviewLocation(new URL(url).search);
    if (!locator) { setState({ key, error: "Invalid file link: workspace, root and path are required" }); return; }
    void useRuntimeStore.getState().fetchWorkspacePath(locator).then((file) => {
      if (!cancelled) setState({ key, file });
    }).catch((cause: unknown) => {
      if (!cancelled) setState({ key, error: cause instanceof Error ? cause.message : "File unavailable" });
    });
    return () => { cancelled = true; };
  }, [key, attempt]);
  const open = (target: FileTarget) => {
    window.history.pushState(null, "", filePreviewUrl(target, target.fragment));
    setUrl(window.location.href);
  };
  const current = state?.key === key ? state : undefined;
  let fragment: string | undefined;
  try { fragment = fragmentOf(url); } catch { /* malformed fragment cannot change the file locator */ }
  return <main className="file-preview-page">
    <header><a href="/">Holon</a><span>File preview</span></header>
    {current?.error ? <p role="alert">{current.error} <button type="button" onClick={() => setAttempt((value) => value + 1)}>Retry</button></p> : current?.file ?
      <FileBrowserPanel key={key} workspaceId={current.file.workspaceId} executionRootId={current.file.executionRootId}
        initialPath={current.file.kind === "directory" ? current.file.path : undefined}
        initialFilePath={current.file.kind === "file" ? current.file.path : undefined} initialFragment={fragment} onOpenFile={open}
        onSnapshot={(snapshot) => {
          if (!snapshot.listing || snapshot.selectedFile?.loading || snapshot.selectedFile?.error) return;
          const location = { workspaceId: snapshot.listing.workspaceId, executionRootId: snapshot.listing.executionRootId,
            path: snapshot.selectedFile?.path ?? snapshot.currentPath };
          // Browsing within Explorer keeps its history/scroll state while refresh restores this location.
          window.history.replaceState(null, "", filePreviewUrl(location, snapshot.selectedFile ? snapshot.fragment : undefined));
        }} />
      : <p role="status">Loading file…</p>}
  </main>;
}
