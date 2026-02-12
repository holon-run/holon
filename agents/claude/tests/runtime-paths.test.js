import { test, describe, afterEach } from "node:test";
import assert from "node:assert";
import path from "path";
import { resolveRuntimePaths } from "../dist/runtimePaths.js";

const managedEnvKeys = [
  "HOLON_WORKSPACE_DIR",
  "HOLON_INPUT_DIR",
  "HOLON_OUTPUT_DIR",
  "HOLON_STATE_DIR",
  "HOLON_AGENT_HOME",
];

afterEach(() => {
  for (const key of managedEnvKeys) {
    delete process.env[key];
  }
});

describe("resolveRuntimePaths", () => {
  test("fails fast when required env vars are absent", () => {
    assert.throws(() => resolveRuntimePaths({}), /missing required environment variable: HOLON_WORKSPACE_DIR/);
  });

  test("reads and trims HOLON_* path env vars", () => {
    const paths = resolveRuntimePaths({
      HOLON_WORKSPACE_DIR: " /tmp/ws ",
      HOLON_INPUT_DIR: "/tmp/input",
      HOLON_OUTPUT_DIR: "/tmp/out",
      HOLON_STATE_DIR: "/tmp/state",
      HOLON_AGENT_HOME: "/tmp/home",
    });
    assert.strictEqual(paths.workspaceDir, "/tmp/ws");
    assert.strictEqual(paths.inputDir, "/tmp/input");
    assert.strictEqual(paths.outputDir, "/tmp/out");
    assert.strictEqual(paths.stateDir, "/tmp/state");
    assert.strictEqual(paths.agentHome, "/tmp/home");
    assert.strictEqual(paths.specPath, path.join("/tmp/input", "spec.yaml"));
    assert.strictEqual(paths.systemPromptPath, path.join("/tmp/input", "prompts", "system.md"));
    assert.strictEqual(paths.userPromptPath, path.join("/tmp/input", "prompts", "user.md"));
    assert.strictEqual(paths.eventPayloadPath, path.join("/tmp/input", "context", "event.json"));
  });
});
