import type { HTMLAttributes } from "react";

import { Badge } from "./Badge";

interface StatusChipProps extends HTMLAttributes<HTMLSpanElement> {
  tone?: string;
}

function toneToBadge(tone: string) {
  if (tone === "live" || tone === "streaming" || tone === "success") return "success";
  if (tone === "connecting" || tone === "syncing") return "accent";
  if (tone === "error") return "danger";
  if (tone === "preview") return "warning";
  if (tone === "muted") return "muted";
  return "neutral";
}

export function StatusChip({ tone = "idle", ...props }: StatusChipProps) {
  return <Badge tone={toneToBadge(tone)} {...props} />;
}
