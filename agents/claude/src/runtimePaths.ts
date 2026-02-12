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

function envOrDefault(value: string | undefined, fallback: string): string {
  const trimmed = value?.trim();
  return trimmed && trimmed.length > 0 ? trimmed : fallback;
}

export function resolveRuntimePaths(env: NodeJS.ProcessEnv = process.env): RuntimePaths {
  const workspaceDir = envOrDefault(env.HOLON_WORKSPACE_DIR, "/holon/workspace");
  const inputDir = envOrDefault(env.HOLON_INPUT_DIR, "/holon/input");
  const outputDir = envOrDefault(env.HOLON_OUTPUT_DIR, "/holon/output");
  const stateDir = envOrDefault(env.HOLON_STATE_DIR, "/holon/state");
  const agentHome = envOrDefault(env.HOLON_AGENT_HOME, "/root");

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
