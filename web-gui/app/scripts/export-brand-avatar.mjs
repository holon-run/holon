import { readFile, writeFile } from "node:fs/promises";
import ts from "typescript";

// Use the same renderer as AgentAvatar; never maintain a separate brand drawing.
const source = await readFile(new URL("../src/lib/agent-avatar.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
});
const { renderAgentAvatar } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);
const target = new URL("../../../docs/website/assets/holonbot.svg", import.meta.url);
await writeFile(target, renderAgentAvatar("holonbot", "neutral"), "utf8");
console.log(`Exported ${target.pathname}`);
