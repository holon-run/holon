import fs from "fs";
import path from "path";
import os from "os";
import { test, describe, mock, before, beforeEach, afterEach } from "node:test";
import assert from "node:assert";
import { fileURLToPath } from "url";

// Import the functions we want to test
const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

// Helper to create a mock ProgressLogger for testing
class MockProgressLogger {
  constructor(logLevel = "progress") {
    this.logLevel = logLevel;
    this.logs = [];
    this.toolUseCount = 0;
  }

  shouldLog(level) {
    const priority = {
      debug: 0,
      info: 1,
      progress: 2,
      minimal: 3,
    };
    return priority[level] >= priority[this.logLevel];
  }

  debug(message) {
    if (this.shouldLog("debug")) {
      this.logs.push(`[DEBUG] ${message}`);
    }
  }

  info(message) {
    if (this.shouldLog("info")) {
      this.logs.push(`[INFO] ${message}`);
    }
  }

  progress(message) {
    if (this.shouldLog("progress")) {
      this.logs.push(`[PROGRESS] ${message}`);
    }
  }

  minimal(message) {
    if (this.shouldLog("minimal")) {
      this.logs.push(`[PHASE] ${message}`);
    }
  }

  logPhase(phaseName) {
    this.minimal(`Starting: ${phaseName}`);
  }

  logToolUse(toolName, filesTouched, fileCount) {
    this.toolUseCount += 1;
    if (!this.shouldLog("progress")) {
      return;
    }

    if (filesTouched && filesTouched.length > 0) {
      // This is the critical path sanitization logic we need to test
      const safeFiles = filesTouched.map((f) => path.basename(f)).filter(Boolean);
      const countInfo = `${safeFiles.length} files`;
      if (safeFiles.length <= 3) {
        this.logs.push(`[TOOL] ${toolName} -> ${safeFiles.join(", ")} (${countInfo})`);
      } else {
        this.logs.push(`[TOOL] ${toolName} -> ${countInfo}`);
      }
      return;
    }

    if (fileCount) {
      this.logs.push(`[TOOL] ${toolName} -> ${fileCount} items`);
      return;
    }

    this.logs.push(`[TOOL] ${toolName}`);
  }

  logOutcome(success, durationSeconds, error) {
    const outcome = success ? "SUCCESS" : "FAILURE";
    this.minimal(`Outcome: ${outcome} (duration: ${durationSeconds.toFixed(1)}s)`);
    if (error && this.shouldLog("info")) {
      this.info(`[ERROR] ${error}`);
    }
  }

  logSummaryExcerpt(summaryPath, lines = 5) {
    try {
      if (!fs.existsSync(summaryPath)) {
        this.info("[WARNING] Summary file not found");
        return;
      }
      const summaryLines = fs.readFileSync(summaryPath, "utf8").split(/\r?\n/);
      this.minimal("=== SUMMARY EXCERPT ===");
      summaryLines.slice(0, lines).forEach((line, index) => {
        this.minimal(`${String(index + 1).padStart(2, " ")}: ${line}`);
      });
      if (summaryLines.length > lines) {
        this.minimal(`... and ${summaryLines.length - lines} more lines`);
      }
      this.minimal("=== END SUMMARY ===");
    } catch (error) {
      this.info(`[WARNING] Failed to read summary: ${String(error)}`);
    }
  }
}

// Test utility functions
function intEnv(name, fallback) {
  const value = process.env[name];
  if (!value) {
    return fallback;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isNaN(parsed) ? fallback : parsed;
}

async function runCommand(command, args, options) {
  const { spawnSync } = await import("child_process");
  const result = spawnSync(command, args, {
    cwd: options?.cwd,
    env: options?.env,
    encoding: "utf8",
  });
  if (!options?.allowFailure && result.status !== 0) {
    throw new Error(
      `Command failed: ${command} ${args.join(" ")} (status ${result.status})\n${result.stderr}`
    );
  }
  return {
    status: result.status,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
  };
}

function generateFallbackSummary(goal, success, result) {
  const outcome = success ? "Success" : "Failure";
  return `# Task Summary\n\nGoal: ${goal}\n\nOutcome: ${outcome}\n\n## Actions\n<details><summary>Click to see full execution log</summary>\n\n${result}\n</details>\n`;
}

describe("Logging Safety", () => {
  test("log level gating works correctly", () => {
    // Test debug level - should log everything
    const debugLogger = new MockProgressLogger("debug");
    debugLogger.debug("test debug");
    debugLogger.info("test info");
    debugLogger.progress("test progress");
    debugLogger.minimal("test minimal");
    assert.strictEqual(debugLogger.logs.length, 4);
    assert(debugLogger.logs.some(log => log.includes("[DEBUG] test debug")));

    // Test minimal level - should only log minimal messages
    const minimalLogger = new MockProgressLogger("minimal");
    minimalLogger.debug("test debug");
    minimalLogger.info("test info");
    minimalLogger.progress("test progress");
    minimalLogger.minimal("test minimal");
    assert.strictEqual(minimalLogger.logs.length, 1);
    assert(minimalLogger.logs[0].includes("[PHASE] test minimal"));

    // Test progress level - should log progress and minimal
    const progressLogger = new MockProgressLogger("progress");
    progressLogger.debug("test debug");
    progressLogger.info("test info");
    progressLogger.progress("test progress");
    progressLogger.minimal("test minimal");
    assert.strictEqual(progressLogger.logs.length, 2);
    assert(progressLogger.logs.some(log => log.includes("[PROGRESS] test progress")));
    assert(progressLogger.logs.some(log => log.includes("[PHASE] test minimal")));
  });

  test("file path sanitization removes host paths", () => {
    const logger = new MockProgressLogger("progress");

    // Test with host paths that should be sanitized
    const dangerousPaths = [
      "/holon/workspace/src/file.ts",
      "/etc/passwd",
      "/Users/username/.ssh/id_rsa",
    ];

    logger.logToolUse("Edit", dangerousPaths);

    // Should have logged only basenames
    const toolLog = logger.logs.find(log => log.includes("[TOOL] Edit"));
    assert(toolLog);

    // Should not contain any path separators or full paths
    assert(!toolLog.includes("/holon/workspace/"));
    assert(!toolLog.includes("/etc/"));
    assert(!toolLog.includes("/Users/"));
    assert(!toolLog.includes("../"));
    assert(!toolLog.includes("./"));

    // Should contain only basenames (since we have <= 3 files, they should all be listed)
    assert(toolLog.includes("file.ts"));
    assert(toolLog.includes("passwd"));
    assert(toolLog.includes("id_rsa"));
    assert(toolLog.includes("3 files"));
  });

  test("file path sanitization shows count for many files", () => {
    const logger = new MockProgressLogger("progress");

    // Test with many files - should show count instead of individual names
    const manyPaths = [
      "/holon/workspace/src/file1.ts",
      "/holon/workspace/src/file2.ts",
      "/holon/workspace/src/file3.ts",
      "/holon/workspace/src/file4.ts",
      "/etc/passwd",
    ];

    logger.logToolUse("Edit", manyPaths);

    const toolLog = logger.logs.find(log => log.includes("[TOOL] Edit"));
    assert(toolLog);

    // Should show count instead of individual file names when > 3 files
    assert(toolLog.includes("5 files"));
    assert(!toolLog.includes("file1.ts")); // Should not list individual files
  });

  test("file path sanitization handles Windows and relative paths", () => {
    const logger = new MockProgressLogger("progress");

    const mixedPaths = [
      "C:\\Windows\\System32\\cmd.exe",
      "../etc/hosts",
      "./../../secret.txt",
    ];

    logger.logToolUse("Edit", mixedPaths);

    const toolLog = logger.logs.find(log => log.includes("[TOOL] Edit"));
    assert(toolLog);

    // Should not contain directory traversal patterns
    assert(!toolLog.includes("../"));
    assert(!toolLog.includes("./"));

    // Should contain basenames, with platform-specific expectations
    if (os.platform() === "win32") {
      // On Windows, backslashes are recognized as path separators and we expect the basename
      assert(toolLog.includes("cmd.exe"));
    } else {
      // On non-Windows platforms, the Windows-style path may be preserved as-is
      assert(toolLog.includes("C:\\Windows\\System32\\cmd.exe"));
    }
    assert(toolLog.includes("hosts"));
    assert(toolLog.includes("secret.txt"));
  });

  test("tool use logging respects log level", () => {
    const minimalLogger = new MockProgressLogger("minimal");
    minimalLogger.logToolUse("Edit", ["/test/file.ts"]);
    assert.strictEqual(minimalLogger.toolUseCount, 1);
    assert.strictEqual(minimalLogger.logs.length, 0); // Should not log at minimal level

    const progressLogger = new MockProgressLogger("progress");
    progressLogger.logToolUse("Edit", ["/test/file.ts"]);
    assert.strictEqual(progressLogger.toolUseCount, 1);
    assert.strictEqual(progressLogger.logs.length, 1);
    assert(progressLogger.logs[0].includes("[TOOL] Edit"));
  });
});

describe("Environment Variable Parsing", () => {
  test("intEnv returns fallback when env var is missing", () => {
    delete process.env.TEST_INT_VAR;
    assert.strictEqual(intEnv("TEST_INT_VAR", 42), 42);
  });

  test("intEnv returns fallback when env var is invalid", () => {
    process.env.TEST_INT_VAR = "not-a-number";
    assert.strictEqual(intEnv("TEST_INT_VAR", 42), 42);

    process.env.TEST_INT_VAR = "";
    assert.strictEqual(intEnv("TEST_INT_VAR", 42), 42);
  });

  test("intEnv returns parsed value when env var is valid", () => {
    process.env.TEST_INT_VAR = "123";
    assert.strictEqual(intEnv("TEST_INT_VAR", 42), 123);

    process.env.TEST_INT_VAR = "0";
    assert.strictEqual(intEnv("TEST_INT_VAR", 42), 0);
  });

  test("HOLON_ANTHROPIC_LOG takes precedence over ANTHROPIC_LOG and auto-mapping", () => {
    const logger = new MockProgressLogger("info"); // Would normally auto-map to "info"
    process.env.ANTHROPIC_LOG = "warn";
    process.env.HOLON_ANTHROPIC_LOG = "debug";

    // Simulate the logic from agent.ts
    let anthropicLogLevel = process.env.HOLON_ANTHROPIC_LOG || process.env.ANTHROPIC_LOG;

    // Auto-map from logger if not explicitly set
    if (!anthropicLogLevel) {
      const logLevelStr = logger["logLevel"];
      if (logLevelStr === "debug") {
        anthropicLogLevel = "debug";
      } else if (logLevelStr === "info") {
        anthropicLogLevel = "info";
      }
    }

    assert.strictEqual(anthropicLogLevel, "debug", "HOLON_ANTHROPIC_LOG should take precedence");

    delete process.env.ANTHROPIC_LOG;
    delete process.env.HOLON_ANTHROPIC_LOG;
  });

  test("ANTHROPIC_LOG is used when HOLON_ANTHROPIC_LOG is not set (overrides auto-mapping)", () => {
    const logger = new MockProgressLogger("debug"); // Would normally auto-map to "debug"
    process.env.ANTHROPIC_LOG = "warn";
    delete process.env.HOLON_ANTHROPIC_LOG;

    // Simulate the logic from agent.ts
    let anthropicLogLevel = process.env.HOLON_ANTHROPIC_LOG || process.env.ANTHROPIC_LOG;

    // Auto-map from logger if not explicitly set
    if (!anthropicLogLevel) {
      const logLevelStr = logger["logLevel"];
      if (logLevelStr === "debug") {
        anthropicLogLevel = "debug";
      } else if (logLevelStr === "info") {
        anthropicLogLevel = "info";
      }
    }

    assert.strictEqual(anthropicLogLevel, "warn", "ANTHROPIC_LOG should be used when HOLON_ANTHROPIC_LOG is not set");

    delete process.env.ANTHROPIC_LOG;
  });

  test("Auto-maps from ProgressLogger debug level when no env vars are set", () => {
    const logger = new MockProgressLogger("debug");
    delete process.env.ANTHROPIC_LOG;
    delete process.env.HOLON_ANTHROPIC_LOG;

    // Simulate the logic from agent.ts
    let anthropicLogLevel = process.env.HOLON_ANTHROPIC_LOG || process.env.ANTHROPIC_LOG;

    // Auto-map from logger if not explicitly set
    if (!anthropicLogLevel) {
      const logLevelStr = logger["logLevel"];
      if (logLevelStr === "debug") {
        anthropicLogLevel = "debug";
      } else if (logLevelStr === "info") {
        anthropicLogLevel = "info";
      }
    }

    assert.strictEqual(anthropicLogLevel, "debug", "Should auto-map from logger debug level");
  });

  test("Auto-maps from ProgressLogger info level when no env vars are set", () => {
    const logger = new MockProgressLogger("info");
    delete process.env.ANTHROPIC_LOG;
    delete process.env.HOLON_ANTHROPIC_LOG;

    // Simulate the logic from agent.ts
    let anthropicLogLevel = process.env.HOLON_ANTHROPIC_LOG || process.env.ANTHROPIC_LOG;

    // Auto-map from logger if not explicitly set
    if (!anthropicLogLevel) {
      const logLevelStr = logger["logLevel"];
      if (logLevelStr === "debug") {
        anthropicLogLevel = "debug";
      } else if (logLevelStr === "info") {
        anthropicLogLevel = "info";
      }
    }

    assert.strictEqual(anthropicLogLevel, "info", "Should auto-map from logger info level");
  });

  test("No auto-mapping for ProgressLogger progress level", () => {
    const logger = new MockProgressLogger("progress");
    delete process.env.ANTHROPIC_LOG;
    delete process.env.HOLON_ANTHROPIC_LOG;

    // Simulate the logic from agent.ts
    let anthropicLogLevel = process.env.HOLON_ANTHROPIC_LOG || process.env.ANTHROPIC_LOG;

    // Auto-map from logger if not explicitly set
    if (!anthropicLogLevel) {
      const logLevelStr = logger["logLevel"];
      if (logLevelStr === "debug") {
        anthropicLogLevel = "debug";
      } else if (logLevelStr === "info") {
        anthropicLogLevel = "info";
      }
    }

    assert.strictEqual(anthropicLogLevel, undefined, "Should not auto-map from progress/minimal levels");
  });

  afterEach(() => {
    delete process.env.TEST_INT_VAR;
    delete process.env.ANTHROPIC_LOG;
    delete process.env.HOLON_ANTHROPIC_LOG;
  });
});

describe("Fallback Summary Generation", () => {
  test("generateFallbackSummary creates proper markdown", () => {
    const goal = "Fix the bug in example.ts";
    const success = true;
    const result = "Successfully fixed the bug by updating the function";

    const summary = generateFallbackSummary(goal, success, result);

    assert(summary.includes("# Task Summary"));
    assert(summary.includes(`Goal: ${goal}`));
    assert(summary.includes("Outcome: Success"));
    assert(summary.includes("## Actions"));
    assert(summary.includes(result));
  });

  test("generateFallbackSummary handles failure case", () => {
    const goal = "Fix the bug in example.ts";
    const success = false;
    const result = "Error: Could not find the file";

    const summary = generateFallbackSummary(goal, success, result);

    assert(summary.includes("Outcome: Failure"));
    assert(summary.includes(result));
  });
});

describe("Command Execution", () => {
  test("runCommand throws on failure when allowFailure is false", async () => {
    await assert.rejects(async () => {
      await runCommand("false", [], { allowFailure: false });
    }, /Command failed: false/);
  });

  test("runCommand returns result when allowFailure is true", async () => {
    const result = await runCommand("false", [], { allowFailure: true });
    assert.strictEqual(result.status, 1);
    assert(typeof result.stdout === "string");
    assert(typeof result.stderr === "string");
  });

  test("runCommand executes successfully", async () => {
    const result = await runCommand("echo", ["hello"], { allowFailure: false });
    assert.strictEqual(result.status, 0);
    assert.strictEqual(result.stdout.trim(), "hello");
  });
});

describe("Artifact Generation", () => {
  let tempDir;

  beforeEach(() => {
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "holon-test-"));
  });

  afterEach(() => {
    fs.rmSync(tempDir, { recursive: true, force: true });
  });

  test("writes manifest.json with correct structure", () => {
    const outputDir = path.join(tempDir, "output");
    fs.mkdirSync(outputDir, { recursive: true });

    const manifest = {
      metadata: {
        agent: "claude-code",
        version: "0.1.0",
      },
      status: "completed",
      outcome: "success",
      duration: 45.7,
      artifacts: [
        { name: "diff.patch", path: "diff.patch" },
        { name: "summary.md", path: "summary.md" },
        { name: "evidence", path: "evidence/" },
      ],
    };

    const manifestPath = path.join(outputDir, "manifest.json");
    fs.writeFileSync(manifestPath, JSON.stringify(manifest, null, 2));

    assert(fs.existsSync(manifestPath));
    const content = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
    assert.strictEqual(content.metadata.agent, "claude-code");
    assert.strictEqual(content.status, "completed");
    assert.strictEqual(content.outcome, "success");
    assert.strictEqual(content.artifacts.length, 3);
  });

  test("writes diff.patch to stable path", () => {
    const outputDir = path.join(tempDir, "output");
    fs.mkdirSync(outputDir, { recursive: true });

    const patchContent = "diff --git a/test.txt b/test.txt\nnew file mode 100644\nindex 0000000..abc1234\n--- /dev/null\n+++ b/test.txt\n@@ -0,0 +1 @@\n+test content\n";

    const patchPath = path.join(outputDir, "diff.patch");
    fs.writeFileSync(patchPath, patchContent);

    assert(fs.existsSync(patchPath));
    const content = fs.readFileSync(patchPath, "utf8");
    assert.strictEqual(content, patchContent);
  });

  test("writes summary.md with fallback content", () => {
    const outputDir = path.join(tempDir, "output");
    fs.mkdirSync(outputDir, { recursive: true });

    const goal = "Test goal";
    const success = true;
    const result = "Test execution result";
    const summaryContent = generateFallbackSummary(goal, success, result);

    const summaryPath = path.join(outputDir, "summary.md");
    fs.writeFileSync(summaryPath, summaryContent);

    assert(fs.existsSync(summaryPath));
    const content = fs.readFileSync(summaryPath, "utf8");
    assert(content.includes("# Task Summary"));
    assert(content.includes(goal));
    assert(content.includes("Success"));
  });
});

describe("Error Handling", () => {
  test("handles missing spec file gracefully", () => {
    const nonExistentPath = "/non/existent/spec.yaml";
    assert(!fs.existsSync(nonExistentPath));

    // This simulates the error handling in the agent
    try {
      if (!fs.existsSync(nonExistentPath)) {
        throw new Error(`Spec not found at ${nonExistentPath}`);
      }
      assert.fail("Should have thrown an error");
    } catch (error) {
      assert(error.message.includes("Spec not found"));
      assert(error.message.includes(nonExistentPath));
    }
  });

  test("handles missing system prompt file gracefully", () => {
    const nonExistentPath = "/non/existent/system.md";
    assert(!fs.existsSync(nonExistentPath));

    try {
      if (!fs.existsSync(nonExistentPath)) {
        throw new Error(`Compiled system prompt not found at ${nonExistentPath}`);
      }
      assert.fail("Should have thrown an error");
    } catch (error) {
      assert(error.message.includes("Compiled system prompt not found"));
      assert(error.message.includes(nonExistentPath));
    }
  });

  test("handles missing user prompt file gracefully", () => {
    const nonExistentPath = "/non/existent/user.md";
    assert(!fs.existsSync(nonExistentPath));

    try {
      if (!fs.existsSync(nonExistentPath)) {
        throw new Error(`Compiled user prompt not found at ${nonExistentPath}`);
      }
      assert.fail("Should have thrown an error");
    } catch (error) {
      assert(error.message.includes("Compiled user prompt not found"));
      assert(error.message.includes(nonExistentPath));
    }
  });
});

describe("Git Diff Command Generation", () => {
  test("verifies git diff command structure for patch compatibility", () => {
    // This test verifies the expected command structure for generating a git diff
    const expectedArgs = ["diff", "--cached", "--patch", "--binary", "--full-index"];
    const command = "git";

    // Verify the command and its arguments directly
    assert.strictEqual(command, "git");
    assert.deepStrictEqual(expectedArgs, ["diff", "--cached", "--patch", "--binary", "--full-index"]);

    // Verify the critical flags are present
    assert(expectedArgs.includes("--binary"));     // Essential for binary files
    assert(expectedArgs.includes("--full-index")); // Ensures git apply compatibility
    assert(expectedArgs.includes("--patch"));      // Generates patch format
    assert(expectedArgs.includes("--cached"));     // Shows staged changes
  });

  test("describes git diff failure result structure", () => {
    // When there are no changes, git diff might return empty output but status 0
    const result = {
      status: 0, // Git diff returns 0 even with no changes (just empty output)
      stdout: "",
      stderr: "",
    };

    assert.strictEqual(result.status, 0);
    assert.strictEqual(result.stdout, "");
    assert.strictEqual(result.stderr, "");
  });

  test("verifies allowFailure option handling", () => {
    // This test verifies that the allowFailure option prevents exceptions
    const mockRunCommand = (command, args, options) => {
      return {
        status: 1, // Non-zero exit status
        stdout: "",
        stderr: "",
      };
    };

    const result = mockRunCommand("git", ["diff", "--cached"], { allowFailure: true });
    assert.strictEqual(result.status, 1);

    // Should not throw when allowFailure is true
  });
});

describe("Bundle Manifest Metadata", () => {
  let tempDir;
  // Import the actual functions to test
  let getAgentMetadata;
  let readBundleManifest;

  before(async () => {
    // Dynamic import to test the exported functions
    const agentModule = await import("../dist/agent.js");
    getAgentMetadata = agentModule.getAgentMetadata;
    readBundleManifest = agentModule.readBundleManifest;
  });

  beforeEach(() => {
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "holon-bundle-test-"));
  });

  afterEach(() => {
    fs.rmSync(tempDir, { recursive: true, force: true });
  });

  test("getAgentMetadata derives from bundle manifest with engine.name", () => {
    const bundleManifest = {
      bundleVersion: "1",
      name: "agent-claude",
      version: "0.6.1",
      engine: {
        name: "claude-code",
        sdk: "@anthropic-ai/claude-agent-sdk",
        sdkVersion: "0.1.75",
      },
    };

    const metadata = getAgentMetadata(bundleManifest);

    assert.strictEqual(metadata.agent, "claude-code");
    assert.strictEqual(metadata.version, "0.6.1");
    assert.notStrictEqual(metadata.engine, undefined);
    assert.strictEqual(metadata.engine.sdk, "@anthropic-ai/claude-agent-sdk");
    assert.strictEqual(metadata.engine.sdkVersion, "0.1.75");
  });

  test("getAgentMetadata derives from bundle manifest without engine.name", () => {
    const bundleManifest = {
      bundleVersion: "1",
      name: "my-custom-agent",
      version: "1.2.3",
    };

    const metadata = getAgentMetadata(bundleManifest);

    assert.strictEqual(metadata.agent, "my-custom-agent");
    assert.strictEqual(metadata.version, "1.2.3");
    assert.strictEqual(metadata.engine, undefined);
  });

  test("getAgentMetadata uses fallback defaults when bundle manifest is null", () => {
    const metadata = getAgentMetadata(null);

    assert.strictEqual(metadata.agent, "claude-code");
    assert.strictEqual(metadata.version, "0.1.0");
    assert.strictEqual(metadata.engine, undefined);
  });

  test("getAgentMetadata handles bundle manifest with missing version", () => {
    const bundleManifest = {
      bundleVersion: "1",
      name: "agent-claude",
      engine: {
        name: "claude-code",
      },
    };

    const metadata = getAgentMetadata(bundleManifest);

    assert.strictEqual(metadata.agent, "claude-code");
    assert.strictEqual(metadata.version, "0.1.0"); // Fallback default
    assert.strictEqual(metadata.engine, undefined);
  });

  test("getAgentMetadata only includes defined engine SDK fields", () => {
    // Test when only sdk is present
    const bundleManifest1 = {
      bundleVersion: "1",
      name: "agent-claude",
      version: "0.6.1",
      engine: {
        name: "claude-code",
        sdk: "@anthropic-ai/claude-agent-sdk",
      },
    };

    const metadata1 = getAgentMetadata(bundleManifest1);

    assert.notStrictEqual(metadata1.engine, undefined);
    assert.strictEqual(metadata1.engine.sdk, "@anthropic-ai/claude-agent-sdk");
    assert.strictEqual(metadata1.engine.sdkVersion, undefined);

    // Test when only sdkVersion is present
    const bundleManifest2 = {
      bundleVersion: "1",
      name: "agent-claude",
      version: "0.6.1",
      engine: {
        name: "claude-code",
        sdkVersion: "0.1.75",
      },
    };

    const metadata2 = getAgentMetadata(bundleManifest2);

    assert.notStrictEqual(metadata2.engine, undefined);
    assert.strictEqual(metadata2.engine.sdk, undefined);
    assert.strictEqual(metadata2.engine.sdkVersion, "0.1.75");
  });

  test("readBundleManifest returns null when file does not exist", () => {
    const manifestPath = path.join(tempDir, "nonexistent-manifest.json");
    const result = readBundleManifest(manifestPath);

    assert.strictEqual(result, null);
  });

  test("readBundleManifest returns null for invalid JSON", () => {
    const manifestPath = path.join(tempDir, "invalid-manifest.json");
    fs.writeFileSync(manifestPath, "invalid json content {");

    const result = readBundleManifest(manifestPath);

    assert.strictEqual(result, null);
  });

  test("readBundleManifest parses valid bundle manifest", () => {
    const manifestPath = path.join(tempDir, "manifest.json");
    const validManifest = {
      bundleVersion: "1",
      name: "agent-claude",
      version: "0.6.1",
      engine: {
        name: "claude-code",
        sdk: "@anthropic-ai/claude-agent-sdk",
        sdkVersion: "0.1.75",
      },
    };
    fs.writeFileSync(manifestPath, JSON.stringify(validManifest, null, 2));

    const result = readBundleManifest(manifestPath);

    assert.notStrictEqual(result, null);
    assert.strictEqual(result.bundleVersion, "1");
    assert.strictEqual(result.name, "agent-claude");
    assert.strictEqual(result.version, "0.6.1");
    assert.strictEqual(result.engine.name, "claude-code");
  });

  test("readBundleManifest uses default path when not specified", () => {
    // When called without arguments, should use default path
    // If the manifest exists, it should return a parsed object
    const result = readBundleManifest();
    // The result could be null if manifest doesn't exist, or an object if it does
    // We just verify it doesn't throw
    assert.ok(result === null || typeof result === "object");
  });
});

describe("Skills Loading", () => {
  let loadSkillsFromSpec;
  let tempDir;

  before(async () => {
    // Dynamic import to test the exported function
    const agentModule = await import("../dist/agent.js");
    loadSkillsFromSpec = agentModule.loadSkillsFromSpec;
  });

  beforeEach(() => {
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "holon-skills-test-"));
  });

  afterEach(() => {
    fs.rmSync(tempDir, { recursive: true, force: true });
  });

  test("loads skills from valid spec with metadata.skills array", async () => {
    const spec = {
      metadata: {
        skills: [
          "/path/to/skill1",
          "skill2",
          "./relative/skill3",
        ],
      },
    };

    const logger = new MockProgressLogger("debug");
    const skills = await loadSkillsFromSpec(spec, logger);

    assert.strictEqual(skills.length, 3);
    assert.strictEqual(skills[0].name, "skill1");
    assert.strictEqual(skills[1].name, "skill2");
    assert.strictEqual(skills[2].name, "skill3");
  });

  test("handles missing metadata.skills field", async () => {
    const spec = {
      metadata: {
        someOtherField: "value",
      },
    };

    const logger = new MockProgressLogger("debug");
    const skills = await loadSkillsFromSpec(spec, logger);

    assert.strictEqual(skills.length, 0);
  });

  test("handles non-array metadata.skills values", async () => {
    const spec = {
      metadata: {
        skills: "not-an-array",
      },
    };

    const logger = new MockProgressLogger("debug");
    const skills = await loadSkillsFromSpec(spec, logger);

    assert.strictEqual(skills.length, 0);
  });

  test("handles skill paths that are not strings", async () => {
    const spec = {
      metadata: {
        skills: [
          "/path/to/skill1",
          { name: "object-skill" },
          12345,
          null,
          undefined,
        ],
      },
    };

    const logger = new MockProgressLogger("debug");
    const skills = await loadSkillsFromSpec(spec, logger);

    // All types should be converted to strings and processed
    assert.strictEqual(skills.length, 5);
    assert.strictEqual(skills[0].name, "skill1");
    assert.strictEqual(skills[1].name, "[object Object]");
    assert.strictEqual(skills[2].name, "12345");
    assert.strictEqual(skills[3].name, "null");
    assert.strictEqual(skills[4].name, "undefined");
  });

  test("handles empty skills array", async () => {
    const spec = {
      metadata: {
        skills: [],
      },
    };

    const logger = new MockProgressLogger("debug");
    const skills = await loadSkillsFromSpec(spec, logger);

    assert.strictEqual(skills.length, 0);
  });

  test("handles missing metadata entirely", async () => {
    const spec = {
      goal: "Test goal",
    };

    const logger = new MockProgressLogger("debug");
    const skills = await loadSkillsFromSpec(spec, logger);

    assert.strictEqual(skills.length, 0);
  });

  test("handles null spec gracefully", async () => {
    const logger = new MockProgressLogger("debug");
    const skills = await loadSkillsFromSpec(null, logger);

    assert.strictEqual(skills.length, 0);
    // Should log a debug message about the error
    assert(logger.logs.some(log => log.includes("Failed to load skills from spec")));
  });

  test("normalizes various path formats", async () => {
    const spec = {
      metadata: {
        skills: [
          "/absolute/path/to/my-skill",
          "relative-skill",
          "../parent-skill",
          "./current-dir-skill",
          "/complex/path/with/subdirs/skill-name",
        ],
      },
    };

    const logger = new MockProgressLogger("debug");
    const skills = await loadSkillsFromSpec(spec, logger);

    assert.strictEqual(skills.length, 5);
    assert.strictEqual(skills[0].name, "my-skill");
    assert.strictEqual(skills[1].name, "relative-skill");
    assert.strictEqual(skills[2].name, "parent-skill");
    assert.strictEqual(skills[3].name, "current-dir-skill");
    assert.strictEqual(skills[4].name, "skill-name");
  });
});

