import fs from "fs";
import path from "path";
import os from "os";
import { spawnSync } from "child_process";
import { parse as parseYaml } from "yaml";
import { query } from "./claudeSdk.js";
import type { Options } from "./claudeSdk.js";
import { readBundleManifest, getAgentMetadata } from "./bundleMetadata.js";

// Re-export for testing
export { readBundleManifest, getAgentMetadata } from "./bundleMetadata.js";

enum LogLevel {
  DEBUG = "debug",
  INFO = "info",
  PROGRESS = "progress",
  MINIMAL = "minimal",
}

class ProgressLogger {
  private logLevel: LogLevel;
  private toolUseCount = 0;

  constructor(level: string) {
    const normalized = level.toLowerCase() as LogLevel;
    this.logLevel = Object.values(LogLevel).includes(normalized)
      ? normalized
      : LogLevel.PROGRESS;
  }

  private shouldLog(level: LogLevel): boolean {
    const priority: Record<LogLevel, number> = {
      [LogLevel.DEBUG]: 0,
      [LogLevel.INFO]: 1,
      [LogLevel.PROGRESS]: 2,
      [LogLevel.MINIMAL]: 3,
    };
    return priority[level] >= priority[this.logLevel];
  }

  debug(message: string): void {
    if (this.shouldLog(LogLevel.DEBUG)) {
      console.log(`[DEBUG] ${message}`);
    }
  }

  info(message: string): void {
    if (this.shouldLog(LogLevel.INFO)) {
      console.log(`[INFO] ${message}`);
    }
  }

  progress(message: string): void {
    if (this.shouldLog(LogLevel.PROGRESS)) {
      console.log(`[PROGRESS] ${message}`);
    }
  }

  minimal(message: string): void {
    if (this.shouldLog(LogLevel.MINIMAL)) {
      console.log(`[PHASE] ${message}`);
    }
  }

  logPhase(phaseName: string): void {
    this.minimal(`Starting: ${phaseName}`);
  }

  logToolUse(toolName: string, filesTouched?: string[], fileCount?: number): void {
    this.toolUseCount += 1;
    if (!this.shouldLog(LogLevel.PROGRESS)) {
      return;
    }

    if (filesTouched && filesTouched.length > 0) {
      const safeFiles = filesTouched.map((f) => path.basename(f)).filter(Boolean);
      const countInfo = `${safeFiles.length} files`;
      if (safeFiles.length <= 3) {
        console.log(`[TOOL] ${toolName} -> ${safeFiles.join(", ")} (${countInfo})`);
      } else {
        console.log(`[TOOL] ${toolName} -> ${countInfo}`);
      }
      return;
    }

    if (fileCount) {
      console.log(`[TOOL] ${toolName} -> ${fileCount} items`);
      return;
    }

    console.log(`[TOOL] ${toolName}`);
  }

  logOutcome(success: boolean, durationSeconds: number, error?: string): void {
    const outcome = success ? "SUCCESS" : "FAILURE";
    this.minimal(`Outcome: ${outcome} (duration: ${durationSeconds.toFixed(1)}s)`);
    if (error && this.shouldLog(LogLevel.INFO)) {
      this.info(`[ERROR] ${error}`);
    }
  }

  logSummaryExcerpt(summaryPath: string, lines = 5): void {
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

function generateFallbackSummary(goal: string, success: boolean, result: string): string {
  const outcome = success ? "Success" : "Failure";
  return `# Task Summary\n\nGoal: ${goal}\n\nOutcome: ${outcome}\n\n## Actions\n<details><summary>Click to see full execution log</summary>\n\n${result}\n</details>\n`;
}

function intEnv(name: string, fallback: number): number {
  const value = process.env[name];
  if (!value) {
    return fallback;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isNaN(parsed) ? fallback : parsed;
}

function runCommand(
  command: string,
  args: string[],
  options?: { cwd?: string; env?: NodeJS.ProcessEnv; allowFailure?: boolean; maxBuffer?: number }
): { status: number | null; stdout: string; stderr: string } {
  const result = spawnSync(command, args, {
    cwd: options?.cwd,
    env: options?.env,
    encoding: "utf8",
    maxBuffer: options?.maxBuffer ?? 50 * 1024 * 1024, // 50MB default
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

function fixPermissions(directory: string, logger: ProgressLogger): void {
  const uidStr = process.env.HOST_UID;
  const gidStr = process.env.HOST_GID;
  if (!uidStr || !gidStr) {
    return;
  }
  const uid = Number.parseInt(uidStr, 10);
  const gid = Number.parseInt(gidStr, 10);
  if (Number.isNaN(uid) || Number.isNaN(gid)) {
    return;
  }

  const visit = (current: string): void => {
    try {
      fs.chownSync(current, uid, gid);
    } catch (error) {
      logger.info(`Warning: Failed to fix permissions: ${String(error)}`);
      return;
    }

    let entries: fs.Dirent[] = [];
    try {
      entries = fs.readdirSync(current, { withFileTypes: true });
    } catch (error) {
      logger.info(`Warning: Failed to read directory: ${String(error)}`);
      return;
    }

    for (const entry of entries) {
      const entryPath = path.join(current, entry.name);
      if (entry.isDirectory()) {
        visit(entryPath);
      } else {
        try {
          fs.chownSync(entryPath, uid, gid);
        } catch (error) {
          logger.info(`Warning: Failed to fix permissions: ${String(error)}`);
        }
      }
    }
  };

  logger.debug(`Fixing permissions for ${directory} to ${uid}:${gid}`);
  visit(directory);
}

async function syncClaudeSettings(logger: ProgressLogger, authToken: string | undefined, baseUrl: string): Promise<void> {
  const settingsPath = path.join(os.homedir(), ".claude", "settings.json");
  if (!fs.existsSync(settingsPath)) {
    return;
  }

  // Check if this is a mounted config (read-only mount from host)
  // If HOLON_MOUNTED_CLAUDE_CONFIG is set, the config is mounted and should not be modified
  const isMountedConfig = process.env.HOLON_MOUNTED_CLAUDE_CONFIG === "1";
  if (isMountedConfig) {
    logger.debug("Using mounted Claude config from host (skipping settings sync)");
    return;
  }

  try {
    const raw = fs.readFileSync(settingsPath, "utf8");
    const settings = JSON.parse(raw) as Record<string, unknown>;
    const envSection: Record<string, string> =
      typeof settings.env === "object" && settings.env !== null
        ? (settings.env as Record<string, string>)
        : {};

    if (authToken) {
      envSection.ANTHROPIC_AUTH_TOKEN = authToken;
      envSection.ANTHROPIC_API_KEY = authToken;
    }
    if (baseUrl) {
      envSection.ANTHROPIC_BASE_URL = baseUrl;
      envSection.ANTHROPIC_API_URL = baseUrl;
      envSection.CLAUDE_CODE_API_URL = baseUrl;
    }
    envSection.IS_SANDBOX = "1";

    settings.env = envSection;
    fs.writeFileSync(settingsPath, JSON.stringify(settings, null, 2));
    logger.debug("Synced environment to Claude settings");
  } catch (error) {
    logger.debug(`Failed to sync Claude settings: ${String(error)}`);
  }
}

async function connectivityCheck(logger: ProgressLogger, baseUrl: string): Promise<void> {
  logger.minimal(`Checking environment: ANTHROPIC_AUTH_TOKEN present: ${Boolean(process.env.ANTHROPIC_AUTH_TOKEN || process.env.ANTHROPIC_API_KEY)}`);
  logger.minimal(`Testing connectivity to ${baseUrl}...`);
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 10_000);
  try {
    const response = await fetch(baseUrl, { signal: controller.signal });
    if (response.ok) {
      logger.minimal(`Connectivity test: HTTP ${response.status} (OK)`);
    } else {
      logger.minimal(`Warning: Connectivity test: HTTP ${response.status}`);
    }
  } catch (error) {
    logger.minimal(`Warning: Connectivity test failed/timed out: ${String(error)}`);
  } finally {
    clearTimeout(timeout);
  }
}

async function runClaude(
  logger: ProgressLogger,
  workspacePath: string,
  systemInstruction: string,
  userPrompt: string,
  logFile: fs.WriteStream
): Promise<{ success: boolean; result: string }> {
  const env = { ...process.env } as NodeJS.ProcessEnv;
  const isMountedConfig = env.HOLON_MOUNTED_CLAUDE_CONFIG === "1";

  // Extract auth token and base URL from environment variables
  // These will be used as fallback when mounted config doesn't have them
  const authToken = env.ANTHROPIC_AUTH_TOKEN || env.ANTHROPIC_API_KEY;
  const baseUrl = env.ANTHROPIC_BASE_URL || env.ANTHROPIC_API_URL || "https://api.anthropic.com";

  // Normalize env-based auth variables so the Claude Code SDK can rely on them,
  // regardless of whether a mounted config is present. Do not overwrite any
  // values that are already set (from mounted config or the user).
  if (authToken) {
    if (!env.ANTHROPIC_AUTH_TOKEN) {
      env.ANTHROPIC_AUTH_TOKEN = authToken;
    }
    if (!env.ANTHROPIC_API_KEY) {
      env.ANTHROPIC_API_KEY = authToken;
    }
  }
  if (baseUrl) {
    if (!env.ANTHROPIC_BASE_URL) {
      env.ANTHROPIC_BASE_URL = baseUrl;
    }
    if (!env.ANTHROPIC_API_URL) {
      env.ANTHROPIC_API_URL = baseUrl;
    }
    if (!env.CLAUDE_CODE_API_URL) {
      env.CLAUDE_CODE_API_URL = baseUrl;
    }
  }

  if (isMountedConfig) {
    logger.info("Using mounted Claude config from host (env-based auth as fallback)");
  }
  env.IS_SANDBOX = "1";

  const model = env.HOLON_MODEL;
  const fallbackModel = env.HOLON_FALLBACK_MODEL;
  const abortController = new AbortController();
  const options: Options = {
    cwd: workspacePath,
    env,
    abortController,
    permissionMode: "bypassPermissions",
    allowDangerouslySkipPermissions: true,
    systemPrompt: { type: "preset", preset: "claude_code", append: systemInstruction },
    settingSources: ["user", "project"],
    tools: { type: "preset", preset: "claude_code" },
    stderr: (data: string) => {
      logFile.write(`[stderr] ${data}`);
      logger.debug(data.trim());
    },
  };

  if (model) {
    options.model = model;
  }
  if (fallbackModel) {
    options.fallbackModel = fallbackModel;
  }

  let success = true;
  let finalOutput = "";
  let resultReceived = false;
  let resultText = "";
  let timeoutError: Error | null = null;
  let queryError: Error | null = null;

  const heartbeatSeconds = intEnv("HOLON_HEARTBEAT_SECONDS", 60);
  const idleTimeoutSeconds = intEnv("HOLON_RESPONSE_IDLE_TIMEOUT_SECONDS", 1800);
  const totalTimeoutSeconds = intEnv("HOLON_RESPONSE_TOTAL_TIMEOUT_SECONDS", 7200);
  const queryTimeoutSeconds = intEnv("HOLON_QUERY_TIMEOUT_SECONDS", 300);

  const startTime = Date.now();
  let lastMsgTime = startTime;
  let msgCount = 0;

  if (heartbeatSeconds > 0) {
    logger.minimal(
      `Response stream heartbeat enabled: interval=${heartbeatSeconds}s idle_timeout=${idleTimeoutSeconds}s total_timeout=${totalTimeoutSeconds}s`
    );
  }

  const heartbeatTimer = heartbeatSeconds > 0
    ? setInterval(() => {
      const now = Date.now();
      const idleFor = (now - lastMsgTime) / 1000;
      const totalFor = (now - startTime) / 1000;

      if (idleFor >= heartbeatSeconds) {
        logger.minimal(`No response yet (idle ${Math.floor(idleFor)}s, total ${Math.floor(totalFor)}s)...`);
      }

      if (queryTimeoutSeconds > 0 && msgCount === 0 && totalFor >= queryTimeoutSeconds) {
        timeoutError = new Error(`No response for ${Math.floor(totalFor)}s (query timeout ${queryTimeoutSeconds}s)`);
      } else if (idleTimeoutSeconds > 0 && idleFor >= idleTimeoutSeconds) {
        timeoutError = new Error(`No response for ${Math.floor(idleFor)}s (idle timeout ${idleTimeoutSeconds}s)`);
      } else if (totalTimeoutSeconds > 0 && totalFor >= totalTimeoutSeconds) {
        timeoutError = new Error(`Response stream exceeded ${totalTimeoutSeconds}s total timeout`);
      }

      if (timeoutError && !abortController.signal.aborted) {
        abortController.abort();
      }
    }, heartbeatSeconds * 1000)
    : null;

  const queryStream = query({ prompt: userPrompt, options });

  try {
    for await (const message of queryStream) {
      lastMsgTime = Date.now();
      msgCount += 1;
      logFile.write(`${JSON.stringify(message)}\n`);

      if (message?.type === "assistant" && message.message && Array.isArray(message.message.content)) {
        for (const block of message.message.content) {
          if (block.type === "text" && typeof block.text === "string") {
            finalOutput += block.text;
          } else if (block.type === "tool_use") {
            const toolName = typeof block.name === "string" ? block.name : "UnknownTool";
            logger.logToolUse(toolName);
          }
        }
      } else if (message?.type === "result") {
        const safeSubtype =
          typeof (message as any).subtype === "string" ? message.subtype : "unknown";
        const isError =
          typeof (message as any).is_error === "boolean"
            ? message.is_error
            : Boolean((message as any).is_error);
        logger.info(`Task result received: ${safeSubtype}, is_error: ${isError}`);
        if (isError) {
          success = false;
        }
        if ("result" in message && typeof message.result === "string") {
          resultText = message.result;
        } else if ("errors" in message && Array.isArray(message.errors)) {
          resultText = message.errors.join("\n");
        }
        resultReceived = true;
      }
    }
  } catch (error) {
    queryError = error instanceof Error ? error : new Error(String(error));
  } finally {
    if (heartbeatTimer) {
      clearInterval(heartbeatTimer);
    }
  }

  if (timeoutError) {
    if (queryError) {
      logger.debug(`SDK query error before timeout: ${String(queryError)}`);
    }
    throw timeoutError;
  }

  if (queryError) {
    throw queryError;
  }

  if (!resultReceived) {
    throw new Error("Claude Agent SDK finished without a result message");
  }

  const finalResult = resultText || finalOutput;

  return { success, result: finalResult };
}

async function runAgent(): Promise<void> {
  const logger = new ProgressLogger(process.env.LOG_LEVEL ?? "progress");
  const mode = process.env.HOLON_MODE ?? "solve";
  const isProbe = process.argv.slice(2).includes("--probe");

  console.log("Holon Claude Agent process started...");
  logger.minimal("Holon Claude Agent Starting...");

  const outputDir = "/holon/output";
  const evidenceDir = path.join(outputDir, "evidence");
  fs.mkdirSync(evidenceDir, { recursive: true });

  const specPath = "/holon/input/spec.yaml";
  if (!fs.existsSync(specPath)) {
    logger.minimal(`Error: Spec not found at ${specPath}`);
    process.exit(1);
  }

  if (isProbe) {
    logger.logPhase("Probe: Validating inputs");
    const workspacePath = "/holon/workspace";
    if (!fs.existsSync(workspacePath)) {
      logger.minimal(`Error: Workspace not found at ${workspacePath}`);
      process.exit(1);
    }

    try {
      fs.accessSync(outputDir, fs.constants.W_OK);
      const probePath = path.join(outputDir, ".probe");
      fs.writeFileSync(probePath, "ok\n");
      fs.unlinkSync(probePath);
    } catch (error) {
      logger.minimal(`Error: Output directory not writable: ${String(error)}`);
      process.exit(1);
    }

    const manifest = {
      status: "completed",
      outcome: "success",
      mode: "probe",
      artifacts: [{ name: "manifest.json", path: "manifest.json" }],
    };
    fs.writeFileSync(path.join(outputDir, "manifest.json"), JSON.stringify(manifest, null, 2));
    fixPermissions(outputDir, logger);
    logger.minimal("Probe completed.");
    return;
  }

  logger.logPhase("Loading specification");

  const spec = parseYaml(fs.readFileSync(specPath, "utf8")) as Record<string, any>;
  const goalVal = spec.goal ?? "";
  const goal = typeof goalVal === "object" && goalVal !== null ? String(goalVal.description ?? "") : String(goalVal);
  logger.info(`Task Goal: ${goal}`);

  const systemPromptPath = "/holon/input/prompts/system.md";
  if (!fs.existsSync(systemPromptPath)) {
    logger.minimal(`Error: Compiled system prompt not found at ${systemPromptPath}`);
    process.exit(1);
  }
  const systemInstruction = fs.readFileSync(systemPromptPath, "utf8");
  logger.info(`Loading compiled system prompt from ${systemPromptPath}`);

  const userPromptPath = "/holon/input/prompts/user.md";
  if (!fs.existsSync(userPromptPath)) {
    logger.minimal(`Error: Compiled user prompt not found at ${userPromptPath}`);
    process.exit(1);
  }
  const userPrompt = fs.readFileSync(userPromptPath, "utf8");
  logger.info(`Loading compiled user prompt from ${userPromptPath}`);

  logger.logPhase("Setting up git workspace");
  const workspacePath = "/holon/workspace";
  process.chdir(workspacePath);
  process.env.IS_SANDBOX = "1";

  logger.debug("Configuring git");
  runCommand("git", ["config", "--global", "--add", "safe.directory", workspacePath], { allowFailure: true });

  const gitName = process.env.GIT_AUTHOR_NAME || "holonbot[bot]";
  const gitEmail = process.env.GIT_AUTHOR_EMAIL || "250454749+holonbot[bot]@users.noreply.github.com";

  runCommand("git", ["config", "--global", "user.name", gitName], { allowFailure: true });
  runCommand("git", ["config", "--global", "user.email", gitEmail], { allowFailure: true });

  // Docker bind-mounts (especially on macOS) can surface filemode/permission bits differently
  // than the underlying repo expects, which makes `git add -A` stage the entire tree.
  // Disable filemode tracking so diffs reflect content changes only.
  runCommand("git", ["config", "core.filemode", "false"], { cwd: workspacePath, allowFailure: true });

  const hasGit = fs.existsSync(path.join(workspacePath, ".git"));
  if (!hasGit) {
    logger.info("No git repo found in workspace. Initializing temporary baseline...");
    runCommand("git", ["init"], { cwd: workspacePath });
    fs.appendFileSync(path.join(workspacePath, ".gitignore"), "\n__pycache__/\n*.pyc\n*.pyo\n.DS_Store\n");
    runCommand("git", ["add", "-A"], { cwd: workspacePath });
    runCommand("git", ["commit", "-m", "holon-baseline"], { cwd: workspacePath });
    logger.logToolUse("GitInit");
  } else {
    logger.info("Existing git repo found. Baseline established.");
  }

  logger.logPhase("Configuring Claude environment");
  const authToken = process.env.ANTHROPIC_AUTH_TOKEN || process.env.ANTHROPIC_API_KEY;
  const baseUrl = process.env.ANTHROPIC_BASE_URL || process.env.ANTHROPIC_API_URL || "https://api.anthropic.com";
  await syncClaudeSettings(logger, authToken, baseUrl);
  await connectivityCheck(logger, baseUrl);

  const logFilePath = path.join(evidenceDir, "execution.log");
  const logFile = fs.createWriteStream(logFilePath, { flags: "w" });

  const startTime = Date.now();
  let success: boolean;
  let result = "";

  try {
    logger.logPhase("Running AI execution");
    logger.minimal("Connecting to Claude Code...");
    logger.minimal("Session established. Running query...");
    logger.minimal("Executing query...");

    const response = await runClaude(logger, workspacePath, systemInstruction, userPrompt, logFile);
    success = response.success;
    result = response.result;

    logger.progress(`Claude Code execution finished. Success: ${success}`);

    logger.logPhase("Generating artifacts");
    const durationSeconds = (Date.now() - startTime) / 1000;

    logger.progress("Staging changes for diff");

    // Debug: Check workspace files before git operations
    const lsResult = runCommand("ls", ["-la", workspacePath], { cwd: workspacePath, allowFailure: true });
    logger.debug(`Workspace listing (first 20 lines):\n${lsResult.stdout.split('\n').slice(0, 20).join('\n')}`);

    // Debug: Check if .git directory exists and its type
    const gitCheckResult = runCommand("test", ["-d", ".git"], { cwd: workspacePath, allowFailure: true });
    const isGitDir = gitCheckResult.status === 0;
    logger.debug(`Is .git a directory: ${isGitDir}`);

    if (isGitDir) {
      const gitFileResult = runCommand("cat", [".git"], { cwd: workspacePath, allowFailure: true });
      if (gitFileResult.status === 0) {
        logger.debug(`.git is a file with content: ${gitFileResult.stdout.trim()}`);
      }
    }

    // Debug: List files in pkg/context/ before git add
    const contextLsResult = runCommand("ls", ["-la", "pkg/context/"], { cwd: workspacePath, allowFailure: true });
    logger.debug(`pkg/context/ listing:\n${contextLsResult.stdout}`);

    runCommand("git", ["add", "-A"], { cwd: workspacePath, allowFailure: true });

    // Remove compiled holon binary from git index.
    // The 'bin/' directory is in .gitignore, but 'go build ./cmd/holon' creates
    // a 'holon' binary in the root directory which is NOT ignored.
    // Compiled binaries should not be included in the PR's code changes.
    runCommand("git", ["reset", "holon"], { cwd: workspacePath, allowFailure: true });
    runCommand("git", ["reset", "bin/holon"], { cwd: workspacePath, allowFailure: true });

    // Debug: check git status before generating diff
    const statusResult = runCommand("git", ["status", "--short"], { cwd: workspacePath, allowFailure: true });
    logger.debug(`Git status after staging:\n${statusResult.stdout || "(empty)"}`);

    // Debug: check what files are staged
    const stagedFilesResult = runCommand("git", ["diff", "--cached", "--name-only"], { cwd: workspacePath, allowFailure: true });
    const stagedFiles = stagedFilesResult.stdout.trim().split("\n").filter((f) => f);
    logger.debug(`Staged files (${stagedFiles.length}):\n${stagedFiles.map((f) => `  ${f}`).join("\n") || "  (none)"}`);

    logger.progress("Generating patch file");
    const diffResult = runCommand(
      "git",
      ["--no-pager", "diff", "--cached", "--patch", "--binary", "--full-index", "--no-color", "--no-ext-diff"],
      { cwd: workspacePath, allowFailure: true }
    );

    const patchContent = diffResult.stdout;

    // Fail fast if patch is unexpectedly empty while we have staged files.
    // Continuing would produce a broken/empty diff.patch that later fails `git apply`.
    if (stagedFiles.length > 0 && patchContent.trim().length === 0) {
      const diagnosticsPath = path.join(evidenceDir, "git-diagnostics.txt");

      const diagnostics: string[] = [];
      diagnostics.push("Holon git diagnostics");
      diagnostics.push(`Workspace: ${workspacePath}`);
      diagnostics.push(`StagedFilesCount: ${stagedFiles.length}`);
      diagnostics.push(`DiffExitCode: ${diffResult.status}`);
      if (diffResult.stderr?.trim()) {
        diagnostics.push("\n--- git diff stderr ---");
        diagnostics.push(diffResult.stderr.trim());
      }

      diagnostics.push("\n--- env ---");
      diagnostics.push(`GIT_EXTERNAL_DIFF=${process.env.GIT_EXTERNAL_DIFF || ""}`);
      diagnostics.push(`GIT_PAGER=${process.env.GIT_PAGER || ""}`);
      diagnostics.push(`PAGER=${process.env.PAGER || ""}`);

      const revParseInside = runCommand("git", ["rev-parse", "--is-inside-work-tree"], { cwd: workspacePath, allowFailure: true });
      diagnostics.push("\n--- git rev-parse --is-inside-work-tree ---");
      diagnostics.push(revParseInside.stdout.trim() || revParseInside.stderr.trim() || "(no output)");

      const revParseGitDir = runCommand("git", ["rev-parse", "--git-dir"], { cwd: workspacePath, allowFailure: true });
      diagnostics.push("\n--- git rev-parse --git-dir ---");
      diagnostics.push(revParseGitDir.stdout.trim() || revParseGitDir.stderr.trim() || "(no output)");

      const headResult = runCommand("git", ["rev-parse", "HEAD"], { cwd: workspacePath, allowFailure: true });
      diagnostics.push("\n--- git rev-parse HEAD ---");
      diagnostics.push(headResult.stdout.trim() || headResult.stderr.trim() || "(no output)");

      const branchResult = runCommand("git", ["branch", "--show-current"], { cwd: workspacePath, allowFailure: true });
      diagnostics.push("\n--- git branch --show-current ---");
      diagnostics.push(branchResult.stdout.trim() || branchResult.stderr.trim() || "(no output)");

      const statusV1 = runCommand("git", ["status", "--porcelain=v1", "-uall"], { cwd: workspacePath, allowFailure: true });
      diagnostics.push("\n--- git status --porcelain=v1 -uall ---");
      diagnostics.push(statusV1.stdout.trim() || statusV1.stderr.trim() || "(no output)");

      const cachedNameStatus = runCommand("git", ["diff", "--cached", "--name-status"], { cwd: workspacePath, allowFailure: true });
      diagnostics.push("\n--- git diff --cached --name-status ---");
      diagnostics.push(cachedNameStatus.stdout.trim() || cachedNameStatus.stderr.trim() || "(no output)");

      const coreFilemode = runCommand("git", ["config", "--get", "core.filemode"], { cwd: workspacePath, allowFailure: true });
      diagnostics.push("\n--- git config core.filemode ---");
      diagnostics.push(coreFilemode.stdout.trim() || coreFilemode.stderr.trim() || "(unset)");

      const diffExternal = runCommand("git", ["config", "--get", "diff.external"], { cwd: workspacePath, allowFailure: true });
      diagnostics.push("\n--- git config diff.external ---");
      diagnostics.push(diffExternal.stdout.trim() || diffExternal.stderr.trim() || "(unset)");

      const pagerDiff = runCommand("git", ["config", "--get", "pager.diff"], { cwd: workspacePath, allowFailure: true });
      diagnostics.push("\n--- git config pager.diff ---");
      diagnostics.push(pagerDiff.stdout.trim() || pagerDiff.stderr.trim() || "(unset)");

      const colorUI = runCommand("git", ["config", "--get", "color.ui"], { cwd: workspacePath, allowFailure: true });
      diagnostics.push("\n--- git config color.ui ---");
      diagnostics.push(colorUI.stdout.trim() || colorUI.stderr.trim() || "(unset)");

      fs.writeFileSync(diagnosticsPath, diagnostics.join("\n") + "\n");

      throw new Error(
        `Patch generation failed: ${stagedFiles.length} files are staged but diff is empty. See evidence: ${diagnosticsPath}`
      );
    }

    logger.progress(`Generated patch: ${patchContent.length} characters`);

    // Read bundle manifest to derive agent metadata
    const bundleManifest = readBundleManifest();
    const agentMetadata = getAgentMetadata(bundleManifest);

    const manifest = {
      metadata: {
        agent: agentMetadata.agent,
        version: agentMetadata.version,
        mode: mode,
        ...(agentMetadata.engine && { engine: agentMetadata.engine }),
      },
      status: "completed",
      outcome: success ? "success" : "failure",
      duration: `${durationSeconds.toFixed(1)}s`,
      artifacts: [
        { name: "diff.patch", path: "diff.patch" },
        { name: "summary.md", path: "summary.md" },
        { name: "evidence", path: "evidence/" },
      ],
    };

    fs.writeFileSync(path.join(outputDir, "manifest.json"), JSON.stringify(manifest, null, 2));
    fs.writeFileSync(path.join(outputDir, "diff.patch"), patchContent);

    const summaryOut = path.join(outputDir, "summary.md");
    let summaryText = "";
    if (fs.existsSync(summaryOut)) {
      logger.info("Found user-generated summary.md in /holon/output.");
      summaryText = fs.readFileSync(summaryOut, "utf8");
    } else {
      logger.info("No summary.md found. Falling back to execution log.");
      summaryText = generateFallbackSummary(goal, success, result);
    }

    fs.writeFileSync(summaryOut, summaryText);
    logger.progress(`Artifacts written to ${outputDir}`);
    fixPermissions(outputDir, logger);

    logger.logSummaryExcerpt(summaryOut);
    logger.logOutcome(success, durationSeconds);
  } catch (error) {
    logger.progress(`Execution failed: ${String(error)}`);
    logger.debug(`Exception details: ${String(error)}`);

    const durationSeconds = (Date.now() - startTime) / 1000;
    logger.logOutcome(false, durationSeconds, String(error));

    // Read bundle manifest to derive agent metadata
    const bundleManifest = readBundleManifest();
    const agentMetadata = getAgentMetadata(bundleManifest);

    const manifest = {
      metadata: {
        agent: agentMetadata.agent,
        version: agentMetadata.version,
        mode: mode,
        ...(agentMetadata.engine && { engine: agentMetadata.engine }),
      },
      status: "completed",
      outcome: "failure",
      duration: `${durationSeconds.toFixed(1)}s`,
      error: String(error),
    };
    fs.writeFileSync(path.join(outputDir, "manifest.json"), JSON.stringify(manifest, null, 2));
    fixPermissions(outputDir, logger);
    process.exitCode = 1;
    return;
  } finally {
    logFile.end();
  }
}

// Only run agent when executed as main module, not when imported as a dependency
// This check prevents the agent from auto-running when tests import the module
// The spec file path only exists in actual Holon execution environment
const SPEC_PATH = "/holon/input/spec.yaml";
const shouldRunAutomatically = fs.existsSync(SPEC_PATH);

if (shouldRunAutomatically) {
  runAgent().catch((error) => {
    console.error(error);
    process.exit(1);
  });
}
