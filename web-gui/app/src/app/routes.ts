import type { RouteKey } from "../runtime/types";

interface BrowserRoute {
  route: RouteKey;
  agentId?: string;
}

export function routeFromLocation(location: Pick<Location, "pathname">): BrowserRoute {
  const path = location.pathname.replace(/\/+$/, "") || "/";

  if (path === "/") return { route: "dashboard" };
  if (path === "/search") return { route: "search" };
  if (path === "/settings") return { route: "settings" };

  const agentMatch = path.match(/^\/agents\/([^/]+)(?:\/conversation)?$/);
  if (agentMatch) {
    return { route: "agent", agentId: safeDecodeURIComponent(agentMatch[1]) };
  }

  return { route: "dashboard" };
}

export function pathForRoute(route: RouteKey, agentId?: string, query?: Record<string, string | number | undefined>): string {
  const queryString = query ? new URLSearchParams(Object.entries(query).flatMap(([key, value]) => (value == null ? [] : [[key, String(value)]]))).toString() : "";
  if (route === "search") return "/search";
  if (route === "settings") return "/settings";
  if (route === "agent" && agentId) return `/agents/${encodeURIComponent(agentId)}/conversation${queryString ? `?${queryString}` : ""}`;
  return "/";
}

export function pushBrowserRoute(route: RouteKey, agentId?: string, query?: Record<string, string | number | undefined>): void {
  const nextPath = pathForRoute(route, agentId, query);
  if (`${window.location.pathname}${window.location.search}` === nextPath) return;
  window.history.pushState(null, "", nextPath);
}

function safeDecodeURIComponent(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}
