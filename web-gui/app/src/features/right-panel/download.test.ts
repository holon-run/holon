import { describe, expect, it, vi } from "vitest";

import { triggerBlobDownload, triggerHrefDownload } from "./download";

function downloadEnvironment(click: () => void) {
  const anchor = {
    href: "",
    download: "",
    click: vi.fn(click),
    remove: vi.fn(),
  };
  const appendChild = vi.fn();
  const documentRef = {
    body: { appendChild },
    createElement: vi.fn(() => anchor),
  } as unknown as Pick<Document, "body" | "createElement">;
  const urlRef = {
    createObjectURL: vi.fn(() => "blob:download"),
    revokeObjectURL: vi.fn(),
  };

  return { anchor, appendChild, documentRef, urlRef };
}

describe("triggerBlobDownload", () => {
  it("clicks a temporary object URL anchor with the requested filename", () => {
    const environment = downloadEnvironment(() => undefined);
    const blob = new Blob(["report"]);

    triggerBlobDownload(blob, "report.bin", environment.documentRef, environment.urlRef);

    expect(environment.urlRef.createObjectURL).toHaveBeenCalledWith(blob);
    expect(environment.anchor.href).toBe("blob:download");
    expect(environment.anchor.download).toBe("report.bin");
    expect(environment.appendChild).toHaveBeenCalledWith(environment.anchor);
    expect(environment.anchor.click).toHaveBeenCalledOnce();
    expect(environment.anchor.remove).toHaveBeenCalledOnce();
    expect(environment.urlRef.revokeObjectURL).toHaveBeenCalledWith("blob:download");
  });

  it("cleans up the anchor and object URL when clicking fails", () => {
    const environment = downloadEnvironment(() => {
      throw new Error("click failed");
    });

    expect(() =>
      triggerBlobDownload(new Blob(["report"]), "report.bin", environment.documentRef, environment.urlRef)
    ).toThrow("click failed");
    expect(environment.anchor.remove).toHaveBeenCalledOnce();
    expect(environment.urlRef.revokeObjectURL).toHaveBeenCalledWith("blob:download");
  });
});

describe("triggerHrefDownload", () => {
  it("clicks a temporary anchor pointing at the URL without buffering", () => {
    const anchor = {
      href: "",
      rel: "",
      click: vi.fn(),
      remove: vi.fn(),
    };
    const appendChild = vi.fn();
    const documentRef = {
      body: { appendChild },
      createElement: vi.fn(() => anchor),
    } as unknown as Pick<Document, "body" | "createElement">;

    triggerHrefDownload("http://example.test/api/workspaces/ws/files/a.bin?download=true", documentRef);

    expect(anchor.href).toBe("http://example.test/api/workspaces/ws/files/a.bin?download=true");
    expect(anchor.rel).toBe("noopener");
    expect(appendChild).toHaveBeenCalledWith(anchor);
    expect(anchor.click).toHaveBeenCalledOnce();
    expect(anchor.remove).toHaveBeenCalledOnce();
  });

  it("removes the anchor when clicking fails", () => {
    const anchor = {
      href: "",
      rel: "",
      click: vi.fn(() => {
        throw new Error("click failed");
      }),
      remove: vi.fn(),
    };
    const documentRef = {
      body: { appendChild: vi.fn() },
      createElement: vi.fn(() => anchor),
    } as unknown as Pick<Document, "body" | "createElement">;

    expect(() => triggerHrefDownload("/api/files/a.bin", documentRef)).toThrow("click failed");
    expect(anchor.remove).toHaveBeenCalledOnce();
  });
});
