/// <reference types="vitest/config" />

import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { createHash } from "node:crypto";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { defineConfig, loadEnv, type ResolvedConfig } from "vite";
import packageJson from "./package.json";

const DEFAULT_HOLON_API_PROXY_TARGET = "http://127.0.0.1:7878";
const WEB_SOURCE_FILES = [
  "index.html",
  "package-lock.json",
  "package.json",
  "tsconfig.json",
  "tsconfig.node.json",
  "vite.config.ts",
];

export default defineConfig(async ({ mode }) => {
  const env = loadEnv(mode, ".", "");
  const holonApiProxyTarget = env.HOLON_API_PROXY_TARGET || DEFAULT_HOLON_API_PROXY_TARGET;
  const sourceHash = await computeWebSourceHash();
  let outputDirectory = path.resolve("dist");

  return {
    define: {
      __HOLON_GUI_VERSION__: JSON.stringify(packageJson.version),
      __HOLON_GUI_SOURCE_HASH__: JSON.stringify(sourceHash),
      __HOLON_E2E_DIAGNOSTICS__: JSON.stringify(mode === "e2e"),
    },
    plugins: [
      tailwindcss(),
      react(),
      {
        name: "holon-web-build-identity",
        configResolved(config: ResolvedConfig) {
          outputDirectory = path.resolve(config.root, config.build.outDir);
        },
        async closeBundle() {
          await mkdir(outputDirectory, { recursive: true });
          await writeFile(
            path.join(outputDirectory, "holon-web-build.json"),
            `${JSON.stringify(
              {
                schema_version: 1,
                source_hash: sourceHash,
                web_version: packageJson.version,
              },
              null,
              2,
            )}\n`,
          );
        },
      },
    ],
    test: {
      exclude: ["e2e/**", "node_modules/**"],
    },
    server: {
      host: "127.0.0.1",
      port: 5173,
      strictPort: false,
      proxy: {
        "/api": {
          target: holonApiProxyTarget,
          changeOrigin: true,
        },
      },
    },
  };
});

async function computeWebSourceHash(): Promise<string> {
  const inputs = await collectFiles("src");
  inputs.push(...WEB_SOURCE_FILES);
  inputs.sort();

  const hash = createHash("sha256");
  for (const input of inputs) {
    hash.update(input);
    hash.update("\0");
    hash.update(await readFile(input));
    hash.update("\0");
  }
  return `sha256:${hash.digest("hex")}`;
}

async function collectFiles(directory: string): Promise<string[]> {
  const entries = await readdir(directory, { withFileTypes: true });
  const paths = await Promise.all(
    entries.map(async (entry) => {
      const entryPath = path.posix.join(directory, entry.name);
      return entry.isDirectory() ? collectFiles(entryPath) : [entryPath];
    }),
  );
  return paths.flat();
}
