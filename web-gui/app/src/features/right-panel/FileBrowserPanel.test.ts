import { describe, expect, it } from "vitest";

import {
  compareFileEntries,
  formatSize,
  isAudioFile,
  isLargePreview,
  isPdfFile,
  isVideoFile,
  type FileSortKey,
} from "./FileBrowserPanel";
import type { WorkspaceFileEntry } from "../../runtime/types";

function entry(partial: Partial<WorkspaceFileEntry> & { name: string }): WorkspaceFileEntry {
  return { type: "file", size: 0, ...partial };
}

describe("media type detection", () => {
  it("detects video by mime type and extension", () => {
    expect(isVideoFile("video/mp4")).toBe(true);
    expect(isVideoFile(undefined, "clip.webm")).toBe(true);
    expect(isVideoFile(undefined, "clip.MOV")).toBe(true);
    expect(isVideoFile("text/plain", "notes.txt")).toBe(false);
  });

  it("detects audio by mime type and extension", () => {
    expect(isAudioFile("audio/mpeg")).toBe(true);
    expect(isAudioFile(undefined, "track.flac")).toBe(true);
    expect(isAudioFile(undefined, "track.ogg")).toBe(true);
    expect(isAudioFile("video/mp4", "clip.mp4")).toBe(false);
  });

  it("detects pdf by mime type and extension", () => {
    expect(isPdfFile("application/pdf")).toBe(true);
    expect(isPdfFile(undefined, "report.PDF")).toBe(true);
    expect(isPdfFile("text/plain", "report.txt")).toBe(false);
  });
});

describe("formatSize", () => {
  it("renders byte, kilobyte, megabyte, and gigabyte tiers", () => {
    expect(formatSize(512)).toBe("512 B");
    expect(formatSize(2048)).toBe("2.0 KB");
    expect(formatSize(3 * 1024 * 1024)).toBe("3.0 MB");
    expect(formatSize(2.5 * 1024 * 1024 * 1024)).toBe("2.50 GB");
  });

  it("flags previews over one gigabyte as large", () => {
    expect(isLargePreview(1024 * 1024 * 1024)).toBe(false);
    expect(isLargePreview(1024 * 1024 * 1024 + 1)).toBe(true);
    expect(isLargePreview(undefined)).toBe(false);
  });
});

describe("compareFileEntries", () => {
  const dir = entry({ name: "zzz", type: "directory" });
  const small = entry({ name: "big.txt", size: 900, modified: 200 });
  const big = entry({ name: "small.txt", size: 100, modified: 400 });

  it("keeps directories before files regardless of sort key", () => {
    for (const key of ["name", "size", "modified"] as FileSortKey[]) {
      expect(compareFileEntries(big, dir, key, true)).toBeGreaterThan(0);
      expect(compareFileEntries(dir, big, key, false)).toBeLessThan(0);
    }
  });

  it("sorts by size with descending inversion", () => {
    expect(compareFileEntries(small, big, "size", true)).toBeGreaterThan(0);
    expect(compareFileEntries(small, big, "size", false)).toBeLessThan(0);
  });

  it("sorts by modified timestamp", () => {
    expect(compareFileEntries(small, big, "modified", true)).toBeLessThan(0);
    expect(compareFileEntries(small, big, "modified", false)).toBeGreaterThan(0);
  });

  it("falls back to name when keys tie", () => {
    const a = entry({ name: "a.txt", size: 10, modified: 100 });
    const b = entry({ name: "b.txt", size: 10, modified: 100 });
    expect(compareFileEntries(a, b, "size", true)).toBeLessThan(0);
    expect(compareFileEntries(a, b, "size", false)).toBeLessThan(0);
  });
});
