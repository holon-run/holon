import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { memo } from "react";

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
  try {
    const url = new URL(src);
    const workspaceId = url.hostname;
    const path = url.pathname
      .split("/")
      .filter(Boolean)
      .map((part) => decodeURIComponent(part))
      .join("/");
    if (!workspaceId || !path) return undefined;
    return { workspaceId, path };
  } catch {
    return undefined;
  }
}

function MarkdownContentView({ text, compact = false }: MarkdownContentProps) {
  const workspaceFileUrl = useRuntimeStore((s) => s.workspaceFileUrl);

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
            const resolvedSrc = workspaceFileUrl(workspaceRef.workspaceId, workspaceRef.path);
            return (
              <a href={resolvedSrc} rel="noreferrer" target="_blank" title={src}>
                <img {...props} src={resolvedSrc} alt={alt ?? workspaceRef.path} />
              </a>
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
