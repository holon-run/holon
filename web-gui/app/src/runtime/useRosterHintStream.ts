import { useEffect } from "react";

import { authorizationHeaders, resolveRuntimeApiBase } from "./client";
import {
  getRuntimeConnectionConfig,
  refreshRosterFromStreamHint,
  useRuntimeStore,
} from "./runtime-store";
import { createRosterHintStream, type RosterHintStreamController } from "./roster-hint-stream";

/**
 * App-lifetime subscription to the global roster hint stream (#4310). The
 * controller singleton survives re-renders and repeated bootstraps; client
 * switches and unhealthy connections stop it through the effect below.
 */
const rosterHintStream: RosterHintStreamController = createRosterHintStream({
  openStream: (signal) => {
    const config = getRuntimeConnectionConfig();
    const baseUrl = resolveRuntimeApiBase(config) ?? "/api";
    return fetch(`${baseUrl}/events/stream`, {
      signal,
      credentials: "include",
      headers: { ...authorizationHeaders(config.token), accept: "text/event-stream" },
    });
  },
  onHint: refreshRosterFromStreamHint,
});

/**
 * Keep the left roster (status dots and unread badges) current without
 * polling: the stream nudges the throttled authoritative snapshot cycle
 * whenever any agent publishes a runtime event.
 */
export function useRosterHintStream(): void {
  const connectionError = useRuntimeStore((state) => state.bootstrap.connection.error);
  const connectionSource = useRuntimeStore((state) => state.bootstrap.connection.source);
  useEffect(() => {
    if (connectionError == null && connectionSource === "http") {
      rosterHintStream.start();
      return () => rosterHintStream.stop();
    }
    rosterHintStream.stop();
    return () => undefined;
  }, [connectionError, connectionSource]);
}
