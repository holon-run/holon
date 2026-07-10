import { useTranslation } from "react-i18next";

import type { RuntimeTaskOutputResult, TaskSummary } from "../../runtime/types";
import { OutputField, SimpleField } from "./ToolExecutionRenderers";

function textField(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function scalarText(value: unknown): string {
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return "";
}

function sameText(left: string, right: string): boolean {
  return left.trim() !== "" && left === right;
}

function firstText(...values: unknown[]): string {
  for (const value of values) {
    const text = textField(value) || scalarText(value);
    if (text.trim() !== "") return text;
  }
  return "";
}

export interface NormalizedTaskDetailContent {
  summary: string;
  command: string;
  result: string;
  stdout: string;
  stderr: string;
  rawOutput: string;
  rawOutputTruncated: boolean;
  exitStatus?: unknown;
}

export function normalizeTaskDetailContent(task: TaskSummary, output?: RuntimeTaskOutputResult): NormalizedTaskDetailContent {
  const taskRecord = output?.task;
  const summary = firstText(taskRecord?.summary, output?.summary, task.summary);
  const command = textField(task.command);
  const stdout = textField(output?.stdout);
  const stderr = textField(output?.stderr);
  const hasSplitStreams = stdout !== "" || stderr !== "";
  const resultCandidate = firstText(taskRecord?.result_summary, output?.summary);
  const result = sameText(resultCandidate, summary) ? "" : resultCandidate;
  const rawOutputCandidate = hasSplitStreams ? "" : firstText(output?.output, taskRecord?.output_preview);
  const rawOutput =
    sameText(rawOutputCandidate, result) || sameText(rawOutputCandidate, summary)
      ? ""
      : rawOutputCandidate;

  return {
    summary,
    command,
    result,
    stdout,
    stderr,
    rawOutput,
    rawOutputTruncated: taskRecord?.output_truncated === true || output?.truncated === true,
    exitStatus: taskRecord?.exit_status,
  };
}

function CommandTaskRenderer({ task, output }: { task: TaskSummary; output?: RuntimeTaskOutputResult }) {
  const { t } = useTranslation();
  const detail = normalizeTaskDetailContent(task, output);

  return (
    <>
      {detail.command ? <OutputField label={t("inspector.command")} value={detail.command} /> : null}
      {detail.result ? <OutputField label={t("inspector.result")} value={detail.result} /> : null}
      {detail.stdout ? <OutputField label={t("inspector.stdout")} value={detail.stdout} /> : null}
      {detail.stderr ? <OutputField label={t("inspector.stderr")} value={detail.stderr} variant="error" /> : null}
      {detail.rawOutput ? (
        <OutputField
          label={detail.rawOutputTruncated ? t("inspector.outputTruncated") : t("inspector.rawOutput")}
          value={detail.rawOutput}
        />
      ) : null}
      {detail.exitStatus != null ? <SimpleField label={t("inspector.exit")} value={detail.exitStatus} /> : null}
    </>
  );
}

function DefaultTaskRenderer({ task, output }: { task: TaskSummary; output?: RuntimeTaskOutputResult }) {
  const { t } = useTranslation();
  const detail = normalizeTaskDetailContent(task, output);

  return (
    <>
      {detail.command ? <OutputField label={t("inspector.command")} value={detail.command} /> : null}
      {detail.result ? <OutputField label={t("inspector.result")} value={detail.result} /> : null}
      {detail.stdout ? <OutputField label={t("inspector.stdout")} value={detail.stdout} /> : null}
      {detail.stderr ? <OutputField label={t("inspector.stderr")} value={detail.stderr} variant="error" /> : null}
      {detail.rawOutput ? (
        <OutputField
          label={detail.rawOutputTruncated ? t("inspector.outputTruncated") : t("inspector.output")}
          value={detail.rawOutput}
        />
      ) : null}
      {detail.exitStatus != null ? <SimpleField label={t("inspector.exit")} value={detail.exitStatus} /> : null}
    </>
  );
}

export function TaskDetailContent({ task, output }: { task: TaskSummary; output?: RuntimeTaskOutputResult }) {
  if (task.kind === "command_task") {
    return <CommandTaskRenderer task={task} output={output} />;
  }

  return <DefaultTaskRenderer task={task} output={output} />;
}
