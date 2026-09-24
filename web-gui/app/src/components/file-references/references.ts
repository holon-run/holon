import type { FileReference, ResolvedFileLocation, WorkspaceFileLocation, FileOpenTarget } from "../../runtime/types";

export type FileTarget = FileOpenTarget;
export type Reference =
  | { kind: "external" }
  | { kind: "anchor"; fragment: string }
  | { kind: "error"; message: string }
  | { kind: "file"; reference: FileReference; fragment?: string };

/** URL syntax is decoded here once. Inline code is already a literal host path. */
export function classifyReference(raw: string, base?: ResolvedFileLocation, literal = false): Reference {
  if (/^(https?:|mailto:)/i.test(raw)) return { kind: "external" };
  try {
    if (/^file:\/\//i.test(raw)) {
      const url = new URL(raw);
      if (url.hostname && url.hostname.toLowerCase() !== "localhost") {
        return { kind: "error", message: "Remote file URLs are not supported" };
      }
      if (url.username || url.password || url.port) {
        return { kind: "error", message: "Unsupported file URL authority" };
      }
      if (url.search) return { kind: "error", message: "File query parameters are not supported" };
      const path = decodeURIComponent(url.pathname);
      if (!path || !path.startsWith("/") || path.includes("\0")) {
        return { kind: "error", message: "Invalid file reference" };
      }
      const fragment = url.hash ? decodeURIComponent(url.hash.slice(1)) : undefined;
      return { kind: "file", reference: { type: "absolute_path", absolutePath: path }, fragment };
    }
    if (raw.startsWith("workspace://")) {
      const uri = raw.split("#", 1)[0];
      const query = uri.indexOf("?");
      if (query >= 0 && !/^root=[^&]+$/.test(uri.slice(query + 1))) {
        return { kind: "error", message: "Unsupported file query (only workspace root is supported)" };
      }
      return { kind: "file", reference: { type: "workspace_uri", workspaceUri: raw }, fragment: fragmentOf(raw) };
    }
    if (/^[a-z][a-z\d+.-]*:/i.test(raw)) return { kind: "error", message: "Unsupported file or URL scheme" };
    if (!literal && raw.startsWith("#")) return { kind: "anchor", fragment: decodeURIComponent(raw.slice(1)) };
    const pathPart = literal ? raw : raw.split("#", 1)[0];
    if (!literal && pathPart.includes("?")) return { kind: "error", message: "File query parameters are not supported" };
    const path = literal ? pathPart : decodeURIComponent(pathPart);
    if (!path || path.includes("\0")) return { kind: "error", message: "Invalid file reference" };
    const fragment = literal ? undefined : fragmentOf(raw);
    if (path.startsWith("/")) return { kind: "file", reference: { type: "absolute_path", absolutePath: path }, fragment };
    if (!base) return { kind: "error", message: "Missing file location context" };
    return { kind: "file", reference: { type: "relative_path", relativePath: path, baseFile: base }, fragment };
  } catch {
    return { kind: "error", message: "Invalid file URL encoding" };
  }
}

export function fragmentOf(raw: string): string | undefined {
  const hash = raw.indexOf("#");
  return hash < 0 ? undefined : decodeURIComponent(raw.slice(hash + 1));
}

export function isInlineReference(value: string, hasBase: boolean): boolean {
  return /^(workspace|file):\/\//i.test(value) || /^(https?:\/\/|mailto:)\S+$/i.test(value)
    || value.startsWith("/")
    || (hasBase && /^(\.\.?\/)/.test(value));
}

export function filePreviewUrl(location: WorkspaceFileLocation & { executionRootId: string }, fragment?: string): string {
  if (!location.workspaceId || !location.executionRootId) throw new Error("File preview requires a workspace and execution root");
  const query = new URLSearchParams({ workspace: location.workspaceId, root: location.executionRootId, path: location.path });
  return `/files?${query}${fragment ? `#${encodeURIComponent(fragment)}` : ""}`;
}

export function filePreviewLocation(search: string): WorkspaceFileLocation | undefined {
  const query = new URLSearchParams(search);
  const workspaceId = query.get("workspace");
  const executionRootId = query.get("root");
  const path = query.get("path");
  if (!workspaceId || !executionRootId || path === null) return undefined;
  return { workspaceId, executionRootId, path };
}

/** Per-document slugger; duplicate headings have stable numeric suffixes. */
export function createHeadingSlugger(): (text: string) => string {
  const used = new Set<string>();
  return (text) => {
    const base = text.toLowerCase().trim().replace(/[^\p{L}\p{N}\p{M}_\-\s]/gu, "").replace(/\s/g, "-");
    let slug = base;
    let suffix = 0;
    while (used.has(slug)) slug = `${base}-${++suffix}`;
    used.add(slug);
    return slug;
  };
}
