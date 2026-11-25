/**
 * Template selection state for the "new agent" dialog.
 *
 * An empty selection means the user has not chosen yet and cannot submit.
 * The explicit blank option starts a blank agent, which must omit the
 * `template` field entirely instead of sending `""` or `null`.
 */
export const BLANK_AGENT_TEMPLATE = "__blank__";

export type CreateAgentTemplateSelection =
  | { kind: "unselected" }
  | { kind: "blank" }
  | { kind: "template"; template: string };

export function parseCreateAgentTemplate(selection: string): CreateAgentTemplateSelection {
  if (selection === "") return { kind: "unselected" };
  if (selection === BLANK_AGENT_TEMPLATE) return { kind: "blank" };
  const template = selection.trim();
  return template ? { kind: "template", template } : { kind: "unselected" };
}

/** Resolves the dialog selection into the create request template, if any. */
export function resolveCreateAgentTemplate(selection: string): string | undefined {
  const parsed = parseCreateAgentTemplate(selection);
  return parsed.kind === "template" ? parsed.template : undefined;
}

/**
 * Whether the dialog can submit. Blank and template selections are both
 * valid; only a missing selection blocks submission. The blank option is
 * always available, so an empty template catalog still allows creating a
 * blank agent.
 */
export function canSubmitCreateAgent(input: {
  agentId: string;
  templateSelection: string;
  busy: boolean;
  uncertain: boolean;
}): boolean {
  return !input.busy
    && !input.uncertain
    && input.agentId.trim() !== ""
    && parseCreateAgentTemplate(input.templateSelection).kind !== "unselected";
}
