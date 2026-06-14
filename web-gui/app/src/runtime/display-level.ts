import type { DisplayLevel } from "./types";

export const displayLevels: DisplayLevel[] = ["info", "verbose", "debug"];

export function isVerboseLevel(level: DisplayLevel): boolean {
  return level === "verbose" || level === "debug";
}
