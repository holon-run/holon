import type { components } from "./generated/openapi";

export type AgentCreationIdentity = Pick<components["schemas"]["AgentDetail"]["identity"], "agent_id" | "incarnation" | "status">;
export type AgentBootstrap = NonNullable<components["schemas"]["AgentDetail"]["bootstrap"]>;
export const BOOTSTRAP_STEPS = ["template", "runtime", "workspace", "model", "initial_message"] as const;

export interface AgentCreateResponse {
  identity: AgentCreationIdentity;
  receipt: {
    receipt_id: string;
    agent_id: string;
    stage: "provisioning" | "ready" | "degraded";
    job_id?: string;
    bootstrap?: AgentBootstrap;
  };
}

export type AgentCreationProblem =
  | "conflict" | "deletion_incomplete" | "invalid" | "unauthorized"
  | "forbidden" | "not_found" | "busy" | "server" | "unexpected";

export type AgentCreationResult =
  | { kind: "accepted"; creation: AgentCreateResponse }
  | { kind: "rejected"; problem: AgentCreationProblem; status: number }
  | { kind: "uncertain" }
  | { kind: "stale" };

export type AgentCreationLookup =
  | { kind: "found"; identity: AgentCreationIdentity }
  | { kind: "missing" | "unavailable" | "stale" };

export interface AgentCreationProgress {
  creation: AgentCreateResponse;
  status: "initializing" | "ready" | "degraded" | "uncertain";
  observationRevision: number;
  bootstrap?: AgentBootstrap;
}

export function validCreationIdentity(identity: AgentCreationIdentity | undefined, agentId: string): identity is AgentCreationIdentity {
  return identity?.agent_id === agentId && Number.isSafeInteger(identity.incarnation)
    && identity.incarnation > 0 && identity.status === "active";
}

export function validCreationResponse(value: AgentCreateResponse, agentId: string): boolean {
  return validCreationIdentity(value?.identity, agentId)
    && value.receipt?.agent_id === agentId
    && typeof value.receipt.receipt_id === "string" && value.receipt.receipt_id.length > 0
    && ["provisioning", "ready", "degraded"].includes(value.receipt.stage);
}

export function bootstrapProgress(bootstrap: AgentBootstrap | undefined): AgentCreationProgress["status"] {
  // Pending steps can coexist with the summary's degraded status during provisioning.
  if (!bootstrap) return "uncertain";
  if (BOOTSTRAP_STEPS.some((key) => bootstrap[key]?.status === "failed")) return "degraded";
  if (BOOTSTRAP_STEPS.every((key) => bootstrap[key]?.status === "succeeded")) return "ready";
  return "initializing";
}
