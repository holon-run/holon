import { useCallback, useEffect, useRef, useState } from "react";
import { resolveRuntimeApiBase } from "../../runtime/client";
import { getRuntimeConnectionConfig } from "../../runtime/runtime-store";
import type { RuntimeConnection } from "../../runtime/types";

export type ServeStatus = {
  desired_enabled: boolean;
  available: boolean;
  connected: boolean;
  status_known: boolean;
  serving: boolean;
  conflict: boolean;
  control_authentication_available?: boolean;
  hostname?: string;
  serve_url?: string;
  message: string;
};

export function tailscaleServeUrl(connection: RuntimeConnection, path = ""): string {
  const base = resolveRuntimeApiBase(connection);
  if (!base) throw new Error("Runtime API base is unavailable");
  return `${base}/control/network/tailscale/serve${path}`;
}

export function useServeStatus(connection: RuntimeConnection) {
  const key = JSON.stringify([connection.mode, connection.source, connection.baseUrl]);
  const currentKey = useRef(key);
  currentKey.current = key;
  const sequence = useRef(0);
  const [snapshot, setSnapshot] = useState<{ key: string; status: ServeStatus }>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [revision, setRevision] = useState(0);
  const invalidate = useCallback(() => {
    sequence.current++;
    setRevision((value) => value + 1);
    setSnapshot(undefined);
  }, []);
  const request = useCallback(async (action?: "enable" | "disable") => {
    invalidate();
    const id = sequence.current;
    setBusy(true);
    setError("");
    try {
      const token = getRuntimeConnectionConfig().token;
      const response = await fetch(tailscaleServeUrl(connection, action ? `/${action}` : ""), {
        method: action ? "POST" : "GET", credentials: "include",
        headers: token ? { Authorization: `Bearer ${token}` } : {},
      });
      if (!response.ok) throw new Error("Serve status request failed");
      const status: ServeStatus = await response.json();
      if (id !== sequence.current || currentKey.current !== key) return undefined;
      setSnapshot({ key, status });
      return status;
    } catch (cause) {
      if (id === sequence.current && currentKey.current === key) setError(String(cause));
      return undefined;
    } finally {
      if (id === sequence.current && currentKey.current === key) setBusy(false);
    }
  }, [connection.mode, connection.source, connection.baseUrl, key, invalidate]);
  useEffect(() => {
    if (connection.source === "http") void request();
    return () => { sequence.current++; };
  }, [request, connection.source]);
  return { status: snapshot?.key === key ? snapshot.status : undefined, busy, error, revision,
    key, request, sequence };
}

export type ServeState = ReturnType<typeof useServeStatus>;
