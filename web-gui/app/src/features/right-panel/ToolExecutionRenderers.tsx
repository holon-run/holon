import { useTranslation } from "react-i18next";
import { formatToolExecutionDetail } from "../inspector/ActivityInspectorPanel";
import type { RuntimeToolExecutionRecord } from "../../runtime/types";

// Utility helpers (mirrors the private helpers in ActivityInspectorPanel)

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value != null && !Array.isArray(value);
}

function textField(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function scalarText(value: unknown): string {
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return "";
}

function unwrapToolOutput(value: unknown): unknown {
  const envelope = isRecord(value) ? value.envelope : undefined;
  const result = isRecord(envelope) ? envelope.result : undefined;
  return result ?? value;
}

function nestedValue(value: unknown, keys: string[]): unknown {
  if (!isRecord(value)) return undefined;
  for (const key of keys) {
    const field = value[key];
    if (field != null && !(typeof field === "string" && field.trim() === "")) return field;
  }
  return undefined;
}

function nestedText(value: unknown, keys: string[]): string {
  const found = nestedValue(value, keys);
  return textField(found) || scalarText(found);
}

function commandText(input: unknown): string {
  const value = nestedValue(input, ["cmd", "command", "cmd_preview"]);
  return typeof value === "string" ? value : "";
}

function asResultRecord(output: unknown, key: string): Record<string, unknown> | undefined {
  if (!isRecord(output)) return undefined;
  return (isRecord(output[key]) ? output[key] : output) as Record<string, unknown>;
}

function arrayRecords(value: unknown): Record<string, unknown>[] {
  return Array.isArray(value) ? value.filter(isRecord) : [];
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function patchInputText(input: unknown): string {
  if (typeof input === "string") return input;
  if (!isRecord(input)) return "";
  return textField(input.patch) || textField(input.input) || textField(input.freeform);
}

// Shared rendering primitives

/** A labeled code/output block rendered as a standalone section. Exported for reuse by other detail panels. */
export function OutputField({
  label,
  value,
  variant,
}: {
  label: string;
  value: string;
  variant?: "error" | "diff";
}) {
  if (!value) return null;
  return (
    <section className={`tool-detail-field${variant ? ` tool-detail-field--${variant}` : ""}`}>
      <h3 className="tool-detail-field-label">{label}</h3>
      <pre className="tool-detail-field-content">{value}</pre>
    </section>
  );
}

/** A simple inline key-value row for scalar fields like exit status. */
export function SimpleField({ label, value }: { label: string; value: unknown }) {
  const text = textField(value) || scalarText(value);
  if (!text) return null;
  return (
    <div className="tool-detail-simple">
      <span className="tool-detail-simple-label">{label}</span>
      <span className="tool-detail-simple-value">{text}</span>
    </div>
  );
}

// ExecCommand renderer

function ExecCommandRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const output = unwrapToolOutput(record.output ?? record.result);
  const cmd = commandText(record.input) || textField(record.cmd_preview);
  const stdout = nestedText(output, ["stdout", "stdout_preview", "output", "output_preview", "combined_output_preview"]);
  const stderr = nestedText(output, ["stderr", "stderr_preview"]);
  const initialOutput = nestedText(output, ["initial_output_preview"]);
  const result = nestedText(output, ["summary", "summary_text", "result_summary", "result_summary_preview"]) || record.summary;
  const error = record.error != null ? textField(record.error) : nestedText(output, ["error"]);
  const exit = nestedValue(output, ["exit_status", "status", "disposition"]);

  return (
    <>
      {cmd ? <OutputField label={t("inspector.command")} value={cmd} /> : null}
      {stdout ? <OutputField label={t("inspector.stdout")} value={stdout} /> : null}
      {stderr ? <OutputField label={t("inspector.stderr")} value={stderr} /> : null}
      {initialOutput ? <OutputField label={t("inspector.initialOutput")} value={initialOutput} /> : null}
      {result ? <OutputField label={t("inspector.result")} value={result} /> : null}
      {error ? <OutputField label={t("inspector.error")} value={error} variant="error" /> : null}
      {exit != null ? <SimpleField label={t("inspector.exit")} value={exit} /> : null}
    </>
  );
}

// ExecCommandBatch renderer

function ExecCommandBatchRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const output = unwrapToolOutput(record.output ?? record.result);
  const items = isRecord(output) && Array.isArray(output.items) ? (output.items as unknown[]) : [];
  const inputItems = isRecord(record.input) && Array.isArray(record.input.items) ? (record.input.items as unknown[]) : [];
  const summary = record.summary;
  const error = record.error != null ? textField(record.error) : nestedText(output, ["error"]);
  const resultSummary = nestedText(output, ["summary", "summary_text", "result_summary", "result_summary_preview"]);

  return (
    <>
      {summary ? <OutputField label={t("inspector.summary")} value={summary} /> : null}
      {resultSummary ? <OutputField label={t("inspector.result")} value={resultSummary} /> : null}
      {error ? <OutputField label={t("inspector.error")} value={error} variant="error" /> : null}
      {items.map((rawItem, index) => {
        if (!isRecord(rawItem)) return null;
        const itemResult = isRecord(rawItem.result) ? rawItem.result : rawItem;
        const cmd = textField(rawItem.cmd) || commandText(inputItems[index]);
        const itemStdout = nestedText(itemResult, ["stdout", "stdout_preview", "output", "output_preview", "combined_output_preview"]);
        const itemStderr = nestedText(itemResult, ["stderr", "stderr_preview"]);
        const itemResultText = nestedText(itemResult, ["summary", "summary_text", "result_summary", "result_summary_preview"]);
        const itemError = nestedText(itemResult, ["error"]);
        const itemExit = nestedValue(itemResult, ["exit_status", "status", "disposition"]);
        const hasContent = cmd || itemStdout || itemStderr || itemResultText || itemError || itemExit != null;
        if (!hasContent) return null;
        return (
          <section key={index} className="tool-detail-batch-item">
            <h3 className="tool-detail-batch-item-title">
              {t("inspector.batchItem")} {typeof rawItem.index === "number" ? rawItem.index : index + 1}
            </h3>
            {cmd ? <OutputField label={t("inspector.command")} value={cmd} /> : null}
            {itemStdout ? <OutputField label={t("inspector.stdout")} value={itemStdout} /> : null}
            {itemStderr ? <OutputField label={t("inspector.stderr")} value={itemStderr} /> : null}
            {itemResultText ? <OutputField label={t("inspector.result")} value={itemResultText} /> : null}
            {itemError ? <OutputField label={t("inspector.error")} value={itemError} variant="error" /> : null}
            {itemExit != null ? <SimpleField label={t("inspector.exit")} value={itemExit} /> : null}
          </section>
        );
      })}
    </>
  );
}

// ApplyPatch renderer

function ApplyPatchRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "apply_patch_result");
  const changedFiles = arrayRecords(result?.changed_files);
  const changedPaths = stringArray(result?.changed_paths);
  const diagnostics = arrayRecords(result?.diagnostics);
  const patchText = patchInputText(record.input) || textField(record.patch) || textField(result?.patch);
  const diffPreview =
    textField(result?.diff_preview) ||
    changedFiles.map((file) => textField(file.diff_preview)).filter(Boolean).join("\n\n");
  const resultSummary = textField(result?.summary_text) || record.summary;
  const error = record.error;

  return (
    <>
      {changedFiles.length > 0 || changedPaths.length > 0 ? (
        <section className="tool-detail-field">
          <h3 className="tool-detail-field-label">{t("inspector.changedFiles")}</h3>
          <ul className="tool-detail-file-list">
            {changedFiles.map((file, i) => {
              const action = textField(file.action);
              const path = textField(file.path) || "unknown path";
              return (
                <li key={`f${i}`} className={`tool-detail-file${action ? ` tool-detail-file--${action}` : ""}`}>
                  {action ? <span className="tool-detail-file-action">{action}</span> : null}
                  <span className="tool-detail-file-path">{path}</span>
                </li>
              );
            })}
            {changedPaths.map((path, i) => (
              <li key={`p${i}`} className="tool-detail-file">
                <span className="tool-detail-file-path">{path}</span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {diagnostics.length > 0 ? (
        <section className="tool-detail-field">
          <h3 className="tool-detail-field-label">{t("inspector.diagnostics")}</h3>
          <ul className="tool-detail-diagnostic-list">
            {diagnostics.map((diag, i) => {
              const level = textField(diag.level);
              const message = textField(diag.message);
              return (
                <li key={i} className={`tool-detail-diagnostic${level ? ` tool-detail-diagnostic--${level}` : ""}`}>
                  {level ? <span className="tool-detail-diagnostic-level">{level}</span> : null}
                  <span className="tool-detail-diagnostic-message">{message || "—"}</span>
                </li>
              );
            })}
          </ul>
        </section>
      ) : null}

      {patchText ? <OutputField label={t("inspector.patch")} value={patchText} variant="diff" /> : null}
      {!patchText && diffPreview ? <OutputField label={t("inspector.patchPreview")} value={diffPreview} variant="diff" /> : null}

      {resultSummary ? <OutputField label={t("inspector.result")} value={resultSummary} /> : null}
      {error ? <OutputField label={t("inspector.error")} value={textField(error)} variant="error" /> : null}
    </>
  );
}

// Generic fallback renderer

function GenericToolRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const formatted = formatToolExecutionDetail(record);
  return <OutputField label={record.tool_name ?? "Detail"} value={formatted.text} />;
}

// Public router component

export function ToolExecutionContent({ record }: { record: RuntimeToolExecutionRecord }) {
  switch (record.tool_name) {
    case "ExecCommand":
      return <ExecCommandRenderer record={record} />;
    case "ExecCommandBatch":
      return <ExecCommandBatchRenderer record={record} />;
    case "ApplyPatch":
      return <ApplyPatchRenderer record={record} />;
    default:
      return <GenericToolRenderer record={record} />;
  }
}
