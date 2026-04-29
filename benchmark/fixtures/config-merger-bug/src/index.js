import { defaultConfig } from "./defaults.js";
import { mergeConfig } from "./merge.js";
import { normalizeOverrides } from "./normalize.js";

export function buildConfig(overrides) {
  return mergeConfig(defaultConfig, normalizeOverrides(overrides));
}
