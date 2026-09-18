import { useCopyText } from "../../components/ClipboardProvider";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode, type RefObject } from "react";
import {
  ArrowLeft,
  Info,
  Monitor,
  Globe,
  MoreHorizontal,
  ExternalLink,
  ArrowUp,
  Braces,
  File as FileIcon,
  FileCode2,
  FileCog,
  FileImage,
  FileText,
  Folder,
  Link,
  Search,
  X,
  type LucideIcon,
} from "lucide-react";
import { createHighlighter, type Highlighter } from "shiki";

import type { WorkspaceDirectoryListing, WorkspaceFileEntry, WorkspaceFileLocation } from "../../runtime/types";
import { getRuntimeConnectionConfig, useRuntimeStore } from "../../runtime/runtime-store";
import { useTranslation } from "react-i18next";
import { MarkdownContent, WorkspaceImage } from "../../components/MarkdownContent";
import { filePreviewUrl, type FileTarget } from "../../components/file-references/references";
import { useFileIdentity } from "../../components/file-references/use-references";
import { createRuntimeClient } from "../../runtime/client";
import { connectionLocation } from "../../runtime/connection-location";
import { triggerBlobDownload, triggerHrefDownload } from "./download";
import { buildPlainCodeHtml, normalizeShikiLineBreaks } from "./source-view";

interface FileBrowserPanelProps {
  workspaceId: string;
  executionRootId?: string;
  initialFilePath?: string;
  initialPath?: string;
  initialFragment?: string;
  onOpenFile?: (target: FileTarget) => void;
  workspaceLabel?: string;
  workspaceControl?: ReactNode;
  backActionRef?: RefObject<(() => void) | null>;
  onClose?: () => void;
  snapshot?: FileBrowserSnapshot;
  onSnapshot?: (snapshot: FileBrowserSnapshot) => void;
}

export interface FileBrowserSnapshot {
  identity: string;
  fragment?: string;
  currentPath: string;
  listing: WorkspaceDirectoryListing | null;
  selectedFile: SelectedFile | null;
  showHidden: boolean;
  showRendered: boolean;
  viewMode: "files" | "preview";
  filterText: string;
  sortKey: "name" | "size" | "modified";
  sortAsc: boolean;
  directoryVisible: boolean;
  scroll: Record<string, { top: number; left: number }>;
  history: FileBrowserLocation[];
}

type FileBrowserLocation = Pick<FileBrowserSnapshot, "currentPath" | "listing" | "selectedFile" | "showRendered" | "viewMode" | "scroll" | "filterText" | "fragment">;

interface SelectedFile {
  path: string;
  absolutePath?: string;
  rootKind?: string;
  content?: string;
  mimeType?: string;
  truncated?: boolean;
  totalSize?: number;
  modified?: number;
  lineCount?: number;
  size?: number;
  loading: boolean;
  error?: string;
}

export function markdownFileReference(
  workspaceId: string,
  path: string,
  executionRootId?: string,
): string {
  const encodeComponent = (value: string) =>
    encodeURIComponent(value).replace(
      /[!'()*]/g,
      (character) => `%${character.charCodeAt(0).toString(16).toUpperCase()}`,
    );
  const encodedPath = path.split("/").map(encodeComponent).join("/");
  const root = executionRootId ? `?root=${encodeComponent(executionRootId)}` : "";
  const uri = `workspace://${workspaceId}/${encodedPath}${root}`;
  const label = path.split("/").pop() || path;
  return `[${label}](${uri})`;
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

const VIDEO_EXTENSIONS = ["mp4", "webm", "ogv", "mov", "mkv", "m4v"];
const AUDIO_EXTENSIONS = ["mp3", "wav", "ogg", "oga", "flac", "m4a", "aac", "opus"];

function fileExtension(name: string): string {
  return name.split(".").pop()?.toLowerCase() ?? "";
}

export function isVideoFile(mimeType?: string, name?: string): boolean {
  if (mimeType?.startsWith("video/")) return true;
  return Boolean(name && VIDEO_EXTENSIONS.includes(fileExtension(name)));
}

export function isAudioFile(mimeType?: string, name?: string): boolean {
  if (mimeType?.startsWith("audio/")) return true;
  return Boolean(name && AUDIO_EXTENSIONS.includes(fileExtension(name)));
}

export function isPdfFile(mimeType?: string, name?: string): boolean {
  if (mimeType === "application/pdf") return true;
  return Boolean(name && fileExtension(name) === "pdf");
}

function formatDateTime(unixSeconds?: number): string | undefined {
  if (!unixSeconds) return undefined;
  return new Date(unixSeconds * 1000).toLocaleString(undefined, {
    dateStyle: "short",
    timeStyle: "short",
  });
}

function formatDate(unixSeconds?: number): string | undefined {
  if (!unixSeconds) return undefined;
  return new Date(unixSeconds * 1000).toLocaleDateString();
}

const GIGABYTE = 1024 * 1024 * 1024;

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < GIGABYTE) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / GIGABYTE).toFixed(2)} GB`;
}

export function isLargePreview(totalBytes?: number): boolean {
  return Boolean(totalBytes && totalBytes > GIGABYTE);
}

export type FileSortKey = "name" | "size" | "modified";

// Containers (directories and symlinks) always sort before plain files;
// equal keys fall back to name order.
export function compareFileEntries(
  a: WorkspaceFileEntry,
  b: WorkspaceFileEntry,
  key: FileSortKey,
  ascending: boolean,
): number {
  const aContainer = a.type === "directory" || a.type === "symlink";
  const bContainer = b.type === "directory" || b.type === "symlink";
  if (aContainer !== bContainer) return aContainer ? -1 : 1;
  let cmp: number;
  if (key === "size") cmp = a.size - b.size;
  else if (key === "modified") cmp = (a.modified ?? 0) - (b.modified ?? 0);
  else cmp = a.name.localeCompare(b.name);
  if (cmp !== 0) return ascending ? cmp : -cmp;
  // Ties fall back to name order so descending sorts stay stable.
  return a.name.localeCompare(b.name);
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

/**
 * Async syntax highlighting via shiki. Returns highlighted HTML that matches
 * the current content/path, or null while pending/stale so the caller renders
 * a layout-compatible plain fallback instead of previously highlighted HTML.
 */
function useShikiHighlight(content: string | undefined, filePath: string | undefined): string | null {
  const [state, setState] = useState<{ content: string; path: string; html: string | null } | null>(null);

  useEffect(() => {
    const lang = content && filePath ? langForFile(filePath) : undefined;
    if (!content || !filePath || !lang) {
      setState(null);
      return;
    }
    let cancelled = false;
    void getHighlighter().then((hl) => {
      if (cancelled) return;
      try {
        const html = normalizeShikiLineBreaks(hl.codeToHtml(content, { lang, theme: "github-light" }));
        setState({ content, path: filePath, html });
      } catch {
        setState(null);
      }
    });
    return () => { cancelled = true; };
  }, [content, filePath]);

  if (!state || state.content !== content || state.path !== filePath) return null;
  return state.html;
}

export function FileBrowserPanel(props: FileBrowserPanelProps) {
  const identity = useFileIdentity();
  return <FileBrowserPanelView key={identity} {...props} identity={identity}
    snapshot={props.snapshot?.identity === identity ? props.snapshot : undefined} />;
}

function FileBrowserPanelView({ identity, workspaceId, executionRootId, initialPath, initialFilePath, initialFragment, onOpenFile, workspaceLabel, workspaceControl, backActionRef, onClose, snapshot, onSnapshot }: FileBrowserPanelProps & { identity: string }) {
  const copyText = useCopyText();
  const client = useMemo(() => createRuntimeClient(getRuntimeConnectionConfig()), [identity]);
  const connection = connectionLocation(getRuntimeConnectionConfig(), window.location.origin);
  const [canReveal, setCanReveal] = useState(false);
  const [revealing, setRevealing] = useState(false);
  const [actionError, setActionError] = useState<string>();
  const [showInfo, setShowInfo] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [ancestorsOpen, setAncestorsOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const ancestorsRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    let cancelled = false;
    if (connection.loopback && connection.sameOrigin) {
      void client.desktopCapabilities().then((caps) => {
        if (!cancelled) setCanReveal(caps.reveal_in_finder === true);
      }).catch(() => { /* Older servers and disabled integrations have no desktop actions. */ });
    }
    return () => { cancelled = true; };
  }, [client, connection.loopback, connection.sameOrigin]);
  useEffect(() => {
    if (!menuOpen && !ancestorsOpen) return;
    const close = (event: MouseEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) setMenuOpen(false);
      if (!ancestorsRef.current?.contains(event.target as Node)) setAncestorsOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.stopImmediatePropagation();
      event.preventDefault();
      (menuOpen ? menuRef : ancestorsRef).current?.querySelector<HTMLButtonElement>("button")?.focus();
      setMenuOpen(false); setAncestorsOpen(false);
    };
    document.addEventListener("click", close);
    document.addEventListener("keydown", escape, true);
    return () => { document.removeEventListener("click", close); document.removeEventListener("keydown", escape, true); };
  }, [menuOpen, ancestorsOpen]);
  const { t } = useTranslation();
  const [fragment, setFragment] = useState(snapshot?.fragment ?? initialFragment);
  const browseWorkspaceDir = useRuntimeStore((s) => s.browseWorkspaceDir);
  const readWorkspaceFile = useRuntimeStore((s) => s.readWorkspaceFile);
  const fetchWorkspacePath = useRuntimeStore((s) => s.fetchWorkspacePath);
  const workspaceFileUrl = useRuntimeStore((s) => s.workspaceFileUrl);

  const effectiveInitialPath =
    initialPath ?? (initialFilePath ? initialFilePath.split("/").slice(0, -1).join("/") : "");
  const [currentPath, setCurrentPath] = useState(snapshot?.currentPath ?? effectiveInitialPath);
  const [listing, setListing] = useState<WorkspaceDirectoryListing | null>(snapshot?.listing ?? null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string>();
  const [selectedFile, setSelectedFile] = useState<SelectedFile | null>(snapshot?.selectedFile ?? null);
  const [showHidden, setShowHidden] = useState(snapshot?.showHidden ?? false);
  const [copiedValue, setCopiedValue] = useState<"absolute" | "markdown" | "web" | null>(null);
  const autoOpenedRef = useRef(Boolean(snapshot?.selectedFile));
  const contentScrollRef = useRef<HTMLDivElement>(null);
  const [showRendered, setShowRendered] = useState(snapshot?.showRendered ?? true);
  const [viewMode, setViewMode] = useState<"files" | "preview">(snapshot?.viewMode ?? "files");
  const [filterText, setFilterText] = useState(snapshot?.filterText ?? "");
  const [sortKey, setSortKey] = useState<"name" | "size" | "modified">(snapshot?.sortKey ?? "name");
  const [sortAsc, setSortAsc] = useState(snapshot?.sortAsc ?? true);
  const [directoryVisible, setDirectoryVisible] = useState(snapshot?.directoryVisible ?? true);
  const [wide, setWide] = useState(false);
  const browserRef = useRef<HTMLDivElement>(null);
  const scroll = useRef(snapshot?.scroll ?? {});
  const history = useRef<FileBrowserLocation[]>(snapshot?.history ?? []);
  const requestGeneration = useRef(0);
  useEffect(() => () => { requestGeneration.current++; }, [identity]);
  const latestSnapshot = useRef<FileBrowserSnapshot | null>(null);
  useLayoutEffect(() => {
    latestSnapshot.current = { identity, fragment, currentPath, listing, selectedFile, showHidden, showRendered, viewMode, filterText, sortKey, sortAsc, directoryVisible, scroll: scroll.current, history: history.current };
    if (!loading && !selectedFile?.loading && (!initialFilePath || autoOpenedRef.current)) onSnapshot?.(latestSnapshot.current);
  });
  useLayoutEffect(() => {
    const node = browserRef.current;
    if (!node) return;
    const observer = new ResizeObserver(([entry]) => setWide(entry.contentRect.width >= 960));
    observer.observe(node);
    return () => observer.disconnect();
  }, []);
  useLayoutEffect(() => {
    browserRef.current?.querySelectorAll<HTMLElement>("[data-file-scroll]").forEach((node) => {
      const saved = scroll.current[node.dataset.fileScroll!];
      if (saved) { node.scrollTop = saved.top; node.scrollLeft = saved.left; }
    });
  }, [viewMode, wide, directoryVisible, selectedFile?.path, showRendered]);
  const split = wide && directoryVisible && Boolean(selectedFile);
  const rememberLocation = () => {
    const current = latestSnapshot.current;
    if (!current?.selectedFile || current.selectedFile.loading) return;
    const { currentPath, listing, selectedFile, showRendered, viewMode, filterText, fragment } = current;
    history.current = [...history.current.slice(-3), { currentPath, listing, selectedFile, showRendered, viewMode, filterText, fragment, scroll: { ...scroll.current } }];
  };
  const goBack = () => {
    const previous = history.current.pop();
    if (!previous) { onClose?.(); return; }
    requestGeneration.current++;
    previousFile.current = previous.selectedFile?.path;
    scroll.current = previous.scroll;
    setCurrentPath(previous.currentPath);
    setFragment(previous.fragment);
    setListing(previous.listing);
    setSelectedFile(previous.selectedFile);
    setShowRendered(previous.showRendered);
    setViewMode(previous.viewMode);
    setFilterText(previous.filterText);
    setLoading(false);
    setError(undefined);
  };
  useLayoutEffect(() => {
    if (!backActionRef) return;
    backActionRef.current = goBack;
    return () => { backActionRef.current = null; };
  });
  const [imageDims, setImageDims] = useState<{ width: number; height: number }>();

  // Determine whether the selected file is markdown.
  const isMarkdownFile = selectedFile?.path?.toLowerCase().endsWith(".md") ?? false;

  // Reset scroll position and toggle state when a new file is opened.
  const previousFile = useRef(snapshot?.selectedFile?.path);
  useEffect(() => {
    if (previousFile.current === selectedFile?.path) return;
    previousFile.current = selectedFile?.path;
    if (contentScrollRef.current) {
      contentScrollRef.current.scrollTop = 0;
    }
    setShowRendered(true);
    setImageDims(undefined);
    setActionError(undefined);
    setMenuOpen(false);
  }, [selectedFile?.path]);

  const highlightedHtml = useShikiHighlight(selectedFile?.content, selectedFile?.path);
  // Plain fallback shares the shiki DOM skeleton so the async highlight swap
  // never changes layout metrics; only token colors appear.
  const plainCodeHtml = useMemo(
    () => (selectedFile?.content != null ? buildPlainCodeHtml(selectedFile.content) : ""),
    [selectedFile?.content],
  );

  const loadDir = useCallback(
    async (path: string) => {
      const request = ++requestGeneration.current;
      setLoading(true);
      setError(undefined);
      setSelectedFile(null);
      setViewMode("files");
      setFilterText("");
      try {
        const result = await browseWorkspaceDir({ workspaceId, path, executionRootId });
        if (request !== requestGeneration.current) return;
        setListing(result);
        setCurrentPath(path);
      } catch (err) {
        if (request !== requestGeneration.current) return;
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        if (request === requestGeneration.current) setLoading(false);
      }
    },
    [workspaceId, executionRootId, browseWorkspaceDir],
  );

  const reloadFile = useCallback(async () => {
    if (!selectedFile?.path) return;
    const request = ++requestGeneration.current;
    const filePath = selectedFile.path;
    if (
      isImageFile(selectedFile.mimeType) ||
      isVideoFile(selectedFile.mimeType, selectedFile.path) ||
      isAudioFile(selectedFile.mimeType, selectedFile.path) ||
      isPdfFile(selectedFile.mimeType, selectedFile.path)
    ) {
      // URL-backed previews: force re-render by re-setting state.
      setSelectedFile({ ...selectedFile, path: filePath, loading: false });
      return;
    }
    setSelectedFile({ path: filePath, loading: true });
    try {
      const content = await readWorkspaceFile({ workspaceId, path: filePath, executionRootId });
      if (request !== requestGeneration.current) return;
      setSelectedFile({
        path: filePath,
        absolutePath: content.absolutePath,
        rootKind: content.rootKind,
        content: content.content,
        mimeType: content.mimeType,
        truncated: content.truncated,
        totalSize: content.totalSize ?? content.size,
        modified: content.modified,
        lineCount: content.lineCount,
        size: content.totalSize ?? content.size,
        loading: false,
      });
    } catch (err) {
      if (request !== requestGeneration.current) return;
      setSelectedFile({
        path: filePath,
        loading: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  }, [selectedFile, workspaceId, executionRootId, readWorkspaceFile]);

  useEffect(() => {
    if (!snapshot?.listing) void loadDir(effectiveInitialPath);
  }, [loadDir, effectiveInitialPath]);

  // Auto-open the initial file after the directory listing loads.
  useEffect(() => {
    if (!listing || !initialFilePath || autoOpenedRef.current) return;
    const fileName = initialFilePath.split("/").pop();
    const entry = listing.entries.find((e) => e.name === fileName);
    if (!entry) {
      autoOpenedRef.current = true;
      void openWorkspacePath(initialFilePath);
      return;
    }
    autoOpenedRef.current = true;
    setViewMode("preview");
    void openEntry(entry, true);
  }, [listing, initialFilePath]); // eslint-disable-line react-hooks/exhaustive-deps

  const breadcrumbParts = currentPath.split("/").filter(Boolean);

  const navigateToBreadcrumb = (index: number) => {
    const target = breadcrumbParts.slice(0, index + 1).join("/");
    void loadDir(target);
  };

  const openEntry = async (entry: WorkspaceFileEntry, preserveFragment = false) => {
    if (!preserveFragment) setFragment(undefined);
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
    if (filePath !== selectedFile?.path) rememberLocation();
    const request = ++requestGeneration.current;
    setViewMode("preview");

    if (!isTextFile(entry.mimeType, entry.name)) {
      // URL-backed previews (image, video, audio, PDF) and other binary
      // files render from entry metadata without reading content.
      setSelectedFile({
        path: filePath,
        absolutePath: listing
          ? `${listing.absolutePath.replace(/\/$/, "")}/${entry.name}`
          : undefined,
        rootKind: listing?.rootKind,
        loading: false,
        mimeType: entry.mimeType,
        size: entry.size,
        modified: entry.modified,
      });
      return;
    }

    setSelectedFile({ path: filePath, loading: true });
    try {
      const content = await readWorkspaceFile({ workspaceId, path: filePath, executionRootId });
      if (request !== requestGeneration.current) return;
      setSelectedFile({
        path: filePath,
        absolutePath: content.absolutePath,
        rootKind: content.rootKind,
        content: content.content,
        mimeType: content.mimeType,
        truncated: content.truncated,
        totalSize: content.totalSize ?? content.size,
        modified: content.modified,
        lineCount: content.lineCount,
        size: content.totalSize ?? content.size,
        loading: false,
      });
    } catch (err) {
      if (request !== requestGeneration.current) return;
      setSelectedFile({
        path: filePath,
        loading: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  };

  /**
   * Open an arbitrary workspace path, used by rendered markdown links:
   * directories navigate the listing, and files preview exactly like a
   * tree-clicked entry. Metadata is resolved first because no directory
   * entry exists for the target.
   */
  const openWorkspacePath = useCallback(async (filePath: string) => {
    rememberLocation();
    const request = ++requestGeneration.current;
    setViewMode("preview");
    setSelectedFile({ path: filePath, loading: true });
    setError(undefined);
    try {
      const info = await fetchWorkspacePath({ workspaceId, path: filePath, executionRootId });
      if (request !== requestGeneration.current) return;
      if (info.type === "directory") {
        setListing(info);
        setCurrentPath(info.path);
        setSelectedFile(null);
        setViewMode("files");
        setFilterText("");
        return;
      }
      const parent = filePath.split("/").slice(0, -1).join("/");
      const directory = await browseWorkspaceDir({ workspaceId, path: parent, executionRootId });
      if (request !== requestGeneration.current) return;
      setListing(directory);
      setCurrentPath(parent);
      if (!isTextFile(info.mimeType, filePath)) {
        setSelectedFile({
          path: info.path,
          absolutePath: info.absolutePath,
          rootKind: info.rootKind,
          loading: false,
          mimeType: info.mimeType,
          size: info.totalSize ?? info.size,
          modified: info.modified,
        });
        return;
      }
      const content = await readWorkspaceFile({ workspaceId, path: filePath, executionRootId });
      if (request !== requestGeneration.current) return;
      setSelectedFile({
        path: content.path,
        absolutePath: content.absolutePath,
        rootKind: content.rootKind,
        content: content.content,
        mimeType: content.mimeType,
        truncated: content.truncated,
        totalSize: content.totalSize ?? content.size,
        modified: content.modified,
        lineCount: content.lineCount,
        size: content.totalSize ?? content.size,
        loading: false,
      });
    } catch (err) {
      if (request !== requestGeneration.current) return;
      setSelectedFile({
        path: filePath,
        loading: false,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  }, [workspaceId, executionRootId, fetchWorkspacePath, readWorkspaceFile, browseWorkspaceDir]);

  const effectiveRootId = listing?.executionRootId ?? executionRootId;
  const downloadSelectedFile = async () => {
    if (!selectedFile?.path) return;
    // Cookie-authenticated downloads can stream directly; Bearer sessions need fetch.
    if (!getRuntimeConnectionConfig().token) {
      triggerHrefDownload(workspaceFileUrl({ workspaceId, path: selectedFile.path, executionRootId: effectiveRootId }, { download: true }));
      return;
    }
    const request = requestGeneration.current;
    try {
      const blob = await useRuntimeStore.getState().fetchWorkspaceFileBlob({ workspaceId, path: selectedFile.path, executionRootId: effectiveRootId }, { download: true });
      if (request !== requestGeneration.current) return;
      triggerBlobDownload(blob, selectedFile.path.split("/").pop() || "download");
    } catch (cause) {
      if (request === requestGeneration.current) setError(cause instanceof Error ? cause.message : "Download failed");
    }
  };

  const openSelectedFileInNewTab = () => {
    if (!selectedFile?.path || !effectiveRootId) return;
    window.open(
      filePreviewUrl({ workspaceId, path: selectedFile.path, executionRootId: effectiveRootId }, fragment),
      "_blank",
      "noopener,noreferrer",
    );
  };

  const copySelectedFileValue = async (
    kind: "absolute" | "markdown" | "web",
    value: string | undefined,
  ) => {
    if (!value) return;
    try {
      if (!await copyText(value)) return;
      setCopiedValue(kind);
      window.setTimeout(() => setCopiedValue(null), 2000);
    } catch {
      // Clipboard access can be denied; leave the button label unchanged.
    }
  };

  const selectedWebUrl = selectedFile?.path && effectiveRootId
    ? new URL(
        filePreviewUrl({ workspaceId, path: selectedFile.path, executionRootId: effectiveRootId }, fragment),
        window.location.origin,
      ).href
    : undefined;
  const selectedMarkdownReference = selectedFile?.path
    ? markdownFileReference(workspaceId, selectedFile.path, executionRootId)
    : undefined;

  const parentPath = currentPath.split("/").filter(Boolean).slice(0, -1).join("/");
  const atRoot = !currentPath;

  const openMarkdownFile = (target: FileTarget) => {
    if (onOpenFile) { onOpenFile(target); return; }
    if (target.workspaceId === workspaceId && target.executionRootId === effectiveRootId) {
      setFragment(target.fragment);
      void openWorkspacePath(target.path);
    } else {
      const store = useRuntimeStore.getState();
      store.openResolvedFile(store.selectedAgentId, target);
    }
  };

  const entries = listing?.entries ?? [];
  const visibleEntries = showHidden
    ? entries
    : entries.filter((e) => !e.name.startsWith("."));
  const toggleSort = (key: "name" | "size" | "modified") => {
    if (sortKey === key) {
      setSortAsc((v) => !v);
    } else {
      setSortKey(key);
      setSortAsc(true);
    }
  };
  const sortedEntries = [...visibleEntries].sort((a, b) =>
    compareFileEntries(a, b, sortKey, sortAsc),
  );
  const filteredEntries = filterText
    ? sortedEntries.filter((e) => e.name.toLowerCase().includes(filterText.toLowerCase()))
    : sortedEntries;

  const selectedFileUrl = selectedFile
    ? workspaceFileUrl({ workspaceId, path: selectedFile.path, executionRootId })
    : undefined;
  const selectedFileTotalBytes =
    selectedFile?.totalSize ?? selectedFile?.size;
  const previewVisible = Boolean(selectedFile) && (viewMode === "preview" || split);
  const largePreviewHint =
    selectedFileTotalBytes != null && isLargePreview(selectedFileTotalBytes)
      ? t("fileBrowser.largeFileHint", { size: formatSize(selectedFileTotalBytes) })
      : undefined;

  return (
    <div className="file-browser" ref={browserRef} data-split={split} onScrollCapture={(event) => {
      const node = event.target as HTMLElement;
      if (node.dataset.fileScroll) {
        scroll.current[node.dataset.fileScroll] = { top: node.scrollTop, left: node.scrollLeft };
        if (latestSnapshot.current) onSnapshot?.({ ...latestSnapshot.current, scroll: scroll.current });
      }
    }}>
      {!backActionRef && selectedFile ? <div className="file-browser-standalone-title">
        <strong title={selectedFile.path}>{selectedFile.path.split("/").pop()}</strong>
      </div> : null}
      <div className="file-browser-toolbar">
        {!backActionRef && (onClose || history.current.length > 0) ? <button type="button" aria-label={t("rightPanel.backToSource")} onClick={goBack}><ArrowLeft size={14} /></button> : null}
        <span className="file-browser-location" role="img" aria-label={`${connection.loopback ? t("connection.localAddress") : connection.host} · ${connection.origin}`} title={`${connection.loopback ? t("connection.localAddress") : connection.host} · ${connection.origin}`}>{connection.loopback ? <Monitor size={14} aria-hidden="true" /> : <Globe size={14} aria-hidden="true" />}<span className="file-browser-location-label">{connection.loopback ? t("connection.localAddress") : connection.host}</span></span>
        {workspaceControl ?? <span className="file-browser-workspace" title={workspaceLabel ?? workspaceId}>{workspaceLabel ?? workspaceId}</span>}
        {listing?.rootKind === "git_worktree_root" ? <span className="file-browser-root-kind">{t("fileBrowser.worktree")}</span> : null}
        <nav className="file-browser-breadcrumb" aria-label={t("fileBrowser.pathBreadcrumb")}>
          <button type="button" className="file-browser-crumb" onClick={() => void loadDir("")}>{t("fileBrowser.root")}</button>
          {breadcrumbParts.length > 1 ? <div className="file-browser-popover" ref={ancestorsRef}>
            <button type="button" aria-label={t("fileBrowser.parentFolders")} aria-expanded={ancestorsOpen} onClick={() => setAncestorsOpen(!ancestorsOpen)}>…</button>
            {ancestorsOpen ? <div className="file-browser-popover-content file-browser-ancestors">
              {breadcrumbParts.slice(0, -1).map((part, i) => <button type="button" key={i} title={breadcrumbParts.slice(0, i + 1).join("/")} onClick={() => { setAncestorsOpen(false); navigateToBreadcrumb(i); }}>{part}</button>)}
            </div> : null}
          </div> : null}
          {breadcrumbParts.length > 0 ? <><span className="file-browser-sep">/</span><button type="button" className="file-browser-crumb file-browser-current-folder" title={currentPath} onClick={() => navigateToBreadcrumb(breadcrumbParts.length - 1)}>{breadcrumbParts.at(-1)}</button></> : null}
        </nav>
        <button type="button" aria-label={t("fileBrowser.fileInfo")} aria-expanded={showInfo} onClick={() => setShowInfo(!showInfo)}><Info size={16} /></button>
      </div>

      <div className="file-browser-controls">
        {(viewMode === "preview" || split) && selectedFile ? <button type="button" className="file-browser-browse" aria-label={t("fileBrowser.browseDirectory")} title={t("fileBrowser.browseDirectory")} aria-pressed={split} onClick={() => {
          if (wide) { setViewMode("preview"); setDirectoryVisible(!directoryVisible); } else setViewMode("files");
        }}><Folder size={14} /><span>{t("fileBrowser.browseDirectory")}</span></button> : <>
          <button type="button" disabled={atRoot} aria-label={t("fileBrowser.upDir")} onClick={() => void loadDir(parentPath)}><ArrowUp size={14} /></button>
          <label className="file-browser-hidden-toggle"><input type="checkbox" checked={showHidden} onChange={(e) => setShowHidden(e.target.checked)} /><small>{t("fileBrowser.hidden")}</small></label>
          {selectedFile ? <button type="button" onClick={() => setViewMode("preview")}>{t("fileBrowser.returnToFile")}</button> : null}
        </>}
        {(viewMode === "preview" || split) && selectedFile && isMarkdownFile ? <div className="file-browser-md-toggle" role="group" aria-label={t("fileBrowser.markdownView")}>
          <button type="button" className={showRendered ? "active" : ""} onClick={() => setShowRendered(true)}>{t("fileBrowser.rendered")}</button>
          <button type="button" className={!showRendered ? "active" : ""} onClick={() => setShowRendered(false)}>{t("fileBrowser.source")}</button>
        </div> : null}
        <div className="file-browser-controls-end">
          {selectedFile && (viewMode === "preview" || split) ? <button type="button" aria-label={t("fileBrowser.openInNewTab")} title={t("fileBrowser.openInNewTab")} disabled={!selectedWebUrl} onClick={openSelectedFileInNewTab}><ExternalLink size={16} /></button> : null}
          <div className="file-browser-popover" ref={menuRef}>
            <button type="button" aria-label={t("fileBrowser.fileActions")} aria-expanded={menuOpen} onClick={() => setMenuOpen(!menuOpen)}><MoreHorizontal size={18} /></button>
            {menuOpen ? <div className="file-browser-popover-content">
              {selectedFile && (viewMode === "preview" || split) ? <>
                {canReveal ? <button type="button" disabled={revealing || selectedFile.loading || Boolean(selectedFile.error) || !effectiveRootId} onClick={() => {
                  setActionError(undefined); setRevealing(true);
                  void client.revealFileInFinder({ workspaceId, executionRootId: effectiveRootId!, path: selectedFile.path })
                    .then(() => setMenuOpen(false)).catch((cause: unknown) => setActionError(cause instanceof Error ? cause.message : t("fileBrowser.revealFailed")))
                    .finally(() => setRevealing(false));
                }}>{t("fileBrowser.revealInFinder")}</button> : null}
                <button type="button" disabled={!selectedFile.absolutePath} onClick={() => void copySelectedFileValue("absolute", selectedFile.absolutePath)}>{copiedValue === "absolute" ? t("fileBrowser.pathCopied") : t("fileBrowser.copyAbsolutePath")}</button>
                <button type="button" onClick={() => void copySelectedFileValue("markdown", selectedMarkdownReference)}>{copiedValue === "markdown" ? t("fileBrowser.markdownCopied") : t("fileBrowser.copyMarkdownReference")}</button>
                <button type="button" disabled={!selectedWebUrl} onClick={() => void copySelectedFileValue("web", selectedWebUrl)}>{copiedValue === "web" ? t("fileBrowser.linkCopied") : t("fileBrowser.copyWebLink")}</button>
                <button type="button" onClick={() => { setMenuOpen(false); void downloadSelectedFile(); }}>{t("fileBrowser.download")}</button>
              </> : null}
              <button type="button" onClick={() => { setMenuOpen(false); void (viewMode === "preview" && selectedFile ? reloadFile() : loadDir(currentPath)); }}>{viewMode === "preview" && selectedFile ? t("fileBrowser.refreshFile") : t("fileBrowser.refreshDir")}</button>
            </div> : null}
          </div>
        </div>
      </div>
      {showInfo ? <div className="file-browser-info" role="region" aria-label={t("fileBrowser.fileInfo")}>
        <dl>
          <dt>{t("connection.currentServer")}</dt><dd>{connection.origin}</dd>
          <dt>{t("fileBrowser.fullPath")}</dt><dd>{(previewVisible ? selectedFile?.absolutePath : listing?.absolutePath) ?? currentPath}</dd>
          <dt>{t("fileBrowser.executionRoot")}</dt><dd>{effectiveRootId}</dd>
          {previewVisible && selectedFile ? <><dt>{t("fileBrowser.fileInfo")}</dt><dd>{[selectedFile.mimeType, selectedFileTotalBytes == null ? undefined : formatSize(selectedFileTotalBytes), selectedFile.modified == null ? undefined : t("fileBrowser.modifiedAt", { time: formatDateTime(selectedFile.modified) }), selectedFile.lineCount == null ? undefined : t("fileBrowser.lineCount", { count: selectedFile.lineCount }), imageDims ? t("fileBrowser.imageDimensions", imageDims) : undefined].filter(Boolean).join(" · ")}</dd></> : null}
        </dl>
      </div> : null}
      {actionError ? <p className="inspector-error" role="alert">{actionError}</p> : null}
      {error ? <p className="inspector-error">{error}</p> : null}

      {viewMode === "files" || split ? (
        <div className="file-browser-filter">
          <Search size={14} className="file-browser-filter-icon" />
          <input
            type="text"
            placeholder={t("fileBrowser.filterPlaceholder")}
            value={filterText}
            onChange={(e) => setFilterText(e.target.value)}
          />
          {filterText ? (
            <>
              <small className="file-browser-filter-count">
                {filteredEntries.length}/{sortedEntries.length}
              </small>
              <button
                type="button"
                className="file-browser-filter-clear"
                aria-label={t("fileBrowser.filterClear")}
                onClick={() => setFilterText("")}
              >
                <X size={14} />
              </button>
            </>
          ) : null}
        </div>
      ) : null}

      {error ? <p className="inspector-error">{error}</p> : null}

      <div className="file-browser-panes">
      {viewMode === "files" || split ? (
        <>
          {loading && !listing ? (
        <p className="inspector-muted">{t("common.loading")}</p>
          ) : filteredEntries.length === 0 ? (
            <p className="inspector-muted">
              {filterText ? t("fileBrowser.noMatch") : t("fileBrowser.emptyDir")}
            </p>
      ) : (
        <div className="file-browser-listing" data-file-scroll="directory">
          <div className="file-browser-columns">
            <button
              type="button"
              className={`file-browser-column file-browser-column-name${sortKey === "name" ? " active" : ""}`}
              onClick={() => toggleSort("name")}
            >
              {t("fileBrowser.columnName")}
              {sortKey === "name" ? (sortAsc ? " ▲" : " ▼") : ""}
            </button>
            <button
              type="button"
              className={`file-browser-column file-browser-column-size${sortKey === "size" ? " active" : ""}`}
              onClick={() => toggleSort("size")}
            >
              {t("fileBrowser.columnSize")}
              {sortKey === "size" ? (sortAsc ? " ▲" : " ▼") : ""}
            </button>
            <button
              type="button"
              className={`file-browser-column file-browser-column-time${sortKey === "modified" ? " active" : ""}`}
              onClick={() => toggleSort("modified")}
            >
              {t("fileBrowser.columnModified")}
              {sortKey === "modified" ? (sortAsc ? " ▲" : " ▼") : ""}
            </button>
          </div>
          <ul className="file-browser-list">
            {filteredEntries.map((entry) => (
            <li key={entry.name}>
              <button
                type="button"
                className="file-browser-entry"
                data-selected={selectedFile?.path === (currentPath ? `${currentPath}/${entry.name}` : entry.name)}
                onClick={() => void openEntry(entry)}
              >
                <span className="file-browser-entry-icon"><FileEntryIcon entry={entry} /></span>
                <span className="file-browser-entry-name">{entry.name}</span>
                {entry.type === "file" ? (
                  <small className="file-browser-entry-size">{formatSize(entry.size)}</small>
                ) : null}
                <small className="file-browser-entry-time">{formatDate(entry.modified)}</small>
              </button>
            </li>
            ))}
          </ul>
        </div>
      )}

        </>
      ) : null}
      {(viewMode === "preview" || split) && selectedFile ? (
        <div className="file-browser-viewer">
          {selectedFile.loading ? (
            <p className="inspector-muted">{t("fileBrowser.loadingFile")}</p>
          ) : selectedFile.error ? (
            <p className="inspector-error">{selectedFile.error}</p>
          ) : isImageFile(selectedFile.mimeType) ? (
            <WorkspaceImage
              className="file-browser-image"
              workspaceId={workspaceId}
              path={selectedFile.path}
              executionRootId={executionRootId}
              alt={selectedFile.path}
              onLoad={(e) => {
                const img = e.currentTarget;
                if (img.naturalWidth && img.naturalHeight) {
                  setImageDims({ width: img.naturalWidth, height: img.naturalHeight });
                }
              }}
            />
          ) : isVideoFile(selectedFile.mimeType, selectedFile.path) ? (
            <div className="file-browser-media">
              {largePreviewHint ? <p className="inspector-muted">{largePreviewHint}</p> : null}
              <video className="file-browser-video" controls preload="metadata" src={selectedFileUrl} />
            </div>
          ) : isAudioFile(selectedFile.mimeType, selectedFile.path) ? (
            <div className="file-browser-media">
              {largePreviewHint ? <p className="inspector-muted">{largePreviewHint}</p> : null}
              <audio className="file-browser-audio" controls preload="metadata" src={selectedFileUrl} />
            </div>
          ) : isPdfFile(selectedFile.mimeType, selectedFile.path) ? (
            <div className="file-browser-media">
              {largePreviewHint ? <p className="inspector-muted">{largePreviewHint}</p> : null}
              <iframe className="file-browser-pdf" src={selectedFileUrl} title={selectedFile.path} />
            </div>
          ) : selectedFile.content != null ? (
            <>
              {selectedFile.truncated ? (
                <p className="inspector-muted file-browser-truncated">
                  {selectedFile.totalSize
                    ? t("fileBrowser.fileTruncated", { size: formatSize(selectedFile.totalSize) })
                    : t("fileBrowser.fileTruncatedNoSize")}
                  {" "}
                  <button type="button" className="file-browser-truncated-action" disabled={!selectedWebUrl} onClick={openSelectedFileInNewTab}>
                    {t("fileBrowser.openInNewTab")}
                  </button>
                  <button type="button" className="file-browser-truncated-action" onClick={downloadSelectedFile}>
                    {t("fileBrowser.download")}
                  </button>
                </p>
              ) : null}
              {isMarkdownFile && showRendered ? (
                <div className="file-browser-markdown markdown-content" ref={contentScrollRef} data-file-scroll={`rendered:${selectedFile.path}`}>
                  <MarkdownContent text={selectedFile.content} fragment={fragment} onFragmentChange={setFragment} onOpenFile={openMarkdownFile}
                    baseFile={effectiveRootId && selectedFile.absolutePath ? {
                      workspaceId, executionRootId: effectiveRootId, path: selectedFile.path,
                      absolutePath: selectedFile.absolutePath, rootKind: selectedFile.rootKind ?? listing?.rootKind ?? "", kind: "file",
                    } : undefined} />
                </div>
              ) : (
                <div
                  className="file-browser-code"
                  data-file-scroll={`source:${selectedFile.path}`}
                  ref={contentScrollRef}
                  dangerouslySetInnerHTML={{ __html: highlightedHtml ?? plainCodeHtml }}
                />
              )}
            </>
          ) : (
            <div>
              <p className="inspector-muted">
                {selectedFile.mimeType
                  ? t("fileBrowser.binaryFile", { type: selectedFile.mimeType })
                  : t("fileBrowser.binaryFileUnknown")}
              </p>
            </div>
          )}
        </div>
      ) : viewMode === "preview" ? (
        <p className="inspector-muted">{t("fileBrowser.noFileSelected")}</p>
      ) : null}
      </div>
    </div>
  );
}
