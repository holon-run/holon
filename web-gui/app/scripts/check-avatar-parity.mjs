// Run with Node 24+, matching CI. No browser, daemon, or runtime state is needed.
// Default: verify JS goldens, then compare all 1152 recipes x five expressions
// against `cargo run --quiet --example avatar_probe`. --update-fixtures only
// refreshes the small JS-derived fixture; review it whenever artwork changes.
import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { stripTypeScriptTypes } from "node:module";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const root = new URL("../../../", import.meta.url);
const source = await readFile(new URL("../src/lib/agent-avatar.ts", import.meta.url), "utf8");
const moduleText = stripTypeScriptTypes(source, { mode: "strip", sourceUrl: "agent-avatar.ts" });
const { AGENT_AVATAR_VERSION, agentAvatarIndex, renderAgentAvatar } = await import(
  `data:text/javascript;base64,${Buffer.from(moduleText).toString("base64")}`
);
const expressions = ["neutral", "focused", "questioning", "waiting", "sleeping"];
const fixturePath = new URL("tests/fixtures/agent-avatar-golden.json", root);
const edgeIds = [
  "holon-product", "holon-auto-dev", "holon-integrations", "bootstrap-agent",
  "智能助手", "agent-🤖", "", "holonbot", "Holonbot", "café", "cafe\u0301",
  "𝄞𐐀😀\u{10ffff}", "\0\n\r\t",
  '"><script>alert("avatar")</script><image href="https://evil.invalid"/>',
  "overflow-".repeat(1024),
];
const fixture = {
  version: AGENT_AVATAR_VERSION,
  cases: edgeIds.map((id) => ({
    id,
    index: agentAvatarIndex(id),
    svg: Object.fromEntries(expressions.map((expression) => [expression, renderAgentAvatar(id, expression)])),
  })),
};
if (process.argv.includes("--update-fixtures")) {
  await writeFile(fixturePath, `${JSON.stringify(fixture, null, 2)}\n`);
  console.log(`Updated JS reference: ${fixture.cases.length} IDs x ${expressions.length} expressions.`);
  process.exit(0);
}
assert.deepEqual(JSON.parse(await readFile(fixturePath, "utf8")), fixture, "JS reference fixture changed");
assert.equal(
  await readFile(new URL("docs/website/assets/holonbot.svg", root), "utf8"),
  renderAgentAvatar("holonbot"),
  "brand export changed",
);
// Cover even the retained (now duplicate) expression slots, not just visible
// shell/palette/crown combinations. This also repeatedly exercises u32 overflow.
const recipes = new Map();
for (let serial = 0; recipes.size < 1152 && serial < 100_000; serial++) {
  const id = `avatar-parity-${serial}`;
  recipes.set(agentAvatarIndex(id), id);
}
assert.equal(recipes.size, 1152, "failed to cover the complete recipe namespace");
const ids = [...edgeIds, ...[...recipes].sort(([a], [b]) => a - b).map(([, id]) => id)];
const run = spawnSync("cargo", ["run", "--quiet", "--example", "avatar_probe"], {
  cwd: fileURLToPath(root),
  input: JSON.stringify(ids),
  encoding: "utf8",
  maxBuffer: 32 * 1024 * 1024,
});
assert.ifError(run.error);
assert.equal(run.status, 0, run.stderr);
const rows = run.stdout.trimEnd().split("\n").map((line) => JSON.parse(line));
assert.equal(rows.length, ids.length * expressions.length);
for (let i = 0; i < rows.length; i++) {
  const id = ids[Math.floor(i / expressions.length)];
  const expression = expressions[i % expressions.length];
  assert.deepEqual(rows[i], {
    id, index: agentAvatarIndex(id), version: AGENT_AVATAR_VERSION,
    expression, svg: renderAgentAvatar(id, expression),
  }, `Rust/JS SVG byte mismatch at row ${i}, expression ${expression}`);
}
console.log(`Exact SVG byte parity: ${rows.length} renders; all 1152 recipes, five expressions, Unicode/overflow/attack/brand/empty IDs.`);
