import { mkdir, readFile, writeFile } from "node:fs/promises";

await mkdir("dist", { recursive: true });
const browserArtifact = await readFile("dist/browser.js", "utf8");
// `browser.js` is also exported as an ESM package entry. The hosted route is
// loaded by a classic `<script>` tag, so remove TypeScript's module marker.
await writeFile(
  "dist/holon.js",
  browserArtifact.replace(/\nexport \{\};(?=\n\/\/# sourceMappingURL)/, ""),
);
