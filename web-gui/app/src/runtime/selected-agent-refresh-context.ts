import { create } from "zustand";

export interface SelectedAgentRefreshContext {
  key: string;
  agentId: string;
  incarnation: number;
  scopeKey: string;
  onAccessError: (error: unknown) => void;
}

/** Admission is owned by the selected route, not by mounted inspector cards. */
export const useSelectedAgentRefreshContext = create<{ current: SelectedAgentRefreshContext | null }>(() => ({
  current: null,
}));

export function registerSelectedAgentRefreshContext(context: SelectedAgentRefreshContext): () => void {
  useSelectedAgentRefreshContext.setState({ current: context });
  return () => {
    if (useSelectedAgentRefreshContext.getState().current === context) {
      useSelectedAgentRefreshContext.setState({ current: null });
    }
  };
}

export interface ResumeAdmissionStatus {
  pending: boolean;
  token: number;
}

/**
 * Global resume admission shared with inspector partitions. While a deferred
 * resume reconciliation is pending it will replace the client generation, so
 * partitions hold automatic reads until it settles.
 */
export const useResumeAdmission = create<{ status: ResumeAdmissionStatus }>(() => ({
  status: { pending: false, token: 0 },
}));

export function setResumeAdmissionStatus(status: ResumeAdmissionStatus): void {
  const previous = useResumeAdmission.getState().status;
  if (previous.pending === status.pending && previous.token === status.token) return;
  useResumeAdmission.setState({ status });
}
