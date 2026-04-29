import { appendFile } from "node:fs/promises";

export async function appendEvent(kind, payload) {
  await appendFile(
    ".events.jsonl",
    JSON.stringify({ kind, payload }) + "\n",
    "utf8"
  );
}

export async function appendReport(text) {
  await appendFile(
    ".reports.jsonl",
    JSON.stringify({ kind: "summary", text }) + "\n",
    "utf8"
  );
}
