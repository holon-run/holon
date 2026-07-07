import { useCallback, useEffect, useRef, useState } from "react";
import {
  ArrowLeft,
  Braces,
  File as FileIcon,
  FileCode2,
  FileCog,
  FileImage,
  FileText,
  Folder,
  Link,
  RefreshCw,
  type LucideIcon,
} from "lucide-react";
import { createHighlighter, type Highlighter } from "shiki";
import Markdown from "react-markdown";
import type { Components } from "react-markdown";
import remarkGfm from "remark-gfm";

import type { WorkspaceDirectoryListing, WorkspaceFileEntry } from "../../runtime/types";
import { useRuntimeStore } from "../../runtime/runtime-store";
import { useTranslation } from "react-i18next";
import { parseWorkspaceImageRef, resolveWorkspaceRelativePath, WorkspaceImage } from "../../components/MarkdownContent";

interface FileBrowserPanelProps {
  workspaceId: string;
  executionRootId?: string;
  initialFilePath?: string;
  initialPath?: string;
  onClose?: () => void;
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

function fileIcon(entry: WorkspaceFileEntry): LucideIcon {
  if (entry.type === "directory") return Folder;
  if (entry.type === "symlink") return Link;
  const ext = entry.name.split(".").pop()?.toLowerCase();
  switch (ext) {
    case "rs": return FileCode2;
    case "ts": case "tsx": return FileCode2;
    case "js": case "jsx": return FileCode2;
    case "json": return Braces;
    case "md": return FileText;
    case "png": case "jpg": case "jpeg": case "gif": case "svg": case "webp": return FileImage;
    case "toml": case "yaml": case "yml": return FileCog;
    default: return FileIcon;
  }
}

function FileEntryIcon({ entry }: { entry: WorkspaceFileEntry }) {
  const Icon = fileIcon(entry);
  return <Icon size={16} />;
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

// --- Shiki syntax highlighting ---

const LANG_MAP: Record<string, string> = {
  rs: "rust", ts: "typescript", tsx: "tsx", js: "javascript", jsx: "jsx",
  json: "json", md: "markdown", toml: "toml", yaml: "yaml", yml: "yaml",
  sh: "bash", bash: "bash", css: "css", scss: "scss", html: "html",
  sql: "sql", py: "python", go: "go", xml: "xml", diff: "diff",
  dockerfile: "docker",
};

const SUPPORTED_LANGS = [...new Set(Object.values(LANG_MAP))];

let highlighterPromise: Promise<Highlighter> | null = null;

function getHighlighter(): Promise<Highlighter> {
  if (!highlighterPromise) {
    highlighterPromise = createHighlighter({
      themes: ["github-light"],
      langs: SUPPORTED_LANGS,
    });
  }
  return highlighterPromise;
}

function langForFile(name: string): string | undefined {
  const base = name.split("/").pop() ?? name;
  const lower = base.toLowerCase();
  if (lower === "dockerfile" || lower.startsWith("dockerfile.")) return "docker";
  const ext = lower.split(".").pop() ?? "";
  return LANG_MAP[ext];
}

/** Async syntax highlighting via shiki. Returns highlighted HTML or null. */
function useShikiHighlight(content: string | undefined, filePath: string | undefined): string | null {
  const [highlighted, setHighlighted] = useState<string | null>(null);

  useEffect(() => {
    if (!content || !filePath) {
      setHighlighted(null);
      return;
    }
    const lang = langForFile(filePath);
    if (!lang) {
      setHighlighted(null);
      return;
    }
    let cancelled = false;
    void getHighlighter().then((hl) => {
      if (cancelled) return;
      try {
        const html = hl.codeToHtml(content, { lang, theme: "github-light" });
        setHighlighted(html);
      } catch {
        setHighlighted(null);
      }
    });
    return () => { cancelled = true; };
  }, [content, filePath]);

  return highlighted;
}

export function FileBrowserPanel({ workspaceId, executionRootId, initialPath, initialFilePath, onClose }: FileBrowserPanelProps) {
  const { t } = useTranslation();
  const browseWorkspaceDir = useRuntimeStore((s) => s.browseWorkspaceDir);
  const readWorkspaceFile = useRuntimeStore((s) => s.readWorkspaceFile);
  const workspaceFileUrl = useRuntimeStore((s) => s.workspaceFileUrl);

  const effectiveInitialPath =
    initialPath ?? (initialFilePath ? initialFilePath.split("/").slice(0, -1).join("/") : "");
  const [currentPath, setCurrentPath] = useState(effectiveInitialPath);
  const [listing, setListing] = useState<WorkspaceDirectoryListing | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  const [selectedFile, setSelectedFile] = useState<SelectedFile | null>(null);
  const [showHidden, setShowHidden] = useState(false);
  const autoOpenedRef = useRef(false);
  const contentScrollRef = useRef<HTMLDivElement>(null);
  const [showRendered, setShowRendered] = useState(true);

  // Determine whether the selected file is markdown.
  const isMarkdownFile = selectedFile?.path?.toLowerCase().endsWith(".md") ?? false;

  // Reset scroll position and toggle state when a new file is opened.
  useEffect(() => {
    if (contentScrollRef.current) {
      contentScrollRef.current.scrollTop = 0;
    }
    setShowRendered(true);
  }, [selectedFile?.path]);

  const highlightedHtml = useShikiHighlight(selectedFile?.content, selectedFile?.path);

  const loadDir = useCallback(
    async (path: string) => {
      setLoading(true);
      setError(undefined);
      setSelectedFile(null);
      try {
        const result = await browseWorkspaceDir(workspaceId, path || undefined, executionRootId);
        setListing(result);
        setCurrentPath(path);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setLoading(false);
      }
    },
    [workspaceId, executionRootId, browseWorkspaceDir],
  );

  const reloadFile = useCallback(async () => {
    if (!selectedFile?.path) return;
    const filePath = selectedFile.path;
    if (isImageFile(selectedFile.mimeType)) {
      // Image files are served via URL, force re-render by re-setting state
      setSelectedFile({ path: filePath, loading: false, mimeType: selectedFile.mimeType });
      return;
    }
    setSelectedFile({ path: filePath, loading: true });
    try {
      const content = await readWorkspaceFile(workspaceId, filePath, executionRootId);
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
  }, [selectedFile, workspaceId, executionRootId, readWorkspaceFile]);

  useEffect(() => {
    void loadDir(effectiveInitialPath);
  }, [loadDir, effectiveInitialPath]);

  // Auto-open the initial file after the directory listing loads.
  useEffect(() => {
    if (!listing || !initialFilePath || autoOpenedRef.current) return;
    const fileName = initialFilePath.split("/").pop();
    const entry = listing.entries.find((e) => e.name === fileName);
    if (!entry) return;
    autoOpenedRef.current = true;
    void openEntry(entry);
  }, [listing, initialFilePath]); // eslint-disable-line react-hooks/exhaustive-deps

  const breadcrumbParts = currentPath.split("/").filter(Boolean);

  const navigateToBreadcrumb = (index: number) => {
    const target = breadcrumbParts.slice(0, index + 1).join("/");
    void loadDir(target);
  };

  const openEntry = async (entry: WorkspaceFileEntry) => {
    if (entry.name === "..") {
      void loadDir(parentPath);
      return;
    }
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
      const content = await readWorkspaceFile(workspaceId, filePath, executionRootId);
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

  const parentPath = currentPath.split("/").filter(Boolean).slice(0, -1).join("/");
  const atRoot = !currentPath;

  const markdownComponents: Components = {
    a: ({ href, children }) => <a href={href} target="_blank" rel="noreferrer">{children}</a>,
    img: ({ src, alt, ...props }) => {
      const workspaceRef = parseWorkspaceImageRef(src);
      if (workspaceRef) {
        return (
          <WorkspaceImage
            {...props}
            workspaceId={workspaceRef.workspaceId}
            path={workspaceRef.path}
            alt={alt ?? workspaceRef.path}
          />
        );
      }

      const relativePath = resolveWorkspaceRelativePath(selectedFile?.path ?? "", src);
      if (!relativePath) {
        return <img {...props} src={src} alt={alt ?? ""} />;
      }
      return (
        <WorkspaceImage
          {...props}
          workspaceId={workspaceId}
          path={relativePath}
          executionRootId={executionRootId}
          alt={alt ?? relativePath}
        />
      );
    },
  };

  const entries = listing?.entries ?? [];
  const visibleEntries = showHidden
    ? entries
    : entries.filter((e) => !e.name.startsWith("."));
  const dirs = visibleEntries.filter((e) => e.type === "directory" || e.type === "symlink");
  const files = visibleEntries.filter((e) => e.type === "file");
  const sortedEntries = [...dirs, ...files];
  if (!atRoot) {
    sortedEntries.unshift({ name: "..", type: "directory" as const, size: 0, mimeType: undefined });
  }

  return (
    <div className="file-browser">
      <div className="file-browser-toolbar">
        <button type="button" className="file-browser-back-btn" onClick={() => onClose?.()}>
          <ArrowLeft size={14} />
          {t("fileBrowser.back")}
        </button>
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
        {!selectedFile ? (
        <label className="file-browser-hidden-toggle">
          <input
            type="checkbox"
            checked={showHidden}
            onChange={(e) => setShowHidden(e.target.checked)}
          />
          <small>hidden</small>
        </label>
        ) : null}
        <button
          type="button"
          className="file-browser-refresh"
          aria-label={selectedFile ? t("fileBrowser.refreshFile") : t("fileBrowser.refreshDir")}
          onClick={() => void (selectedFile ? reloadFile() : loadDir(currentPath))}
        >
          <RefreshCw size={14} />
        </button>
      </div>

      {error ? <p className="inspector-error">{error}</p> : null}

      {loading && !listing ? (
        <p className="inspector-muted">{t("common.loading")}</p>
      ) : sortedEntries.length === 0 ? (
        <p className="inspector-muted">Empty directory</p>
      ) : (
        <ul className="file-browser-list">
          {sortedEntries.map((entry) => (
            <li key={entry.name}>
              <button
                type="button"
                className="file-browser-entry"
                data-parent-dir={entry.name === ".." ? true : undefined}
                data-selected={selectedFile?.path === (currentPath ? `${currentPath}/${entry.name}` : entry.name)}
                onClick={() => void openEntry(entry)}
              >
                <span className="file-browser-entry-icon"><FileEntryIcon entry={entry} /></span>
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
            <div className="file-browser-viewer-actions">
              {isMarkdownFile ? (
                <div className="file-browser-md-toggle" role="group" aria-label="Markdown view mode">
                  <button
                    type="button"
                    className={showRendered ? "active" : ""}
                    onClick={() => setShowRendered(true)}
                  >
                    Rendered
                  </button>
                  <button
                    type="button"
                    className={!showRendered ? "active" : ""}
                    onClick={() => setShowRendered(false)}
                  >
                    Source
                  </button>
                </div>
              ) : null}
              <button type="button" className="file-browser-close-btn" aria-label="Close file" onClick={() => setSelectedFile(null)}>× Close</button>
            </div>
          </div>
          {selectedFile.loading ? (
            <p className="inspector-muted">Loading file…</p>
          ) : selectedFile.error ? (
            <p className="inspector-error">{selectedFile.error}</p>
          ) : isImageFile(selectedFile.mimeType) ? (
            <WorkspaceImage
              className="file-browser-image"
              workspaceId={workspaceId}
              path={selectedFile.path}
              executionRootId={executionRootId}
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
              {isMarkdownFile && showRendered ? (
                <div className="file-browser-markdown" ref={contentScrollRef}>
                  <Markdown remarkPlugins={[remarkGfm]} components={markdownComponents}>
                    {selectedFile.content}
                  </Markdown>
                </div>
              ) : highlightedHtml ? (
                <div className="file-browser-code" ref={contentScrollRef} dangerouslySetInnerHTML={{ __html: highlightedHtml }} />
              ) : (
                <pre className="file-browser-text" ref={contentScrollRef as React.Ref<HTMLPreElement>}>
                  <code>{selectedFile.content}</code>
                </pre>
              )}
            </>
          ) : (
            <p className="inspector-muted">
              Binary file ({selectedFile.mimeType ?? "unknown type"}).
              {" "}
              <a
                href={workspaceFileUrl(workspaceId, selectedFile.path, true, executionRootId)}
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
