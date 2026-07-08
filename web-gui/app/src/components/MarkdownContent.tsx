import ReactMarkdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import { SKIP, visit } from "unist-util-visit";
import { memo, useEffect, useState, type ImgHTMLAttributes, type ReactNode } from "react";

import { useRuntimeStore } from "../runtime/runtime-store";

const WORKSPACE_URL_RE = /workspace:\/\/[^\s<>"')\]]+/g;

interface MarkdownContentProps {
  text: string;
  compact?: boolean;
}

export interface WorkspaceImageRef {
  workspaceId: string;
  path: string;
}

export function parseWorkspaceImageRef(src: string | undefined): WorkspaceImageRef | undefined {
  if (!src?.startsWith("workspace://")) return undefined;
  const value = src.slice("workspace://".length);
  const pathStart = value.indexOf("/");
  if (pathStart <= 0) return undefined;

  const workspaceId = value.slice(0, pathStart);
  const rawPath = value.slice(pathStart + 1).split(/[?#]/, 1)[0];
  if (!workspaceId || !rawPath) return undefined;

  try {
    const path = rawPath
      .split("/")
      .filter(Boolean)
      .map((part) => {
        const decoded = decodeURIComponent(part);
        if (decoded === "..") throw new Error("workspace image path escapes workspace");
        return decoded;
      })
      .join("/");
    if (!path) return undefined;
    return { workspaceId, path };
  } catch {
    return undefined;
  }
}

export function markdownUrlTransform(url: string, key: string): string {
  if ((key === "src" || key === "href") && parseWorkspaceImageRef(url)) return url;
  // Keep workspace:// URLs as-is (even invalid ones) so the renderer
  // component can decide whether to render as link or plain text.
  if ((key === "src" || key === "href") && url.startsWith("workspace://")) return url;
  return defaultUrlTransform(url);
}

export function resolveWorkspaceRelativePath(baseFilePath: string, src: string | undefined): string | undefined {
  if (!src || /^[a-z][a-z\d+.-]*:/i.test(src) || src.startsWith("//")) return undefined;
  const pathOnly = src.split(/[?#]/, 1)[0];
  if (!pathOnly) return undefined;

  const parts = pathOnly.startsWith("/") ? [] : baseFilePath.split("/").filter(Boolean).slice(0, -1);
  try {
    for (const rawPart of pathOnly.split("/")) {
      if (!rawPart || rawPart === ".") continue;
      const part = decodeURIComponent(rawPart);
      if (part === ".") continue;
      if (part === "..") {
        if (parts.length === 0) return undefined;
        parts.pop();
        continue;
      }
      parts.push(part);
    }
  } catch {
    return undefined;
  }

  return parts.length > 0 ? parts.join("/") : undefined;
}

interface WorkspaceImageProps extends Omit<ImgHTMLAttributes<HTMLImageElement>, "src"> {
  workspaceId: string;
  path: string;
  executionRootId?: string;
}

export function WorkspaceImage({
  workspaceId,
  path,
  executionRootId,
  alt,
  ...props
}: WorkspaceImageProps) {
  const fetchWorkspaceFileBlob = useRuntimeStore((s) => s.fetchWorkspaceFileBlob);
  const [objectUrl, setObjectUrl] = useState<string>();
  const [error, setError] = useState<string>();

  useEffect(() => {
    let cancelled = false;
    let createdUrl: string | undefined;
    setObjectUrl(undefined);
    setError(undefined);

    void fetchWorkspaceFileBlob(workspaceId, path, executionRootId)
      .then((blob) => {
        const nextUrl = URL.createObjectURL(blob);
        if (cancelled) {
          URL.revokeObjectURL(nextUrl);
          return;
        }
        createdUrl = nextUrl;
        setObjectUrl(nextUrl);
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });

    return () => {
      cancelled = true;
      if (createdUrl) URL.revokeObjectURL(createdUrl);
    };
  }, [fetchWorkspaceFileBlob, workspaceId, path, executionRootId]);

  if (error) {
    return (
      <span className="workspace-image-error" title={error}>
        {alt ?? path} image unavailable
      </span>
    );
  }
  if (!objectUrl) {
    return <span className="workspace-image-loading">Loading image…</span>;
  }
  return (
    <span className="workspace-image-frame">
      <img
        {...props}
        src={objectUrl}
        alt={alt ?? path}
      />
    </span>
  );
}

interface WorkspaceFileLinkProps {
  href: string;
  children?: ReactNode;
}

function WorkspaceFileLink({ href, children }: WorkspaceFileLinkProps) {
  const showFileBrowser = useRuntimeStore((s) => s.showFileBrowser);
  const selectedAgentId = useRuntimeStore((s) => s.selectedAgentId);
  const workspaceRef = parseWorkspaceImageRef(href);
  if (!workspaceRef) {
    return (
      <a href={href} rel="noreferrer" target="_blank">
        {children}
      </a>
    );
  }
  return (
    <a
      href={href}
      onClick={(e) => {
        e.preventDefault();
        showFileBrowser(selectedAgentId, workspaceRef.workspaceId, undefined, undefined, workspaceRef.path);
      }}
      rel="noreferrer"
    >
      {children}
    </a>
  );
}

/**
 * GFM autolink only covers http(s)/www. URLs. This plugin extends
 * autolinking to bare `workspace://` URLs so that they render as
 * clickable links in markdown text.
 */
export function remarkWorkspaceAutolink() {
  return (tree: import("unist").Node) => {
    visit(tree, "text", (node: any, index: number | null, parent: any) => {
      if (index === null || !parent || parent.type === "link") return;
      const value: string = node.value;
      if (!value.includes("workspace://")) return;

      const segments: any[] = [];
      let last = 0;
      WORKSPACE_URL_RE.lastIndex = 0;
      let m: RegExpExecArray | null;
      while ((m = WORKSPACE_URL_RE.exec(value)) !== null) {
        // Strip trailing punctuation that is unlikely to be part of the URL
        const url = m[0].replace(/[.,;!?:]+$/, "");
        // Only autolink valid workspace:// URLs
        if (!parseWorkspaceImageRef(url)) continue;
        if (m.index > last) segments.push({ type: "text", value: value.slice(last, m.index) });
        segments.push({ type: "link", url, children: [{ type: "text", value: url }] });
        const trailing = m[0].slice(url.length);
        if (trailing) segments.push({ type: "text", value: trailing });
        last = m.index + m[0].length;
      }
      if (segments.length === 0) return;
      if (last < value.length) segments.push({ type: "text", value: value.slice(last) });

      parent.children.splice(index, 1, ...segments);
      return [SKIP, index + segments.length] as [typeof SKIP, number];
    });
  };
}

function MarkdownContentView({ text, compact = false }: MarkdownContentProps) {
  return (
    <div className={`markdown-content${compact ? " compact" : ""}`}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkWorkspaceAutolink]}
        urlTransform={markdownUrlTransform}
        components={{
          a: ({ children, href, ...props }) => {
            if (href && parseWorkspaceImageRef(href)) {
              return <WorkspaceFileLink href={href}>{children}</WorkspaceFileLink>;
            }
            // Invalid workspace:// URLs should not render as clickable links
            if (href?.startsWith("workspace://")) {
              return <>{children}</>;
            }
            return (
              <a {...props} href={href} rel="noreferrer" target="_blank">
                {children}
              </a>
            );
          },
          img: ({ src, alt, ...props }) => {
            const workspaceRef = parseWorkspaceImageRef(src);
            if (!workspaceRef) {
              return <img {...props} src={src} alt={alt ?? ""} />;
            }
            return (
              <WorkspaceImage
                {...props}
                workspaceId={workspaceRef.workspaceId}
                path={workspaceRef.path}
                alt={alt ?? workspaceRef.path}
                title={src}
              />
            );
          },
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}

export const MarkdownContent = memo(MarkdownContentView);
