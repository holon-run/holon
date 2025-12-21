import fs from "fs";
import path from "path";
import os from "os";
import { spawn, spawnSync } from "child_process";
import readline from "readline";
import { parse as parseYaml } from "yaml";

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
  options?: { cwd?: string; env?: NodeJS.ProcessEnv; allowFailure?: boolean }
): { status: number | null; stdout: string; stderr: string } {
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
  logger.minimal(`Checking environment: ANTHROPIC_API_KEY present: ${Boolean(process.env.ANTHROPIC_API_KEY || process.env.ANTHROPIC_AUTH_TOKEN)}`);
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
  const authToken = env.ANTHROPIC_AUTH_TOKEN || env.ANTHROPIC_API_KEY;
  const baseUrl = env.ANTHROPIC_BASE_URL || env.ANTHROPIC_API_URL || "https://api.anthropic.com";

  if (authToken) {
    env.ANTHROPIC_AUTH_TOKEN = authToken;
    env.ANTHROPIC_API_KEY = authToken;
  }
  if (baseUrl) {
    env.ANTHROPIC_BASE_URL = baseUrl;
    env.ANTHROPIC_API_URL = baseUrl;
    env.CLAUDE_CODE_API_URL = baseUrl;
  }
  env.IS_SANDBOX = "1";
  env.CLAUDE_CODE_ENTRYPOINT = "holon-adapter-ts";

  const args: string[] = [
    "--output-format",
    "stream-json",
    "--verbose",
    "--permission-mode",
    "bypassPermissions",
    "--append-system-prompt",
    systemInstruction,
  ];

  const model = env.HOLON_MODEL;
  const fallbackModel = env.HOLON_FALLBACK_MODEL;
  if (model) {
    args.push("--model", model);
  }
  if (fallbackModel) {
    args.push("--fallback-model", fallbackModel);
  }

  args.push("--print", "--", userPrompt);

  const proc = spawn("claude", args, {
    cwd: workspacePath,
    env,
    stdio: ["ignore", "pipe", "pipe"],
  });

  let success = true;
  let finalOutput = "";
  let resultReceived = false;
  let timeoutError: Error | null = null;
  let spawnError: Error | null = null;

  proc.on("error", (error) => {
    spawnError = error;
  });

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

        if (timeoutError) {
          proc.kill("SIGTERM");
          setTimeout(() => proc.kill("SIGKILL"), 5_000);
        }
      }, heartbeatSeconds * 1000)
    : null;

  proc.stderr?.on("data", (chunk: Buffer) => {
    const text = chunk.toString("utf8");
    logFile.write(`[stderr] ${text}`);
    logger.debug(text.trim());
  });

  const rl = readline.createInterface({ input: proc.stdout ?? process.stdin });
  for await (const line of rl) {
    if (!line) {
      continue;
    }
    lastMsgTime = Date.now();
    msgCount += 1;

    logFile.write(`${line}\n`);

    let parsed: any;
    try {
      parsed = JSON.parse(line);
    } catch (error) {
      logger.debug(`Non-JSON output: ${line}`);
      continue;
    }

    if (parsed.type === "assistant" && parsed.message && Array.isArray(parsed.message.content)) {
      for (const block of parsed.message.content) {
        if (block.type === "text" && typeof block.text === "string") {
          finalOutput += block.text;
        } else if (block.type === "tool_use") {
          const toolName = typeof block.name === "string" ? block.name : "UnknownTool";
          logger.logToolUse(toolName);
        }
      }
    } else if (parsed.type === "result") {
      logger.info(`Task result received: ${parsed.subtype}, is_error: ${parsed.is_error}`);
      if (parsed.is_error) {
        success = false;
      }
      resultReceived = true;
    }
  }

  const exitCode: number | null = await new Promise((resolve) => {
    proc.on("close", (code) => resolve(code));
  });

  if (heartbeatTimer) {
    clearInterval(heartbeatTimer);
  }

  if (timeoutError) {
    throw timeoutError;
  }

  if (spawnError) {
    throw spawnError;
  }

  if (exitCode !== 0 && !resultReceived) {
    throw new Error(`Claude Code exited with status ${exitCode}`);
  }

  logFile.write(`--- FINAL OUTPUT ---\n${finalOutput}\n`);

  return { success, result: finalOutput };
}

async function runAdapter(): Promise<void> {
  const logger = new ProgressLogger(process.env.LOG_LEVEL ?? "progress");

  console.log("Holon Claude Adapter process started...");
  logger.minimal("Holon Claude Adapter Starting...");

  const outputDir = "/holon/output";
  const evidenceDir = path.join(outputDir, "evidence");
  fs.mkdirSync(evidenceDir, { recursive: true });

  logger.logPhase("Loading specification");
  const specPath = "/holon/input/spec.yaml";
  if (!fs.existsSync(specPath)) {
    logger.minimal(`Error: Spec not found at ${specPath}`);
    process.exit(1);
  }

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
  runCommand("git", ["config", "--global", "user.name", "holon-adapter"], { allowFailure: true });
  runCommand("git", ["config", "--global", "user.email", "adapter@holon.local"], { allowFailure: true });

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
    runCommand("git", ["add", "-A"], { cwd: workspacePath, allowFailure: true });

    logger.progress("Generating patch file");
    const diffResult = runCommand(
      "git",
      ["diff", "--cached", "--patch", "--binary", "--full-index"],
      { cwd: workspacePath, allowFailure: true }
    );

    const patchContent = diffResult.stdout;
    logger.progress(`Generated patch: ${patchContent.length} characters`);

    const manifest = {
      metadata: {
        adapter: "claude-code-ts",
        version: "0.1.0",
      },
      status: "completed",
      outcome: success ? "success" : "failure",
      duration: durationSeconds,
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

    const manifest = {
      status: "completed",
      outcome: "failure",
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

runAdapter().catch((error) => {
  console.error(error);
  process.exit(1);
});
