import type { RouteKey } from "../runtime/types";

interface BrowserRoute {
  route: RouteKey;
  agentId?: string;
  skillId?: string;
  templateId?: string;
  eventSeq?: number;
}

function eventSeqFromSearch(search: string): number | undefined {
  const raw = new URLSearchParams(search).get("event_seq");
  if (!raw) return undefined;
  const eventSeq = Number(raw);
  return Number.isInteger(eventSeq) && eventSeq > 0 ? eventSeq : undefined;
}

export function routeFromLocation(location: Pick<Location, "pathname" | "search">): BrowserRoute {
  const path = location.pathname.replace(/\/+$/, "") || "/";

  if (path === "/") return { route: "dashboard" };
  if (path === "/search") return { route: "search" };
  if (path === "/skills") return { route: "skills" };
  const skillMatch = path.match(/^\/skills\/([^/]+)$/);
  if (skillMatch) {
    return {
      route: "skillDetail",
      skillId: safeDecodeURIComponent(skillMatch[1]),
    };
  }
  if (path === "/templates") return { route: "templates" };
  const templateMatch = path.match(/^\/templates\/([^/]+)$/);
  if (templateMatch) {
    return {
      route: "templateDetail",
      templateId: safeDecodeURIComponent(templateMatch[1]),
    };
  }
  if (path === "/settings") return { route: "settings" };

  const agentMatch = path.match(/^\/agents\/([^/]+)(?:\/conversation)?$/);
  if (agentMatch) {
    return {
      route: "agent",
      agentId: safeDecodeURIComponent(agentMatch[1]),
      eventSeq: eventSeqFromSearch(location.search),
    };
  }

  return { route: "dashboard" };
}

export function pathForRoute(route: RouteKey, agentId?: string, templateId?: string, query?: Record<string, string | number | undefined>): string {
  const queryString = query ? new URLSearchParams(Object.entries(query).flatMap(([key, value]) => (value == null ? [] : [[key, String(value)]]))).toString() : "";
  if (route === "search") return "/search";
  if (route === "skills") return "/skills";
  if (route === "skillDetail" && agentId) return `/skills/${encodeURIComponent(agentId)}`;
  if (route === "templates") return "/templates";
  if (route === "templateDetail" && templateId) return `/templates/${encodeURIComponent(templateId)}`;
  if (route === "settings") return "/settings";
  if (route === "agent" && agentId) return `/agents/${encodeURIComponent(agentId)}/conversation${queryString ? `?${queryString}` : ""}`;
  return "/";
}

export function pushBrowserRoute(route: RouteKey, agentId?: string, templateId?: string, query?: Record<string, string | number | undefined>): void {
  const nextPath = pathForRoute(route, agentId, templateId, query);
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
