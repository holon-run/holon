import { useTranslation } from "react-i18next";
import type { MouseEvent } from "react";
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

function truncatedText(value: string, maxChars: number): string {
  if (value.length <= maxChars) return value;
  const truncated = value.slice(0, maxChars - 1);
  const lastNewline = truncated.lastIndexOf("\n");
  const cutPoint = lastNewline > maxChars * 0.6 ? lastNewline : truncated.length;
  return `${truncated.slice(0, cutPoint)}…`;
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

function ViewImageRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "view_image_result");
  const dimensions = isRecord(result?.dimensions) ? result.dimensions : undefined;
  const width = nestedValue(result, ["width"]) ?? nestedValue(dimensions, ["width"]);
  const height = nestedValue(result, ["height"]) ?? nestedValue(dimensions, ["height"]);
  const path = nestedValue(record.input, ["path", "image_path"]) ?? nestedValue(result, ["path", "image_path"]);
  const observation = nestedText(result, ["visual_observation", "observation", "text_preview"]);
  const summary = textField(result?.summary_text) || record.summary;

  return (
    <>
      <SimpleField label={t("inspector.path")} value={path} />
      {width != null && height != null ? <SimpleField label={t("inspector.dimensions")} value={`${width}×${height}`} /> : null}
      {observation ? <OutputField label={t("inspector.observation")} value={observation} /> : null}
      {summary ? <OutputField label={t("inspector.result")} value={summary} /> : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

function GenerateImageRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "generate_image_result");
  const prompt = nestedValue(record.input, ["prompt"]) ?? nestedValue(result, ["prompt"]);
  const name = nestedValue(record.input, ["name"]) ?? nestedValue(result, ["name"]);
  const size = nestedValue(record.input, ["size"]) ?? nestedValue(result, ["size"]);
  const background = nestedValue(record.input, ["background"]) ?? nestedValue(result, ["background"]);
  const outputFormat = nestedValue(record.input, ["output_format"]) ?? nestedValue(result, ["output_format"]);
  const imageUri = nestedValue(result, ["image_uri", "uri", "path"]);
  const summary = textField(result?.summary_text) || record.summary;

  return (
    <>
      <SimpleField label={t("rightPanel.name")} value={name} />
      <SimpleField label={t("inspector.dimensions")} value={size} />
      <SimpleField label={t("inspector.path")} value={imageUri} />
      <SimpleField label={t("inspector.mode")} value={background} />
      <SimpleField label={t("rightPanel.output")} value={outputFormat} />
      {prompt ? <OutputField label={t("inspector.input")} value={String(prompt)} /> : null}
      {summary ? <OutputField label={t("inspector.result")} value={summary} /> : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

function WebSearchRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const results = arrayRecords(nestedValue(output, ["results"]));
  const query = nestedValue(output, ["query"]) ?? nestedValue(input, ["query", "search_query", "q"]);
  const provider = nestedValue(output, ["provider"]);
  const mode = nestedValue(output, ["mode"]);

  return (
    <>
      <SimpleField label={t("inspector.query")} value={query} />
      <SimpleField label={t("inspector.provider")} value={provider} />
      <SimpleField label={t("inspector.mode")} value={mode} />
      <SimpleField label={t("inspector.results")} value={t("inspector.resultsCount", { count: results.length })} />
      {results.slice(0, 15).map((item, index) => {
        const title = nestedText(item, ["title"]) || t("inspector.untitled");
        const url = nestedText(item, ["url"]);
        const source = nestedText(item, ["source"]);
        const publishedAt = nestedText(item, ["published_at"]);
        const snippet = nestedText(item, ["snippet"]);
        const text = [url, source ? `(${source})` : "", publishedAt, snippet ? truncatedText(snippet, 300) : ""].filter(Boolean).join("\n");
        return <OutputField key={index} label={`${index + 1}. ${title}`} value={text} />;
      })}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

function WebFetchRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const url = nestedValue(output, ["url"]) ?? nestedValue(input, ["url"]);
  const finalUrl = nestedValue(output, ["final_url"]);
  const truncated = nestedValue(output, ["truncated"]) === true;
  const content = nestedText(output, ["text"]);

  return (
    <>
      <SimpleField label={t("inspector.url")} value={url} />
      {finalUrl && finalUrl !== url ? <SimpleField label={t("inspector.finalUrl")} value={finalUrl} /> : null}
      <SimpleField label={t("inspector.status")} value={nestedValue(output, ["status"])} />
      <SimpleField label={t("inspector.contentType")} value={nestedValue(output, ["content_type"])} />
      <SimpleField label={t("inspector.bytesRead")} value={nestedValue(output, ["bytes_read"])} />
      {truncated ? <SimpleField label={t("inspector.truncated")} value={t("inspector.yes")} /> : null}
      {content ? <OutputField label={t("inspector.content")} value={truncatedText(content, 2000)} /> : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

function MemorySearchRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const results = arrayRecords(nestedValue(output, ["results"]));
  const query = nestedValue(output, ["query"]) ?? nestedValue(input, ["query"]);

  return (
    <>
      <SimpleField label={t("inspector.query")} value={query} />
      <SimpleField label={t("inspector.results")} value={t("inspector.resultsCount", { count: results.length })} />
      {results.slice(0, 15).map((item, index) => {
        const sourceRef = nestedText(item, ["source_ref"]) || t("inspector.unknownSource");
        const score = nestedText(item, ["score"]);
        const preview = nestedText(item, ["preview"]);
        const text = [score ? `score: ${score}` : "", preview ? truncatedText(preview, 300) : ""].filter(Boolean).join("\n");
        return <OutputField key={index} label={`${index + 1}. ${sourceRef}`} value={text} />;
      })}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

function MemoryGetRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const sourceRef = nestedValue(output, ["source_ref"]) ?? nestedValue(input, ["source_ref"]);
  const content = nestedText(output, ["content"]);
  const truncated = nestedValue(output, ["truncated"]) === true;

  return (
    <>
      <SimpleField label={t("inspector.sourceRef")} value={sourceRef} />
      {truncated ? <SimpleField label={t("inspector.truncated")} value={t("inspector.yes")} /> : null}
      {content ? <OutputField label={t("inspector.content")} value={truncatedText(content, 2000)} /> : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

// UseWorkspace renderer

function UseWorkspaceRenderer({
  record,
  onBrowseFiles,
}: {
  record: RuntimeToolExecutionRecord;
  onBrowseFiles?: (workspaceId: string, executionRootId?: string) => void;
}) {
  const { t } = useTranslation();
  const output = unwrapToolOutput(record.output ?? record.result);
  const result = isRecord(output) ? output : undefined;
  const input = isRecord(record.input) ? record.input : {};

  const workspaceId = nestedText(result, ["workspace_id"]) || nestedText(input, ["workspace_id"]);
  const workspaceAnchor = nestedText(result, ["workspace_anchor"]) || nestedText(input, ["path"]);
  const executionRoot = nestedText(result, ["execution_root"]);
  const executionRootId = nestedText(result, ["execution_root_id"]);
  const cwd = nestedText(result, ["cwd"]);
  const mode = nestedText(result, ["mode"]) || nestedText(input, ["mode"]);
  const projectionKind = nestedText(result, ["projection_kind"]);
  const summary = textField(result?.summary_text) || record.summary;

  const canBrowse = !!workspaceId && !!onBrowseFiles;
  const browse = canBrowse
    ? (e: MouseEvent) => {
        e.preventDefault();
        onBrowseFiles!(workspaceId, executionRootId || undefined);
      }
    : undefined;

  return (
    <>
      {workspaceId ? <SimpleField label={t("inspector.workspaceId")} value={workspaceId} /> : null}
      {workspaceAnchor ? (
        <div className="tool-detail-simple">
          <span className="tool-detail-simple-label">{t("inspector.workspacePath")}</span>
          {browse ? (
            <a href="#" className="workspace-path-link" onClick={browse}>
              {workspaceAnchor}
            </a>
          ) : (
            <span className="tool-detail-simple-value">{workspaceAnchor}</span>
          )}
        </div>
      ) : null}
      {executionRoot ? (
        <div className="tool-detail-simple">
          <span className="tool-detail-simple-label">{t("inspector.executionRoot")}</span>
          {browse ? (
            <a href="#" className="workspace-path-link" onClick={browse}>
              {executionRoot}
            </a>
          ) : (
            <span className="tool-detail-simple-value">{executionRoot}</span>
          )}
        </div>
      ) : null}
      {cwd ? <SimpleField label={t("inspector.cwd")} value={cwd} /> : null}
      {mode ? <SimpleField label={t("inspector.mode")} value={mode} /> : null}
      {projectionKind ? <SimpleField label={t("inspector.projectionKind")} value={projectionKind} /> : null}
      {canBrowse ? (
        <a href="#" className="workspace-path-link tool-detail-browse-link" onClick={browse}>
          {t("fileBrowser.openInFileBrowser")}
        </a>
      ) : null}
      {summary ? <OutputField label={t("inspector.result")} value={summary} /> : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

// Generic fallback renderer

function GenericToolRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const formatted = formatToolExecutionDetail(record);
  return <OutputField label={record.tool_name ?? "Detail"} value={formatted.text} />;
}

// Public router component

export function ToolExecutionContent({
  record,
  onBrowseFiles,
}: {
  record: RuntimeToolExecutionRecord;
  onBrowseFiles?: (workspaceId: string, executionRootId?: string) => void;
}) {
  switch (record.tool_name) {
    case "ExecCommand":
      return <ExecCommandRenderer record={record} />;
    case "ExecCommandBatch":
      return <ExecCommandBatchRenderer record={record} />;
    case "ApplyPatch":
      return <ApplyPatchRenderer record={record} />;
    case "ViewImage":
      return <ViewImageRenderer record={record} />;
    case "GenerateImage":
      return <GenerateImageRenderer record={record} />;
    case "WebSearch":
      return <WebSearchRenderer record={record} />;
    case "WebFetch":
      return <WebFetchRenderer record={record} />;
    case "MemorySearch":
      return <MemorySearchRenderer record={record} />;
    case "MemoryGet":
      return <MemoryGetRenderer record={record} />;
    case "UseWorkspace":
      return <UseWorkspaceRenderer record={record} onBrowseFiles={onBrowseFiles} />;
    default:
      return <GenericToolRenderer record={record} />;
  }
}
