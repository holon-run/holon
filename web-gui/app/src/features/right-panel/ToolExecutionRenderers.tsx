import { useTranslation } from "react-i18next";
import type { MouseEvent, ReactNode } from "react";
import { formatToolExecutionDetail } from "../inspector/ActivityInspectorPanel";
import { useRuntimeStore } from "../../runtime/runtime-store";
import { parseWorkspaceImageRef, WorkspaceImage } from "../../components/MarkdownContent";
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

/** A labeled field whose value is a clickable external link. */
export function LinkField({ label, href, text }: { label: string; href: string; text?: string }) {
  const display = text || href;
  return (
    <div className="tool-detail-simple">
      <span className="tool-detail-simple-label">{label}</span>
      <a href={href} target="_blank" rel="noopener noreferrer" className="tool-detail-link">
        {display}
      </a>
    </div>
  );
}

/** Renders a workspace:// image URI as an actual preview when possible. */
export function ImagePreview({ uri, alt }: { uri: string; alt?: string }) {
  const ref = parseWorkspaceImageRef(uri);
  if (!ref) {
    return <SimpleField label="Image" value={uri} />;
  }
  return (
    <section className="tool-detail-field">
      <h3 className="tool-detail-field-label">Image preview</h3>
      <div className="tool-detail-image-preview">
        <WorkspaceImage workspaceId={ref.workspaceId} path={ref.path} alt={alt ?? ref.path} />
      </div>
    </section>
  );
}

/** A card-style result item with optional clickable link, source badge, and snippet. */
export function ResultCard({
  index,
  title,
  url,
  source,
  snippet,
  extra,
}: {
  index?: number;
  title: string;
  url?: string;
  source?: string;
  snippet?: string;
  extra?: ReactNode;
}) {
  return (
    <div className="tool-detail-result-card">
      <div className="tool-detail-result-title">
        {index != null ? <span className="tool-detail-result-index">{index}.</span> : null}
        {url ? (
          <a href={url} target="_blank" rel="noopener noreferrer" className="tool-detail-link">
            {title}
          </a>
        ) : (
          <span>{title}</span>
        )}
      </div>
      {source ? <span className="tool-detail-result-source">{source}</span> : null}
      {snippet ? <p className="tool-detail-result-snippet">{snippet}</p> : null}
      {extra}
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
  const agentId = typeof record.agent_id === "string" ? record.agent_id : undefined;
  const workspaceId = useRuntimeStore((s) =>
    agentId ? s.sessionsByAgentId[agentId]?.detail?.agent?.workspaceSummary?.id : undefined,
  );

  const output = unwrapToolOutput(record.output ?? record.result);
  const result = asResultRecord(output, "view_image_result");
  const visualRef = isRecord(result?.visual_reference) ? result.visual_reference : undefined;
  const sizeInfo = isRecord(visualRef?.size) ? visualRef.size : undefined;
  const width = nestedValue(sizeInfo, ["width"]);
  const height = nestedValue(sizeInfo, ["height"]);
  const inputPath = nestedText(record.input, ["path", "image_path"]);
  const resultPath = nestedText(visualRef, ["path"]);
  const displayPath = inputPath || resultPath;
  const mime = nestedText(visualRef, ["mime"]);
  const byteCount = nestedValue(visualRef, ["byte_count"]);
  const sha256 = nestedText(visualRef, ["sha256"]);
  const refId = nestedText(visualRef, ["id"]);

  const observationObj = isRecord(result?.observation) ? result.observation : undefined;
  const observationSummary = nestedText(observationObj, ["summary"]);
  const generatedBy = isRecord(observationObj?.generated_by) ? observationObj.generated_by : undefined;
  const genProvider = nestedText(generatedBy, ["provider"]);
  const genModel = nestedText(generatedBy, ["model"]);
  const genMode = nestedText(generatedBy, ["mode"]);
  const ocrItems = arrayRecords(nestedValue(observationObj, ["ocr"]));
  const elementItems = arrayRecords(nestedValue(observationObj, ["elements"]));
  const uncertaintiesRaw = nestedValue(observationObj, ["uncertainties"]);
  const uncertainties = Array.isArray(uncertaintiesRaw)
    ? uncertaintiesRaw
        .map((u) =>
          typeof u === "string" ? u : nestedText(u, ["text", "description", "summary", "message"]),
        )
        .filter(Boolean)
    : [];
  const summary = textField(result?.summary_text) || record.summary;

  // Construct workspace URI for relative/absolute paths that aren't already workspace://
  const imageUri = displayPath?.startsWith("workspace://")
    ? displayPath
    : displayPath && workspaceId
      ? `workspace://${workspaceId}/${displayPath.replace(/^\/+/, "")}`
      : displayPath;

  return (
    <>
      <SimpleField label={t("inspector.path")} value={displayPath} />
      {refId ? <SimpleField label="ID" value={refId} /> : null}
      {mime ? <SimpleField label={t("inspector.contentType")} value={mime} /> : null}
      {byteCount != null ? (
        <SimpleField label={t("inspector.bytesRead")} value={String(byteCount)} />
      ) : null}
      {width != null && height != null ? <SimpleField label={t("inspector.dimensions")} value={`${width}×${height}`} /> : null}
      {sha256 ? <SimpleField label="SHA-256" value={sha256} /> : null}
      {genProvider || genModel ? (
        <SimpleField label={t("inspector.model")} value={[genProvider, genModel].filter(Boolean).join("/")} />
      ) : null}
      {genMode ? <SimpleField label={t("inspector.mode")} value={genMode} /> : null}
      {observationSummary ? <OutputField label={t("inspector.observation")} value={observationSummary} /> : null}
      {ocrItems.length > 0 ? (
        <OutputField label="OCR" value={JSON.stringify(ocrItems, null, 2)} />
      ) : null}
      {elementItems.length > 0 ? (
        <OutputField label="Elements" value={JSON.stringify(elementItems, null, 2)} />
      ) : null}
      {uncertainties.length > 0 ? (
        <OutputField label="Uncertainties" value={uncertainties.join("; ")} />
      ) : null}
      {summary ? <OutputField label={t("inspector.result")} value={summary} /> : null}
      {imageUri ? <ImagePreview uri={imageUri} alt={observationSummary ?? displayPath} /> : null}
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
  const images = arrayRecords(nestedValue(result, ["images"]));
  const firstImageUri = images.length > 0 ? nestedText(images[0], ["uri", "path"]) : "";
  const imageUri = nestedValue(result, ["image_uri", "uri", "path"]) ?? firstImageUri;
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
      {imageUri ? <ImagePreview uri={String(imageUri)} alt={String(prompt ?? name ?? "Generated image")} /> : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

function WebSearchRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const results = arrayRecords(nestedValue(output, ["results"]) ?? nestedValue(output, ["citations"]));
  const query = nestedValue(output, ["query"]) ?? nestedValue(input, ["query", "search_query", "q"]);
  const provider = nestedValue(output, ["provider"]) ?? nestedValue(output, ["backend"]);
  const mode = nestedValue(output, ["mode"]);
  const truncated = nestedValue(output, ["truncated"]) === true;

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
        const meta = [publishedAt].filter(Boolean).join(" · ");
        return (
          <ResultCard
            key={index}
            index={index + 1}
            title={title}
            url={url}
            source={[source, meta].filter(Boolean).join(" · ") || undefined}
            snippet={snippet ? truncatedText(snippet, 300) : undefined}
          />
        );
      })}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

function WebFetchRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const url = nestedText(output, ["url"]) ?? nestedText(input, ["url"]);
  const finalUrl = nestedText(output, ["final_url"]);
  const truncated = nestedValue(output, ["truncated"]) === true;
  const content = nestedText(output, ["text"]);

  return (
    <>
      {url ? <LinkField label={t("inspector.url")} href={url} /> : null}
      {finalUrl && finalUrl !== url ? <LinkField label={t("inspector.finalUrl")} href={finalUrl} /> : null}
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
        const title = nestedText(item, ["title"]);
        const preview = nestedText(item, ["preview"]) || nestedText(item, ["snippet"]);
        const text = [title, score ? `score: ${score}` : "", preview ? truncatedText(preview, 300) : ""].filter(Boolean).join("\n");
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
  const result = asResultRecord(output, "memory");
  const sourceRef = nestedText(result, ["source_ref"]) ?? nestedText(input, ["source_ref"]);
  const title = nestedText(result, ["title"]);
  const kind = nestedText(result, ["kind"]);
  const content = nestedText(result, ["content"]);
  const truncated = nestedValue(result, ["truncated"]) === true;

  return (
    <>
      <SimpleField label={t("inspector.sourceRef")} value={sourceRef} />
      {title ? <SimpleField label={t("inspector.title")} value={title} /> : null}
      {kind ? <SimpleField label={t("inspector.kind")} value={kind} /> : null}
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

// XSearch renderer

function XSearchRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const query = nestedText(input, ["query"]);
  const provider = nestedValue(output, ["provider"]);
  const model = nestedValue(output, ["model"]);
  const backend = nestedValue(output, ["backend"]);
  const summaryText = nestedText(output, ["summary_text"]);
  const text = nestedText(output, ["text"]);
  const citations = arrayRecords(nestedValue(output, ["citations"]));

  return (
    <>
      <SimpleField label={t("inspector.query")} value={query} />
      <SimpleField label={t("inspector.provider")} value={provider} />
      <SimpleField label={t("inspector.model", { defaultValue: "Model" })} value={model} />
      <SimpleField label={t("inspector.mode")} value={backend} />
      {summaryText ? <OutputField label={t("inspector.summary")} value={summaryText} /> : null}
      {text ? <OutputField label={t("inspector.content")} value={truncatedText(text, 2000)} /> : null}
      {citations.length ? (
        <section className="tool-detail-field">
          <h3 className="tool-detail-field-label">{t("inspector.sources")}</h3>
          {citations.map((citation, index) => {
            const url = nestedText(citation, ["url"]);
            const title = nestedText(citation, ["title"]) || `Citation ${index + 1}`;
            return url ? (
              <ResultCard key={index} index={index + 1} title={title} url={url} />
            ) : (
              <ResultCard key={index} index={index + 1} title={title} />
            );
          })}
        </section>
      ) : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

// ListModelProviders renderer

function ListModelProvidersRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const output = unwrapToolOutput(record.output ?? record.result);
  const providers = arrayRecords(
    nestedValue(output, ["providers"]) ?? nestedValue(output, ["model_providers"]) ?? nestedValue(output, ["model_discovery_cache"]),
  );

  return (
    <>
      <SimpleField label={t("inspector.results")} value={t("inspector.resultsCount", { count: providers.length })} />
      {providers.map((provider, index) => {
        const id = nestedText(provider, ["id"]);
        const providerName = nestedText(provider, ["provider"]);
        const displayName = nestedText(provider, ["display_name"]);
        const availability = nestedText(provider, ["availability"]) ?? nestedText(provider, ["state"]);
        const family = nestedText(provider, ["provider_family"]);
        const providerConfigured = nestedValue(provider, ["provider_configured"]);
        const credentialConfigured = nestedValue(provider, ["credential_configured"]);
        const modelCount = nestedValue(provider, ["model_count"]);
        const discoveredCount = nestedValue(provider, ["discovered_model_count"]);
        const defaultModel = nestedText(provider, ["default_model"]);
        const endpoint = nestedText(provider, ["endpoint"]);
        return (
          <section key={index} className="tool-detail-result-card">
            <div className="tool-detail-result-title">
              <span>{displayName || id || providerName || "—"}</span>
              {availability ? <span className={`tool-detail-badge tool-detail-badge--${availability === "available" ? "ok" : "warn"}`}>{availability}</span> : null}
            </div>
            {id ? <p className="tool-detail-result-snippet">{id}</p> : null}
            <dl className="tool-detail-meta-grid">
              {family ? <div><dt>family</dt><dd>{family}</dd></div> : null}
              {endpoint ? <div><dt>endpoint</dt><dd>{endpoint}</dd></div> : null}
              {providerConfigured != null ? <div><dt>configured</dt><dd>{providerConfigured ? "✓" : "✗"}</dd></div> : null}
              {credentialConfigured != null ? <div><dt>credential</dt><dd>{credentialConfigured ? "✓" : "✗"}</dd></div> : null}
              {modelCount != null ? <div><dt>models</dt><dd>{String(modelCount)}{discoveredCount != null ? ` (${discoveredCount} discovered)` : ""}</dd></div> : null}
              {defaultModel ? <div><dt>default</dt><dd>{defaultModel}</dd></div> : null}
            </dl>
          </section>
        );
      })}
      {providers.length === 0 ? (
        <OutputField label={t("inspector.result")} value={JSON.stringify(output, null, 2)} />
      ) : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

// ListProviderModels renderer

function ListProviderModelsRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const output = unwrapToolOutput(record.output ?? record.result);
  const provider = nestedText(output, ["provider"]);
  const returned = nestedValue(output, ["returned"]);
  const limit = nestedValue(output, ["limit"]);
  const nextCursor = nestedText(output, ["next_cursor"]);
  const models = arrayRecords(nestedValue(output, ["models"]));

  return (
    <>
      <SimpleField label={t("inspector.provider")} value={provider} />
      <SimpleField label={t("inspector.returned")} value={returned != null ? String(returned) : undefined} />
      <SimpleField label={t("inspector.limit", { defaultValue: "Limit" })} value={limit != null ? String(limit) : undefined} />
      {nextCursor ? <SimpleField label={t("inspector.nextCursor", { defaultValue: "Next cursor" })} value={nextCursor} /> : null}
      {models.map((model, index) => {
        const id = nestedText(model, ["id"]);
        const modelRef = nestedText(model, ["model_ref"]);
        const displayName = nestedText(model, ["display_name"]);
        const availability = nestedText(model, ["availability"]);
        const selectable = nestedValue(model, ["selectable"]);
        const unavailableReason = nestedText(model, ["unavailable_reason"]);
        return (
          <section key={index} className="tool-detail-result-card">
            <div className="tool-detail-result-title">
              <span>{displayName || id || "—"}</span>
              {availability ? <span className={`tool-detail-badge tool-detail-badge--${availability === "available" ? "ok" : "warn"}`}>{availability}</span> : null}
              {selectable != null ? <span className={`tool-detail-badge tool-detail-badge--${selectable ? "ok" : "mute"}`}>{selectable ? "selectable" : "locked"}</span> : null}
            </div>
            {modelRef ? <p className="tool-detail-result-snippet">{modelRef}</p> : null}
            {unavailableReason ? <p className="tool-detail-result-snippet" style={{ color: "var(--danger)" }}>{unavailableReason}</p> : null}
          </section>
        );
      })}
      {models.length === 0 ? (
        <OutputField label={t("inspector.result")} value={JSON.stringify(output, null, 2)} />
      ) : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

// AgentGet renderer

function AgentGetRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const output = unwrapToolOutput(record.output ?? record.result);
  const rawAgent = nestedValue(output, ["agent"]);
  const agent: Record<string, unknown> = isRecord(rawAgent) ? rawAgent : (isRecord(output) ? output : {});
  const identity = isRecord(agent.identity) ? agent.identity : {};
  const model = isRecord(agent.model) ? agent.model : {};
  const agentId = nestedText(identity, ["agent_id"]);
  const visibility = nestedText(identity, ["visibility"]);
  const ownership = nestedText(identity, ["ownership"]);
  const profilePreset = nestedText(identity, ["profile_preset"]);
  const effectiveModel = nestedText(model, ["effective_model", "model_ref"]);
  const activeModel = nestedText(model, ["active_model", "model_ref"]);
  const activeTaskCount = nestedValue(agent, ["active_task_count"]);
  const children = arrayRecords(nestedValue(agent, ["active_children"]));
  const waitConditions = arrayRecords(nestedValue(agent, ["active_wait_conditions"]));

  return (
    <>
      <SimpleField label="Agent ID" value={agentId} />
      {visibility ? <SimpleField label={t("inspector.mode")} value={visibility} /> : null}
      {ownership ? <SimpleField label="Ownership" value={ownership} /> : null}
      {profilePreset ? <SimpleField label="Preset" value={profilePreset} /> : null}
      {effectiveModel ? <SimpleField label={t("inspector.model", { defaultValue: "Model" })} value={effectiveModel} /> : null}
      {activeModel && activeModel !== effectiveModel ? <SimpleField label="Active" value={activeModel} /> : null}
      {activeTaskCount != null ? <SimpleField label={t("inspector.tasks")} value={String(activeTaskCount)} /> : null}
      {children.length ? (
        <section className="tool-detail-field">
          <h3 className="tool-detail-field-label">{t("inspector.tasks")} ({children.length})</h3>
          {children.map((child, index) => {
            const childId = nestedText(child, ["agent_id"]);
            const childPreset = nestedText(child, ["profile_preset"]);
            const childStatus = nestedText(child, ["status"]);
            return (
              <div key={index} className="tool-detail-result-card">
                <div className="tool-detail-result-title"><span>{childId || "—"}</span></div>
                <p className="tool-detail-result-snippet">{[childPreset, childStatus].filter(Boolean).join(" · ")}</p>
              </div>
            );
          })}
        </section>
      ) : null}
      {waitConditions.length ? (
        <section className="tool-detail-field">
          <h3 className="tool-detail-field-label">{t("inspector.wake", { defaultValue: "Wait" })} ({waitConditions.length})</h3>
          {waitConditions.map((wait, index) => {
            const wake = nestedText(wait, ["wake"]);
            const reason = nestedText(wait, ["reason"]);
            const resource = nestedText(wait, ["resource"]);
            return (
              <div key={index} className="tool-detail-result-card">
                <div className="tool-detail-result-title"><span>{wake || "—"}</span></div>
                <p className="tool-detail-result-snippet">{[reason, resource].filter(Boolean).join(" · ")}</p>
              </div>
            );
          })}
        </section>
      ) : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

// SpawnAgent renderer

function SpawnAgentRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const agentId = nestedText(output, ["agent_id"]);
  const childAgentId = nestedText(output, ["child_agent_id"]);
  const supervisionTaskId = nestedText(output, ["supervision_task_id"]);
  const taskHandle = isRecord(nestedValue(output, ["task_handle"])) ? nestedValue(output, ["task_handle"]) : undefined;
  const taskId = nestedText(taskHandle, ["task_id"]);
  const preset = nestedText(input, ["preset"]);
  const template = nestedText(input, ["template"]);
  const workspaceMode = nestedText(input, ["workspace_mode"]);
  const initialMessage = nestedText(input, ["initial_message"]);

  return (
    <>
      <SimpleField label="Agent ID" value={agentId} />
      {childAgentId ? <SimpleField label="Child agent" value={childAgentId} /> : null}
      {taskId || supervisionTaskId ? <SimpleField label="Task ID" value={taskId ?? supervisionTaskId} /> : null}
      {preset ? <SimpleField label="Preset" value={preset} /> : null}
      {template ? <SimpleField label="Template" value={template} /> : null}
      {workspaceMode ? <SimpleField label="Workspace" value={workspaceMode} /> : null}
      {initialMessage ? <OutputField label={t("inspector.input")} value={truncatedText(initialMessage, 500)} /> : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

// WaitFor renderer

function WaitForRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const input = isRecord(record.input) ? record.input : {};
  const wake = nestedText(input, ["wake"]);
  const reason = nestedText(input, ["reason"]);
  const resource = nestedText(input, ["resource"]);
  const recheckAfterMs = nestedValue(input, ["recheck_after_ms"]);

  return (
    <>
      <SimpleField label={t("inspector.wake", { defaultValue: "Wake" })} value={wake} />
      <SimpleField label={t("inspector.reason")} value={reason} />
      {resource ? <SimpleField label={t("inspector.resource")} value={resource} /> : null}
      {recheckAfterMs != null ? <SimpleField label={t("inspector.recheckAfter")} value={`${recheckAfterMs}ms`} /> : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

// Enqueue renderer

function EnqueueRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const text = nestedText(input, ["text"]);
  const priority = nestedText(input, ["priority"]);
  const enqueued = nestedValue(output, ["enqueued"]);
  const followUpText = nestedText(output, ["follow_up_text"]);
  const summaryText = nestedText(output, ["summary_text"]);

  return (
    <>
      <SimpleField label={t("inspector.mode")} value={priority} />
      {enqueued != null ? <SimpleField label="Enqueued" value={enqueued ? "✓" : "✗"} /> : null}
      {text ? <OutputField label={t("inspector.input")} value={truncatedText(text, 500)} /> : null}
      {followUpText ? <OutputField label={t("inspector.content")} value={followUpText} /> : null}
      {summaryText ? <OutputField label={t("inspector.summary")} value={summaryText} /> : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

// TaskOutput renderer

function TaskOutputRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const input = isRecord(record.input) ? record.input : {};
  const output = unwrapToolOutput(record.output ?? record.result);
  const taskId = nestedText(input, ["task_id"]);
  const status = nestedText(output, ["status"]);
  const disposition = nestedText(output, ["disposition"]);
  const stdout = nestedText(output, ["stdout", "text"]);
  const stderr = nestedText(output, ["stderr"]);
  const truncated = nestedValue(output, ["truncated"]) === true;

  return (
    <>
      <SimpleField label={t("inspector.taskId")} value={taskId} />
      {status ? <SimpleField label={t("common.status")} value={status} /> : null}
      {disposition ? <SimpleField label="Disposition" value={disposition} /> : null}
      {stdout ? <OutputField label={t("inspector.stdout")} value={truncatedText(stdout, 3000)} /> : null}
      {stderr ? <OutputField label={t("inspector.stderr")} value={truncatedText(stderr, 1000)} variant="error" /> : null}
      {truncated ? <SimpleField label={t("inspector.truncated")} value={t("inspector.yes")} /> : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
}

// Generic fallback renderer

function GenericToolRenderer({ record }: { record: RuntimeToolExecutionRecord }) {
  const { t } = useTranslation();
  const formatted = formatToolExecutionDetail(record);
  const inputText = record.input ? JSON.stringify(record.input, null, 2) : undefined;
  const output = unwrapToolOutput(record.output ?? record.result);
  const outputText = output ? JSON.stringify(output, null, 2) : undefined;
  const summary = record.summary || formatted.text;
  return (
    <>
      {summary ? <OutputField label={t("inspector.summary")} value={summary} /> : null}
      {inputText ? <OutputField label={t("inspector.input")} value={truncatedText(inputText, 2000)} /> : null}
      {outputText ? <OutputField label={t("inspector.result")} value={truncatedText(outputText, 2000)} /> : null}
      {record.error ? <OutputField label={t("inspector.error")} value={textField(record.error)} variant="error" /> : null}
    </>
  );
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
    case "XSearch":
      return <XSearchRenderer record={record} />;
    case "ListModelProviders":
      return <ListModelProvidersRenderer record={record} />;
    case "ListProviderModels":
      return <ListProviderModelsRenderer record={record} />;
    case "AgentGet":
      return <AgentGetRenderer record={record} />;
    case "SpawnAgent":
      return <SpawnAgentRenderer record={record} />;
    case "WaitFor":
      return <WaitForRenderer record={record} />;
    case "Enqueue":
      return <EnqueueRenderer record={record} />;
    case "TaskOutput":
      return <TaskOutputRenderer record={record} />;
    default:
      return <GenericToolRenderer record={record} />;
  }
}
