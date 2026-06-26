import { useCallback, useEffect, useState } from "react";

import type { WorkspaceDirectoryListing, WorkspaceFileEntry } from "../../runtime/types";
import { useRuntimeStore } from "../../runtime/runtime-store";

interface FileBrowserPanelProps {
  workspaceId: string;
  initialPath?: string;
}

interface SelectedFile {
  path: string;
  content?: string;
  mimeType?: string;
  truncated?: boolean;
  totalSize?: number;
  loading: boolean;
  error?: string;
}

function fileIcon(entry: WorkspaceFileEntry): string {
  if (entry.type === "directory") return "📁";
  if (entry.type === "symlink") return "🔗";
  const ext = entry.name.split(".").pop()?.toLowerCase();
  switch (ext) {
    case "rs": return "🦀";
    case "ts": case "tsx": return "📘";
    case "js": case "jsx": return "📙";
    case "json": return "📋";
    case "md": return "📝";
    case "png": case "jpg": case "jpeg": case "gif": case "svg": case "webp": return "🖼️";
    case "toml": case "yaml": case "yml": return "⚙️";
    default: return "📄";
  }
}

function isTextFile(mimeType?: string, name?: string): boolean {
  if (!mimeType) return false;
  if (mimeType.startsWith("text/")) return true;
  const textTypes = [
    "application/json",
    "application/javascript",
    "application/typescript",
    "application/x-yaml",
    "application/toml",
    "application/x-sh",
  ];
  if (textTypes.some((t) => mimeType.startsWith(t))) return true;
  if (name) {
    const ext = name.split(".").pop()?.toLowerCase();
    return ["rs", "ts", "tsx", "js", "jsx", "json", "md", "toml", "yaml", "yml", "sh", "css", "html", "sql", "py"].includes(ext ?? "");
  }
  return false;
}

function isImageFile(mimeType?: string): boolean {
  return Boolean(mimeType?.startsWith("image/"));
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function FileBrowserPanel({ workspaceId, initialPath }: FileBrowserPanelProps) {
  const browseWorkspaceDir = useRuntimeStore((s) => s.browseWorkspaceDir);
  const readWorkspaceFile = useRuntimeStore((s) => s.readWorkspaceFile);
  const workspaceFileUrl = useRuntimeStore((s) => s.workspaceFileUrl);

  const [currentPath, setCurrentPath] = useState(initialPath ?? "");
  const [listing, setListing] = useState<WorkspaceDirectoryListing | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  const [selectedFile, setSelectedFile] = useState<SelectedFile | null>(null);
  const [showHidden, setShowHidden] = useState(false);

  const loadDir = useCallback(
    async (path: string) => {
      setLoading(true);
      setError(undefined);
      setSelectedFile(null);
      try {
        const result = await browseWorkspaceDir(workspaceId, path || undefined);
        setListing(result);
        setCurrentPath(path);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    },
    [workspaceId, browseWorkspaceDir],
  );

  useEffect(() => {
    void loadDir(initialPath ?? "");
  }, [loadDir, initialPath]);

  const breadcrumbParts = currentPath.split("/").filter(Boolean);

  const navigateToBreadcrumb = (index: number) => {
    const target = breadcrumbParts.slice(0, index + 1).join("/");
    void loadDir(target);
  };

  const openEntry = async (entry: WorkspaceFileEntry) => {
    if (entry.type === "directory") {
      const dirPath = currentPath ? `${currentPath}/${entry.name}` : entry.name;
      void loadDir(dirPath);
      return;
    }

    const filePath = currentPath ? `${currentPath}/${entry.name}` : entry.name;

    if (isImageFile(entry.mimeType)) {
      setSelectedFile({ path: filePath, loading: false, mimeType: entry.mimeType });
      return;
    }

    if (!isTextFile(entry.mimeType, entry.name)) {
      setSelectedFile({ path: filePath, loading: false, mimeType: entry.mimeType });
      return;
    }

    setSelectedFile({ path: filePath, loading: true });
    try {
      const content = await readWorkspaceFile(workspaceId, filePath);
      setSelectedFile({
        path: filePath,
        content: content.content,
        mimeType: content.mimeType,
        truncated: content.truncated,
        totalSize: content.totalSize ?? content.size,
        loading: false,
      });
    } catch (err) {
      setSelectedFile({
        path: filePath,
        loading: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  };

  const entries = listing?.entries ?? [];
  const visibleEntries = showHidden
    ? entries
    : entries.filter((e) => !e.name.startsWith("."));
  const dirs = visibleEntries.filter((e) => e.type === "directory" || e.type === "symlink");
  const files = visibleEntries.filter((e) => e.type === "file");
  const sortedEntries = [...dirs, ...files];

  return (
    <div className="file-browser">
      <div className="file-browser-toolbar">
        <nav className="file-browser-breadcrumb" aria-label="Path breadcrumb">
          <button
            type="button"
            className="file-browser-crumb"
            onClick={() => void loadDir("")}
          >
            root
          </button>
          {breadcrumbParts.map((part, i) => (
            <span key={i} className="file-browser-crumb-group">
              <span className="file-browser-sep">/</span>
              <button
                type="button"
                className="file-browser-crumb"
                onClick={() => navigateToBreadcrumb(i)}
              >
                {part}
              </button>
            </span>
          ))}
        </nav>
        <label className="file-browser-hidden-toggle">
          <input
            type="checkbox"
            checked={showHidden}
            onChange={(e) => setShowHidden(e.target.checked)}
          />
          <small>hidden</small>
        </label>
        <button
          type="button"
          className="file-browser-refresh"
          aria-label="Refresh directory"
          onClick={() => void loadDir(currentPath)}
        >
          ↻
        </button>
      </div>

      {error ? <p className="inspector-error">{error}</p> : null}

      {loading && !listing ? (
        <p className="inspector-muted">Loading…</p>
      ) : sortedEntries.length === 0 ? (
        <p className="inspector-muted">Empty directory</p>
      ) : (
        <ul className="file-browser-list">
          {sortedEntries.map((entry) => (
            <li key={entry.name}>
              <button
                type="button"
                className="file-browser-entry"
                data-selected={selectedFile?.path === (currentPath ? `${currentPath}/${entry.name}` : entry.name)}
                onClick={() => void openEntry(entry)}
              >
                <span className="file-browser-entry-icon">{fileIcon(entry)}</span>
                <span className="file-browser-entry-name">{entry.name}</span>
                {entry.type === "file" ? (
                  <small className="file-browser-entry-size">{formatSize(entry.size)}</small>
                ) : null}
              </button>
            </li>
          ))}
        </ul>
      )}

      {selectedFile ? (
        <div className="file-browser-viewer">
          <div className="file-browser-viewer-head">
            <strong>{selectedFile.path.split("/").pop()}</strong>
            <button
              type="button"
              aria-label="Close file viewer"
              onClick={() => setSelectedFile(null)}
            >
              ×
            </button>
          </div>
          {selectedFile.loading ? (
            <p className="inspector-muted">Loading file…</p>
          ) : selectedFile.error ? (
            <p className="inspector-error">{selectedFile.error}</p>
          ) : isImageFile(selectedFile.mimeType) ? (
            <img
              className="file-browser-image"
              src={workspaceFileUrl(workspaceId, selectedFile.path)}
              alt={selectedFile.path}
            />
          ) : selectedFile.content != null ? (
            <>
              {selectedFile.truncated ? (
                <p className="inspector-muted">
                  File truncated — showing partial content
                  {selectedFile.totalSize ? ` (${formatSize(selectedFile.totalSize)} total)` : ""}.
                </p>
              ) : null}
              <pre className="file-browser-text">
                <code>{selectedFile.content}</code>
              </pre>
            </>
          ) : (
            <p className="inspector-muted">
              Binary file ({selectedFile.mimeType ?? "unknown type"}).
              {" "}
              <a
                href={workspaceFileUrl(workspaceId, selectedFile.path, true)}
                download
              >
                Download
              </a>
            </p>
          )}
        </div>
      ) : null}
    </div>
  );
}
