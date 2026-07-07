import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { memo, useEffect, useState, type ImgHTMLAttributes } from "react";

import { useRuntimeStore } from "../runtime/runtime-store";

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

export function WorkspaceImage({ workspaceId, path, executionRootId, alt, ...props }: WorkspaceImageProps) {
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
  return <img {...props} src={objectUrl} alt={alt ?? path} />;
}

function MarkdownContentView({ text, compact = false }: MarkdownContentProps) {
  return (
    <div className={`markdown-content${compact ? " compact" : ""}`}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          a: ({ children, ...props }) => (
            <a {...props} rel="noreferrer" target="_blank">
              {children}
            </a>
          ),
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
