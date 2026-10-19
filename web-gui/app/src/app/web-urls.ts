import { matchBrowserRoute } from "./routes";

export type WebReference = { kind: "web"; href: string } | { kind: "error"; message: string };

const invalidCharacters = /[\u0000-\u0020\u007f\\]/;
const origin = "https://holon.invalid";

/** Validate before URL normalization can erase traversal or change the origin. */
function validRoutePath(path: string): boolean {
  try {
    return !invalidCharacters.test(path) && path.split("/").every((segment) => {
      const decoded = decodeURIComponent(segment);
      return decoded !== "." && decoded !== ".."
        && !/[/\\\u0000-\u001f\u007f]/.test(decoded)
        // Do not let a second decoder reinterpret encoded separators or traversal.
        && !/%(?:2e|2f|5c|25)/i.test(decoded);
    });
  } catch {
    return false;
  }
}

export function isAppPath(path: string): boolean {
  return /^\/apps\/[^/]+\/[^/]+(?:\/.*)?$/.test(path) && validRoutePath(path);
}

/** Root-relative Web routes win over host paths, not over explicit file: URLs. */
export function classifyWebReference(raw: string): WebReference | undefined {
  const network = raw.startsWith("//");
  const absolute = /^(?:https?:|mailto:)/i.test(raw);
  const root = raw.startsWith("/") && !network;
  if (!network && !absolute && !root) return undefined;
  const path = raw.split(/[?#]/, 1)[0];
  const registered = root && (isAppPath(path) || matchBrowserRoute({ pathname: path, search: "" }) !== undefined);
  // Invalid App-shaped paths must not become native navigation after normalization.
  const appShaped = root && /^\/apps\/[^/]+\/[^/]+/.test(path);
  if (root && !registered && !appShaped) return undefined;
  try {
    if (invalidCharacters.test(raw)) throw new Error();
    const url = new URL(raw, origin);
    if (root) {
      if (!registered || !validRoutePath(path) || url.origin !== origin) throw new Error();
    } else if (url.protocol !== "http:" && url.protocol !== "https:" && !(absolute && url.protocol === "mailto:")) {
      throw new Error();
    }
    if (!root && !network && url.protocol !== "mailto:" && !/^https?:\/\//i.test(raw)) throw new Error();
    if (network && (!url.hostname || raw.startsWith("///"))) throw new Error();
    return { kind: "web", href: raw };
  } catch {
    return { kind: "error", message: "Invalid Web URL" };
  }
}
