import { describe, expect, it, vi } from "vitest";
import { classifyReference, createHeadingSlugger, filePreviewLocation, filePreviewUrl } from "./references";
import { collectMarkdownReferences } from "./markdown";
import { ReferenceCache } from "./cache";
import type { FileReference, ResolvedFileLocation, ResolveFileReferencesResponse } from "../../runtime/types";

const base: ResolvedFileLocation = { workspaceId: "ws", executionRootId: "worktree:/tmp/wt", path: "docs/base.md", absolutePath: "/tmp/wt/docs/base.md", rootKind: "git_worktree_root", kind: "file" };
const resolved = { status: "resolved" as const, location: base };
const item = (key: string) => ({ key, reference: { type: "absolute_path", absolutePath: `/${key}` } as FileReference });

describe("shared Markdown references", () => {
  it("decodes URL paths once; code paths stay literal", () => {
    const path = "/tmp/空 格(1)%25#?.md";
    const encoded = path.split("/").map(encodeURIComponent).join("/");
    expect(classifyReference(encoded + "#section", base)).toEqual({ kind: "file", reference: { type: "absolute_path", absolutePath: path }, fragment: "section" });
    expect(classifyReference(path, base, true)).toEqual({ kind: "file", reference: { type: "absolute_path", absolutePath: path }, fragment: undefined });
    expect(classifyReference("/tmp/literal%2520.md")).toMatchObject({ reference: { absolutePath: "/tmp/literal%20.md" } });
    expect(classifyReference("/tmp/bad%xx").kind).toBe("error");
  });
  it("normalizes local file URLs into absolute paths without weakening remote-host checks", () => {
    expect(classifyReference("file:///tmp/report%20draft%2520%23.md#section%201")).toEqual({
      kind: "file",
      reference: { type: "absolute_path", absolutePath: "/tmp/report draft%20#.md" },
      fragment: "section 1",
    });
    expect(classifyReference("file://localhost/tmp/report.md")).toEqual({
      kind: "file",
      reference: { type: "absolute_path", absolutePath: "/tmp/report.md" },
      fragment: undefined,
    });
    for (const value of [
      "file://nas/share/report.md",
      "file://127.0.0.1/tmp/report.md",
      "file://user@localhost/tmp/report.md",
      "file:///tmp/report.md?download=1",
      "file:///tmp/bad%xx.md",
      "file:///tmp/bad%00.md",
    ]) expect(classifyReference(value).kind).toBe("error");
  });
  it("requires full document provenance and does not reinterpret absolute paths", () => {
    expect(classifyReference("./a.md")).toMatchObject({ kind: "error", message: "Missing file location context" });
    expect(classifyReference("../a.md", base)).toMatchObject({ reference: { type: "relative_path", relativePath: "../a.md", baseFile: base } });
    expect(classifyReference("/a.md", base)).toMatchObject({ reference: { type: "absolute_path", absolutePath: "/a.md" } });
    expect(classifyReference("#中文", base)).toEqual({ kind: "anchor", fragment: "中文" });
  });
  it("keeps historical URIs intact and rejects unsupported queries and schemes", () => {
    const uri = "workspace://ws/a%2520.md?root=worktree%3Aone#part";
    expect(classifyReference(uri)).toMatchObject({ reference: { type: "workspace_uri", workspaceUri: uri }, fragment: "part" });
    for (const value of ["a.md?download=1", "workspace://ws/a?root=x&other=y", "workspace://ws/a?root=x&root=y", "javascript:alert(1)", "C:\\foo"]) expect(classifyReference(value, base).kind).toBe("error");
    for (const value of ["https://example.com/a?b#c", "http://example.com", "mailto:user@example.com"]) expect(classifyReference(value).kind).toBe("external");
  });
  it("collects links, images, reference-style links and literal inline code, not prose or fenced code", () => {
    const text = '[one](/tmp/a.md) ![image](/tmp/a.md) [again][ref] ` /tmp/空 格%20#?.md ` `./child.md`\n\nworkspace://ws/old.md?root=worktree%3Aone\n\n/tmp/plain.md\n\n```\n/tmp/code.md\nworkspace://ws/code.md\n```\n\n[ref]: /tmp/a.md';
    const entries = [...collectMarkdownReferences(text, base).values()];
    expect(entries).toHaveLength(4);
    expect(entries.map((entry) => entry.value)).toContainEqual({ kind: "file", reference: { type: "absolute_path", absolutePath: "/tmp/空 格%20#?.md" }, fragment: undefined });
    expect(collectMarkdownReferences('`./child.md`')).toHaveLength(0);
  });
  it("collects file URLs from Markdown links and inline code", () => {
    const entries = [...collectMarkdownReferences(
      "[report](file:///tmp/report%20draft.md#summary) `file://localhost/tmp/other%20report.md#details`",
    ).values()];
    expect(entries.map((entry) => entry.value)).toEqual([
      { kind: "file", reference: { type: "absolute_path", absolutePath: "/tmp/report draft.md" }, fragment: "summary" },
      { kind: "file", reference: { type: "absolute_path", absolutePath: "/tmp/other report.md" }, fragment: "details" },
    ]);
  });
  it("makes stable Unicode heading slugs, including collisions", () => {
    const slug = createHeadingSlugger();
    expect(["Hello, World!", "Hello World", "hello-world-1", "中文 标题"].map(slug)).toEqual(["hello-world", "hello-world-1", "hello-world-1-1", "中文-标题"]);
  });
  it("rejects incomplete GUI locators instead of emitting an unusable link", () => {
    expect(() => filePreviewUrl({ ...base, executionRootId: "" })).toThrow("execution root");
    expect(() => filePreviewUrl({ ...base, workspaceId: "" })).toThrow("workspace");
  });
  it("round-trips GUI links without losing root, encoded filename characters or fragment", () => {
    const file = { ...base, path: "空 格/(a)%20#?.md" };
    const url = new URL(filePreviewUrl(file, "中文"), "https://holon.test");
    expect(url.pathname).toBe("/files");
    expect(filePreviewLocation(url.search)).toEqual({ workspaceId: file.workspaceId, executionRootId: file.executionRootId, path: file.path });
    expect(decodeURIComponent(url.hash.slice(1))).toBe("中文");
    expect(filePreviewLocation("?workspace=ws&path=a")).toBeUndefined();
  });
});

describe("reference cache", () => {
  it("deduplicates and batches at 64, including overlapping callers and streamed additions", async () => {
    const cache = new ReferenceCache(); cache.setScope("one");
    const resolver = vi.fn(async (refs: FileReference[]) => ({ results: refs.map(() => resolved) }));
    const items = Array.from({ length: 130 }, (_, i) => item(String(i)));
    await Promise.all([cache.resolve("one", [...items, items[0]], resolver), cache.resolve("one", [items[0]], resolver)]);
    expect(resolver.mock.calls.map(([refs]) => refs.length)).toEqual([64, 64, 2]);
    await cache.resolve("one", [...items, item("new")], resolver);
    expect(resolver.mock.calls.at(-1)?.[0]).toHaveLength(1);
  });
  it("expires successes, bounds entries and retries failures", async () => {
    let now = 0;
    const cache = new ReferenceCache(() => now); cache.setScope("one");
    const resolver = vi.fn(async (refs: FileReference[]) => ({ results: refs.map(() => resolved) }));
    await cache.resolve("one", [item("a")], resolver);
    now = 30_001;
    await cache.resolve("one", [item("a")], resolver);
    expect(resolver).toHaveBeenCalledTimes(2);
    await cache.resolve("one", Array.from({ length: 513 }, (_, i) => item(String(i))), resolver);
    const calls = resolver.mock.calls.length;
    await cache.resolve("one", [item("0")], resolver);
    expect(resolver).toHaveBeenCalledTimes(calls + 1);
    const failure = vi.fn(async () => ({ results: [{ status: "unresolved" as const, reason: "not_found", message: "Missing" }] }));
    await cache.resolve("one", [item("missing")], failure);
    await cache.resolve("one", [item("missing")], failure);
    expect(failure).toHaveBeenCalledTimes(2);
  });
  it("drops late replies after identity changes, including identity A to B to A", async () => {
    const cache = new ReferenceCache(); cache.setScope("one");
    let finish!: (result: ResolveFileReferencesResponse) => void;
    const pending = cache.resolve("one", [item("a")], () => new Promise((resolve) => { finish = resolve; }));
    cache.setScope("two"); cache.setScope("one");
    finish({ results: [resolved] });
    expect(await pending).toHaveLength(0);
    const resolver = vi.fn(async () => ({ results: [resolved] }));
    await cache.resolve("one", [item("a")], resolver);
    expect(resolver).toHaveBeenCalledTimes(1);
    expect(await cache.resolve("old", [item("a")], resolver)).toHaveLength(0);
  });
});
