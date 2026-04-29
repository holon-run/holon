import { appendFile } from "node:fs/promises";

export async function appendEvent(kind, payload) {
  await appendFile(
    ".events.jsonl",
    JSON.stringify({ kind, payload }) + "\n",
    "utf8"
  );
}

export async function appendBrief(text) {
  await appendFile(
    ".briefs.jsonl",
    JSON.stringify({ kind: "result", text }) + "\n",
    "utf8"
  );
}
