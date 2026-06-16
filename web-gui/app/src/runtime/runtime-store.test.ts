import { afterEach, describe, expect, it, vi } from "vitest";

import { readStoredRuntimeConnectionConfig, writeStoredRuntimeConnectionConfig } from "./runtime-store";

class MemoryStorage implements Storage {
  private readonly items = new Map<string, string>();

  get length() {
    return this.items.size;
  }

  clear(): void {
    this.items.clear();
  }

  getItem(key: string): string | null {
    return this.items.get(key) ?? null;
  }

  key(index: number): string | null {
    return Array.from(this.items.keys())[index] ?? null;
  }

  removeItem(key: string): void {
    this.items.delete(key);
  }

  setItem(key: string, value: string): void {
    this.items.set(key, value);
  }
}

function installWindow(localStorage: Storage, sessionStorage: Storage) {
  vi.stubGlobal("window", {
    clearTimeout: () => undefined,
    localStorage,
    sessionStorage,
  });
}

describe("runtime connection storage", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("keeps active runtime connections isolated per window session", () => {
    const sharedLocalStorage = new MemoryStorage();
    const remoteWindowSession = new MemoryStorage();
    const localWindowSession = new MemoryStorage();

    installWindow(sharedLocalStorage, remoteWindowSession);
    writeStoredRuntimeConnectionConfig({
      mode: "remote",
      baseUrl: "http://remote.example:7878/",
      token: "remote-token",
    });

    installWindow(sharedLocalStorage, localWindowSession);
    expect(readStoredRuntimeConnectionConfig()).toEqual({ mode: "local" });
    writeStoredRuntimeConnectionConfig({ mode: "local" });

    installWindow(sharedLocalStorage, remoteWindowSession);
    expect(readStoredRuntimeConnectionConfig()).toEqual({
      mode: "remote",
      baseUrl: "http://remote.example:7878",
      token: "remote-token",
    });

    installWindow(sharedLocalStorage, localWindowSession);
    expect(readStoredRuntimeConnectionConfig()).toEqual({ mode: "local" });
  });

  it("retains saved remote tokens without making new windows remote by default", () => {
    const sharedLocalStorage = new MemoryStorage();
    const firstWindowSession = new MemoryStorage();
    const secondWindowSession = new MemoryStorage();

    installWindow(sharedLocalStorage, firstWindowSession);
    writeStoredRuntimeConnectionConfig({
      mode: "remote",
      baseUrl: "http://remote.example:7878",
      token: "saved-token",
    });

    installWindow(sharedLocalStorage, secondWindowSession);
    expect(readStoredRuntimeConnectionConfig()).toEqual({ mode: "local" });
    writeStoredRuntimeConnectionConfig({ mode: "remote", baseUrl: "http://remote.example:7878" });

    expect(readStoredRuntimeConnectionConfig()).toEqual({
      mode: "remote",
      baseUrl: "http://remote.example:7878",
      token: "saved-token",
    });
  });
});
