import type { DisplayLevel } from "./types";

export const displayLevels: DisplayLevel[] = ["info", "verbose", "debug"];

export function availableDisplayLevels(developerDiagnosticsEnabled: boolean): readonly DisplayLevel[] {
  return developerDiagnosticsEnabled ? displayLevels : displayLevels.slice(0, 2);
}

export function isVerboseLevel(level: DisplayLevel): boolean {
  return level === "verbose" || level === "debug";
}
