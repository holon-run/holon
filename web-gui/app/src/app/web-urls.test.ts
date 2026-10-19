import { describe, expect, it } from "vitest";
import { classifyWebReference } from "./web-urls";
import { matchBrowserRoute, pathForRoute } from "./routes";
import { classifyReference } from "../components/file-references/references";

describe("Web URL and file path boundary", () => {
  it.each([
    "/apps/tuptup-agent-dev/voice-audition-20261019/",
    "/apps/other/not-discovered",
    "/apps/a/b/assets/试听%20(1).mp3?mode=demo%26x#中文",
    "/apps/a/b/?next=https://example.org/#part",
    "/agents/a/conversation?event_seq=1#part",
    "/files?workspace=ws&root=opaque%3Aone&path=a%23b",
    "/skills/a", "/agents/a/skills/b", "/templates/b", "/settings/",
    "https://example.org/apps/a/b?query#part", "http://elsewhere.test",
    "//example.org/apps/a/b?query#part", "mailto:user@example.org",
  ])("preserves a Web href: %s", (href) => {
    expect(classifyReference(href)).toEqual({ kind: "web", href });
  });

  it("shares exact GUI matching without accepting the dashboard fallback", () => {
    for (const href of [
      pathForRoute("dashboard"), pathForRoute("files"), pathForRoute("search"),
      pathForRoute("agent", "a"), pathForRoute("skills"), pathForRoute("skillDetail", "s"),
      pathForRoute("skillDetail", "s", undefined, undefined, "a"),
      pathForRoute("templates"), pathForRoute("templateDetail", undefined, "t"), pathForRoute("settings"),
    ]) expect(classifyWebReference(href)?.kind).toBe("web");
    expect(matchBrowserRoute({ pathname: "/unknown", search: "" })).toBeUndefined();
  });

  it.each(["/apps", "/apps/a", "/apps-evil/a/b", "/unknown", "/tmp/file.md", "/home/a.txt"])(
    "keeps unknown root paths as files: %s", (path) => {
      expect(classifyReference(path)).toMatchObject({ kind: "file", reference: { absolutePath: path } });
    },
  );

  it("keeps explicit file URLs and document-relative paths out of Web navigation", () => {
    expect(classifyReference("file:///apps/a/b")).toMatchObject({ kind: "file", reference: { absolutePath: "/apps/a/b" } });
    expect(classifyReference("?query").kind).toBe("error");
    expect(classifyReference("relative.html").kind).toBe("error");
  });

  it.each([
    "/apps/a/b/../c", "/apps/a/b/%2e%2e/c", "/apps/a/b/%252e%252e/c",
    "/apps/a%2fb/c", "/apps/a/b/%5cfile", "/apps/a/b/%255cfile",
    "/apps/a/b/%00", "/apps/a/b/%0a", "/apps/a/b/%xx",
    "/apps/a/b\\evil", "/apps/a/b/\nfile", "/apps/a/b/\tfile",
    "///evil.test/apps/a/b", "//evil.test\\@holon.invalid/apps/a/b",
    "https:evil.test", "http:\\\\evil.test", "https://example.org/\npath",
  ])("rejects URL normalization and encoding confusion: %s", (value) => {
    expect(classifyReference(value).kind).toBe("error");
  });

  it.each(["javascript:alert(1)", "JaVaScRiPt:alert(1)", "data:text/html,hello", "vbscript:x", "blob:x", "ftp://host", "tel:123"])(
    "does not add protocol support: %s", (value) => expect(classifyReference(value).kind).toBe("error"),
  );
});
