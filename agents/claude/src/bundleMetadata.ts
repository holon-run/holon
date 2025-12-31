import fs from "fs";

/**
 * Bundle manifest interface defining the structure of /holon/agent/manifest.json
 */
export interface BundleManifest {
  bundleVersion?: string;
  name?: string;
  version?: string;
  entry?: string;
  platform?: string;
  arch?: string;
  libc?: string;
  engine?: {
    name?: string;
    sdk?: string;
    sdkVersion?: string;
  };
  runtime?: {
    type?: string;
    version?: string;
  };
  env?: Record<string, string>;
  capabilities?: Record<string, boolean>;
}

/**
 * Agent metadata interface for the output manifest
 */
export interface AgentMetadata {
  agent: string;
  version: string;
  engine?: {
    sdk?: string;
    sdkVersion?: string;
  };
}

/**
 * Reads the bundle manifest from the specified path.
 * @param manifestPath - Path to the bundle manifest file (defaults to /holon/agent/manifest.json)
 * @returns Parsed bundle manifest or null if file is missing or invalid
 */
export function readBundleManifest(manifestPath: string = "/holon/agent/manifest.json"): BundleManifest | null {
  try {
    if (!fs.existsSync(manifestPath)) {
      return null;
    }
    const raw = fs.readFileSync(manifestPath, "utf8");
    return JSON.parse(raw) as BundleManifest;
  } catch (error) {
    // If manifest is missing or invalid, return null to use fallback defaults
    return null;
  }
}

/**
 * Derives agent metadata from the bundle manifest.
 * @param bundleManifest - The bundle manifest object (can be null)
 * @returns Agent metadata with agent name, version, and optional engine SDK info
 */
export function getAgentMetadata(bundleManifest: BundleManifest | null): AgentMetadata {
  // If bundle manifest is available, derive metadata from it
  if (bundleManifest) {
    const agent = bundleManifest.engine?.name || bundleManifest.name || "claude-code";
    const version = bundleManifest.version || "0.1.0";
    const metadata: AgentMetadata = { agent, version };

    // Optionally include engine SDK info for debugging
    // Only add engine object if at least one SDK field is defined
    if (bundleManifest.engine) {
      const engine: NonNullable<AgentMetadata["engine"]> = {};

      if (bundleManifest.engine.sdk !== undefined) {
        engine.sdk = bundleManifest.engine.sdk;
      }

      if (bundleManifest.engine.sdkVersion !== undefined) {
        engine.sdkVersion = bundleManifest.engine.sdkVersion;
      }

      if (Object.keys(engine).length > 0) {
        metadata.engine = engine;
      }
    }

    return metadata;
  }

  // Fallback to existing defaults for backward compatibility
  return {
    agent: "claude-code",
    version: "0.1.0",
  };
}
