import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { memo, useEffect, useState, useMemo, useRef, useId, type ImgHTMLAttributes, type ReactNode } from "react";

import { useRuntimeStore } from "../runtime/runtime-store";
import type { RuntimeCitation, WorkspaceFileLocation, ResolvedFileLocation } from "../runtime/types";
import { collectMarkdownReferences, remarkFileReferences } from "./file-references/markdown";
import { useFileIdentity, useReferences } from "./file-references/use-references";
import { filePreviewUrl, type FileTarget } from "./file-references/references";

interface MarkdownContentProps {
  text: string;
  citations?: RuntimeCitation[];
  compact?: boolean;
  baseFile?: ResolvedFileLocation;
  fragment?: string;
  onOpenFile?: (target: FileTarget) => void;
}

export type WorkspaceImageRef = WorkspaceFileLocation;

export function parseWorkspaceImageRef(src: string | undefined): WorkspaceImageRef | undefined {
  if (!src?.startsWith("workspace://")) return undefined;
  const value = src.slice("workspace://".length);
  const pathStart = value.indexOf("/");
  if (pathStart <= 0) return undefined;

  const workspaceId = value.slice(0, pathStart);
  const remainder = value.slice(pathStart + 1).split("#", 1)[0];
  const queryStart = remainder.indexOf("?");
  const rawPath = queryStart >= 0 ? remainder.slice(0, queryStart) : remainder;
  if (!workspaceId || !rawPath) return undefined;

  // Opaque execution-root token from `?root=`; percent-decode leniently the
  // same way the runtime-side parser does.
  let executionRootId: string | undefined;
  if (queryStart >= 0) {
    for (const pair of remainder.slice(queryStart + 1).split("&")) {
      const eq = pair.indexOf("=");
      if (
        eq <= 0 ||
        pair.slice(0, eq) !== "root" ||
        !pair.slice(eq + 1) ||
        executionRootId !== undefined
      ) {
        return undefined;
      }
      try {
        executionRootId = decodeURIComponent(pair.slice(eq + 1));
      } catch {
        return undefined;
      }
    }
  }

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
    return executionRootId ? { workspaceId, path, executionRootId } : { workspaceId, path };
  } catch {
    return undefined;
  }
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
  const scope = useFileIdentity();
  const identity = JSON.stringify([scope, workspaceId, executionRootId, path]);
  const [loaded, setLoaded] = useState<{ identity: string; url: string }>();
  const objectUrl = loaded?.identity === identity ? loaded.url : undefined;
  const [attempt, setAttempt] = useState(0);
  const [failure, setFailure] = useState<{ identity: string; message: string }>();
  const error = failure?.identity === identity ? failure.message : undefined;

  useEffect(() => {
    let cancelled = false;
    let createdUrl: string | undefined;
    setLoaded(undefined);
    setFailure(undefined);

    void fetchWorkspaceFileBlob({ workspaceId, path, executionRootId })
      .then((blob) => {
        const nextUrl = URL.createObjectURL(blob);
        if (cancelled) {
          URL.revokeObjectURL(nextUrl);
          return;
        }
        createdUrl = nextUrl;
        setLoaded({ identity, url: nextUrl });
      })
      .catch((err) => {
        if (!cancelled) setFailure({ identity, message: err instanceof Error ? err.message : String(err) });
      });

    return () => {
      cancelled = true;
      if (createdUrl) URL.revokeObjectURL(createdUrl);
    };
  }, [fetchWorkspaceFileBlob, workspaceId, path, executionRootId, identity, attempt]);

  if (error) {
    return (
      <span className="workspace-image-error" title={error}>
        {alt ?? path}: {error} <button type="button" onClick={(event) => { event.preventDefault(); event.stopPropagation(); setAttempt((value) => value + 1); }}>Retry</button>
      </span>
    );
  }
  if (!objectUrl) {
    return <span className="workspace-image-loading">{alt ?? path} — Loading image…</span>;
  }
  return (
    <span className="workspace-image-frame">
      <img
        {...props}
        src={objectUrl}
        alt={alt ?? path}
        onError={() => setFailure({ identity, message: "Image could not be decoded" })}
      />
    </span>
  );
}

export function stripOpenAiCitationSentinels(text: string): string {
  const start = "\uE200cite\uE202";
  let visible = "";
  let remaining = text;
  while (true) {
    const markerStart = remaining.indexOf(start);
    if (markerStart < 0) break;
    visible += remaining.slice(0, markerStart);
    const markerBody = remaining.slice(markerStart + start.length);
    const markerEnd = markerBody.indexOf("\uE201");
    if (markerEnd >= 0) {
      remaining = markerBody.slice(markerEnd + 1);
      continue;
    }
    const tokenEnd = markerBody.search(/\s|[^A-Za-z0-9_,.:\-\uE202]/);
    remaining = tokenEnd < 0 ? "" : markerBody.slice(tokenEnd);
  }
  return `${visible}${remaining}`.replace(/[\uE200\uE201\uE202]/g, "");
}

export function safeCitation(citation: RuntimeCitation): RuntimeCitation | undefined {
  try {
    const parsed = new URL(citation.url);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return undefined;
    const title = citation.title?.trim();
    return { url: parsed.toString(), title: title || parsed.hostname || parsed.toString() };
  } catch {
    return undefined;
  }
}

function MarkdownContentView({ text, citations, compact = false, baseFile, fragment, onOpenFile }: MarkdownContentProps) {
  const baseKey = JSON.stringify(baseFile);
  const base = useMemo(() => baseFile, [baseKey]);
  const visibleText = useMemo(() => stripOpenAiCitationSentinels(text), [text]);
  const references = useMemo(() => collectMarkdownReferences(visibleText, base), [visibleText, base]);
  const { results, retry } = useReferences(references);
  const prefix = `${useId()}-`;
  const container = useRef<HTMLDivElement>(null);
  const [fragmentError, setFragmentError] = useState<string>();
  const scrollToFragment = (name: string) => {
    const heading = [...(container.current?.querySelectorAll<HTMLElement>("[data-heading-slug]") ?? [])].find((node) => node.dataset.headingSlug === name);
    if (heading) { heading.scrollIntoView({ block: "start" }); setFragmentError(undefined); }
    else setFragmentError(`Section “${name}” was not found in this document`);
  };
  useEffect(() => {
    setFragmentError(undefined);
    if (fragment) scrollToFragment(fragment);
  }, [fragment, visibleText, baseKey]);
  const openFile = onOpenFile ?? ((target: FileTarget) => {
    const store = useRuntimeStore.getState();
    store.openResolvedFile(store.selectedAgentId, target);
  });
  const renderReference = (key: string, children: ReactNode, image = false, alt?: string) => {
    const entry = references.get(key)?.value;
    if (!entry) return children;
    if (entry.kind === "anchor") return <a href={`#${prefix}${entry.fragment}`} onClick={(event) => { event.preventDefault(); scrollToFragment(entry.fragment); }}>{children}</a>;
    if (entry.kind === "external") return children;
    const result = results.get(key);
    const error = entry.kind === "error" ? entry.message : result?.status === "unresolved" ? result.message : undefined;
    if (error) return <span className="file-reference-error" title={error}>{children} <small>({error})</small>{entry.kind === "file" ? <> <button type="button" onClick={(event) => { event.preventDefault(); event.stopPropagation(); retry(); }}>Retry</button></> : null}</span>;
    if (entry.kind !== "file" || result?.status !== "resolved") return <span className="file-reference-pending" title="Resolving file…">{children}</span>;
    const target = { ...result.location, fragment: entry.fragment };
    if (image) return <WorkspaceImage workspaceId={target.workspaceId} executionRootId={target.executionRootId} path={target.path} alt={alt} />;
    return <a href={filePreviewUrl(target, target.fragment)} onClick={(event) => {
      if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
      event.preventDefault(); openFile(target);
    }}>{children}</a>;
  };
  const safeCitations = Array.from(
    new Map(
      (citations ?? [])
        .map(safeCitation)
        .filter((citation): citation is RuntimeCitation => Boolean(citation))
        .map((citation) => [citation.url, citation]),
    ).values(),
  );
  return (
    <div ref={container} className={`markdown-content${compact ? " compact" : ""}`}>
      {fragmentError ? <p role="status" className="inspector-muted">{fragmentError}</p> : null}
      <ReactMarkdown
        remarkPlugins={[remarkGfm, [remarkFileReferences, { base, prefix }]]}
        components={{
          a: ({ children, href, node }) => {
            const key = node?.properties["data-file-reference"];
            if (typeof key === "string") return renderReference(key, children);
            return <a href={href} rel="noreferrer" target="_blank">{children}</a>;
          },
          img: ({ src, alt, node }) => {
            const key = node?.properties["data-file-reference"];
            if (typeof key === "string") return renderReference(key, alt || "Image", true, alt);
            return src ? <img src={src} alt={alt ?? ""} /> : <span>{alt || "Image"} — Unsupported image URL</span>;
          },
        }}
      >
        {visibleText}
      </ReactMarkdown>
      {safeCitations.length > 0 ? (
        <section className="markdown-sources" aria-label="Sources">
          <strong>Sources</strong>
          <ol>
            {safeCitations.map((citation) => (
              <li key={citation.url}>
                <a href={citation.url} rel="noreferrer noopener" target="_blank">
                  {citation.title}
                </a>
              </li>
            ))}
          </ol>
        </section>
      ) : null}
    </div>
  );
}

export const MarkdownContent = memo(MarkdownContentView);
