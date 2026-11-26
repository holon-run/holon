import i18next from "i18next";
import { create } from "zustand";

import { authorizationHeaders, resolveRuntimeApiBase } from "./client";
import {
  createNotificationSync,
  emptyNotifications,
  NotificationHttpError,
  type NotificationSnapshot,
  type RuntimeNotification,
} from "./notification-sync";
import { getRuntimeConnectionConfig, useRuntimeStore } from "./runtime-store";
import {
  claimSystemNotification,
  createSystemNotifications,
  readSystemNotificationPermission,
  requestSystemNotificationPermission,
  type SystemNotificationPermission,
} from "./system-notifications";

// Kept in memory only. A runtime/subject switch clears it, independently of
// conversation read markers and presentation caches.
export const useRuntimeNotifications = create<NotificationSnapshot>(() => emptyNotifications);

function compactPreviewText(raw: string | null | undefined, maxChars: number): string | undefined {
  if (!raw) return undefined;
  const cleaned = raw
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/[#>*_`~-]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  if (!cleaned) return undefined;
  if (cleaned.length <= maxChars) return cleaned;
  return `${cleaned.slice(0, maxChars - 1).trimEnd()}…`;
}

function showSystemNotification(item: RuntimeNotification) {
  try {
    const agentName =
      useRuntimeStore.getState().bootstrap.agents.find((a) => a.id === item.target.agent_id)?.name
      ?? item.target.agent_id;
    const context = item.title ?? item.summary ?? item.target.work_item_id;
    const notification = new Notification(i18next.t(`notifications.kinds.${item.kind}`), {
      body: context ? `${agentName} · ${context}` : agentName,
      tag: item.id,
    });
    // Only the page owns the deep link, so a click brings this tab forward.
    notification.onclick = () => window.focus();
  } catch {
    // Browsers reject construction when the permission changed under us.
  }
}

export const systemNotifications = createSystemNotifications({
  permission: readSystemNotificationPermission,
  requestPermission: requestSystemNotificationPermission,
  claim: claimSystemNotification,
  show: showSystemNotification,
});

// Mirrored into a store so the bell panel can render the permission state.
export const useSystemNotificationPermission = create<{
  permission: SystemNotificationPermission;
  refresh: () => void;
  request: () => Promise<void>;
}>(() => ({
  permission: readSystemNotificationPermission(),
  refresh: () => {
    useSystemNotificationPermission.setState({ permission: readSystemNotificationPermission() });
  },
  request: async () => {
    const permission = await systemNotifications.requestPermission();
    useSystemNotificationPermission.setState({ permission });
  },
}));

const IGNORED_PENDING_STORAGE_PREFIX = "holon.webGui.ignoredPendingNotifications.v1";

function ignoredPendingStorageKey(): string {
  const config = getRuntimeConnectionConfig();
  const scope = resolveRuntimeApiBase(config) ?? (typeof window !== "undefined" ? window.location.origin : "local");
  return `${IGNORED_PENDING_STORAGE_PREFIX}:${scope}`;
}

function readIgnoredPendingIds(): ReadonlySet<string> {
  if (typeof window === "undefined") return new Set();
  try {
    const raw = window.localStorage.getItem(ignoredPendingStorageKey());
    if (!raw) return new Set();
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed)
      ? new Set(parsed.filter((value): value is string => typeof value === "string" && value.length > 0))
      : new Set();
  } catch {
    return new Set();
  }
}

function writeIgnoredPendingIds(ids: ReadonlySet<string>): void {
  if (typeof window === "undefined") return;
  try {
    const key = ignoredPendingStorageKey();
    if (ids.size === 0) {
      window.localStorage.removeItem(key);
    } else {
      window.localStorage.setItem(key, JSON.stringify(Array.from(ids)));
    }
  } catch {
    // Ignore browser storage errors; in-memory state still updates for the session.
  }
}

interface WorkItemPreviewEntry {
  title?: string;
  summary?: string;
}

const workItemPreviewCache = new Map<string, WorkItemPreviewEntry>();
const briefPreviewCache = new Map<string, string>();
let previewCacheScope = "";

function ensurePreviewCacheScope(scope: string) {
  if (previewCacheScope !== scope) {
    previewCacheScope = scope;
    workItemPreviewCache.clear();
    briefPreviewCache.clear();
  }
}

interface RawWorkItemPreviewDto {
  objective?: string | null;
  blocked_by?: string | null;
  focus?: string | null;
  result_summary?: string | null;
  active_wait_conditions?: Array<{ waiting_for?: string | null }> | null;
}

interface RawBriefBatchDto {
  briefs?: Array<{ id?: string; text?: string | null }>;
}

/**
 * Fallback hydration for older daemons whose `/notifications/{pending,recent}`
 * responses omit inline `title` / `summary`. Cached per work item / brief ID so
 * each item is fetched at most once per session.
 */
async function enrichNotificationsIfNeeded(
  baseUrl: string,
  headers: Record<string, string>,
  signal: AbortSignal,
  pending: RuntimeNotification[],
  recent: RuntimeNotification[],
): Promise<{ pending: RuntimeNotification[]; recent: RuntimeNotification[] }> {
  ensurePreviewCacheScope(baseUrl);
  const candidateRecent = recent.slice(0, 20);
  const allCandidates = [...pending, ...candidateRecent];

  // 1. Hydrate missing WorkItem objectives / wait prompts.
  const missingWorkKeys = new Map<string, { agentId: string; workItemId: string }>();
  for (const item of allCandidates) {
    if (item.title || !item.target.work_item_id) continue;
    const cacheKey = `${item.target.agent_id}:${item.target.work_item_id}`;
    if (!workItemPreviewCache.has(cacheKey) && missingWorkKeys.size < 20) {
      missingWorkKeys.set(cacheKey, {
        agentId: item.target.agent_id,
        workItemId: item.target.work_item_id,
      });
    }
  }

  if (missingWorkKeys.size > 0) {
    await Promise.allSettled(
      Array.from(missingWorkKeys.entries()).map(async ([cacheKey, { agentId, workItemId }]) => {
        const resp = await fetch(
          `${baseUrl}/agents/${encodeURIComponent(agentId)}/work-items/${encodeURIComponent(workItemId)}`,
          { signal, credentials: "include", headers },
        );
        if (!resp.ok) {
          workItemPreviewCache.set(cacheKey, {});
          return;
        }
        const data = (await resp.json()) as RawWorkItemPreviewDto;
        const title = compactPreviewText(data.objective, 160);
        const waitReason =
          data.active_wait_conditions?.find((w) => w.waiting_for)?.waiting_for
          ?? data.blocked_by
          ?? data.focus
          ?? data.result_summary;
        const summary = compactPreviewText(waitReason, 240);
        workItemPreviewCache.set(cacheKey, {
          title,
          summary: summary && summary !== title ? summary : undefined,
        });
      }),
    );
  }

  // 2. Hydrate missing Brief text previews for recent notifications (`id` === `brief_id`).
  const missingBriefsByAgent = new Map<string, string[]>();
  for (const item of candidateRecent) {
    if (item.summary || !item.id.startsWith("brief_")) continue;
    const cacheKey = `${item.target.agent_id}:${item.id}`;
    if (briefPreviewCache.has(cacheKey)) continue;
    const list = missingBriefsByAgent.get(item.target.agent_id);
    if (list) list.push(item.id);
    else missingBriefsByAgent.set(item.target.agent_id, [item.id]);
  }

  if (missingBriefsByAgent.size > 0) {
    await Promise.allSettled(
      Array.from(missingBriefsByAgent.entries()).map(async ([agentId, briefIds]) => {
        const resp = await fetch(
          `${baseUrl}/agents/${encodeURIComponent(agentId)}/briefs:batchGet`,
          {
            method: "POST",
            signal,
            credentials: "include",
            headers: { ...headers, "content-type": "application/json" },
            body: JSON.stringify({ brief_ids: Array.from(new Set(briefIds)) }),
          },
        );
        if (!resp.ok) return;
        const data = (await resp.json()) as RawBriefBatchDto;
        for (const brief of data.briefs ?? []) {
          if (!brief.id) continue;
          briefPreviewCache.set(`${agentId}:${brief.id}`, compactPreviewText(brief.text, 240) ?? "");
        }
        for (const id of briefIds) {
          const key = `${agentId}:${id}`;
          if (!briefPreviewCache.has(key)) briefPreviewCache.set(key, "");
        }
      }),
    );
  }

  const applyEnrichment = (item: RuntimeNotification): RuntimeNotification => {
    const workEntry = item.target.work_item_id
      ? workItemPreviewCache.get(`${item.target.agent_id}:${item.target.work_item_id}`)
      : undefined;
    const briefText = briefPreviewCache.get(`${item.target.agent_id}:${item.id}`) || undefined;
    const title = item.title ?? workEntry?.title;
    const rawSummary = item.summary ?? briefText ?? workEntry?.summary;
    const summary = rawSummary && rawSummary !== title ? rawSummary : undefined;
    if (title === item.title && summary === item.summary) return item;
    return {
      ...item,
      ...(title ? { title } : {}),
      ...(summary ? { summary } : {}),
    };
  };

  return {
    pending: pending.map(applyEnrichment),
    recent: recent.map(applyEnrichment),
  };
}

export const runtimeNotificationSync = createNotificationSync({
  load(signal) {
    const config = getRuntimeConnectionConfig();
    const base = resolveRuntimeApiBase(config);
    const apiBase = base ?? "/api";
    const headers = { ...authorizationHeaders(config.token), accept: "application/json" };
    async function list(name: "pending" | "recent"): Promise<RuntimeNotification[]> {
      const response = await fetch(`${apiBase}/notifications/${name}`, {
        signal, credentials: "include", cache: "no-store", headers,
      });
      if (!response.ok) throw new NotificationHttpError(response.status);
      const { items } = await response.json() as { items: RuntimeNotification[] };
      if (!Array.isArray(items)) throw new Error("Invalid notification list");
      return items;
    }
    return Promise.all([list("pending"), list("recent")]).then(([pending, recent]) =>
      enrichNotificationsIfNeeded(apiBase, headers, signal, pending, recent),
    );
  },
  isHidden: () => document.visibilityState === "hidden",
  isViewing(target) {
    const state = useRuntimeStore.getState();
    return state.route === "agent" && state.selectedAgentId === target.agent_id;
  },
  publish: (snapshot) => useRuntimeNotifications.setState(snapshot, true),
  notify: (items) => { void systemNotifications.deliver(items); },
  readIgnoredPendingIds,
  writeIgnoredPendingIds,
});
