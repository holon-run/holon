import path from "path";

export interface RuntimePaths {
  workspaceDir: string;
  inputDir: string;
  outputDir: string;
  stateDir: string;
  agentHome: string;
  specPath: string;
  systemPromptPath: string;
  userPromptPath: string;
  eventPayloadPath: string;
}

function requiredEnv(name: string, value: string | undefined): string {
  const trimmed = value?.trim();
  if (!trimmed) {
    throw new Error(`missing required environment variable: ${name}`);
  }
  return trimmed;
}

export function resolveRuntimePaths(env: NodeJS.ProcessEnv = process.env): RuntimePaths {
  const workspaceDir = requiredEnv("HOLON_WORKSPACE_DIR", env.HOLON_WORKSPACE_DIR);
  const inputDir = requiredEnv("HOLON_INPUT_DIR", env.HOLON_INPUT_DIR);
  const outputDir = requiredEnv("HOLON_OUTPUT_DIR", env.HOLON_OUTPUT_DIR);
  const stateDir = requiredEnv("HOLON_STATE_DIR", env.HOLON_STATE_DIR);
  const agentHome = requiredEnv("HOLON_AGENT_HOME", env.HOLON_AGENT_HOME);

  return {
    workspaceDir,
    inputDir,
    outputDir,
    stateDir,
    agentHome,
    specPath: path.join(inputDir, "spec.yaml"),
    systemPromptPath: path.join(inputDir, "prompts", "system.md"),
    userPromptPath: path.join(inputDir, "prompts", "user.md"),
    eventPayloadPath: path.join(inputDir, "context", "event.json"),
  };
}
