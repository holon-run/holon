import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ConversationTurnGroup } from "../../runtime/conversation-view-model";

type TurnTiming = Pick<ConversationTurnGroup, "execution" | "startedAt" | "completedAt" | "durationMs">;

export function turnElapsedMs(turn: TurnTiming, now: number): number | null {
  if (turn.execution.kind === "terminal" && turn.durationMs != null) {
    return Number.isFinite(turn.durationMs) && turn.durationMs >= 0 ? turn.durationMs : null;
  }
  const start = Date.parse(turn.startedAt ?? "");
  const end = turn.execution.kind === "active" ? now : Date.parse(turn.completedAt ?? "");
  if (!Number.isFinite(start) || !Number.isFinite(end)) return null;
  return Math.max(0, end - start);
}

export function formatTurnDuration(milliseconds: number): string {
  const seconds = Math.floor(milliseconds / 1000);
  const minutes = Math.floor(seconds / 60);
  const remainder = String(seconds % 60).padStart(2, "0");
  return minutes < 60 ? `${minutes}:${remainder}`
    : `${Math.floor(minutes / 60)}:${String(minutes % 60).padStart(2, "0")}:${remainder}`;
}

/** Only the clock rerenders each second; activity and brief readers stay still. */
export function TurnElapsedTime({ turn }: { turn: TurnTiming }) {
  const { t } = useTranslation();
  const [now, setNow] = useState(Date.now);
  const active = turn.execution.kind === "active";
  const startedAt = turn.startedAt;
  useEffect(() => {
    if (!active || !Number.isFinite(Date.parse(startedAt ?? ""))) return;
    const tick = () => setNow(Date.now());
    tick();
    const interval = window.setInterval(tick, 1000);
    document.addEventListener("visibilitychange", tick);
    return () => {
      window.clearInterval(interval);
      document.removeEventListener("visibilitychange", tick);
    };
  }, [active, startedAt]);
  const elapsed = turnElapsedMs(turn, now);
  if (elapsed === null) return null;
  const duration = formatTurnDuration(elapsed);
  const label = t("agentPage.turnElapsed", { duration });
  return <>
    <span aria-hidden="true">·</span>
    <time className="conversation-turn-elapsed" dateTime={`PT${Math.floor(elapsed / 1000)}S`} aria-label={label}>
      {active ? duration : label}
    </time>
  </>;
}
