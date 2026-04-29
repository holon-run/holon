import { appendEvent, appendReport } from "./storage.js";
import { parseEnvelope } from "./parser.js";
import { summarizeRun } from "./report.js";

export class Runtime {
  constructor(queue) {
    this.queue = queue;
  }

  async runOnce() {
    const raw = this.queue.shift();
    if (!raw) {
      return { status: "idle" };
    }

    const envelope = parseEnvelope(raw);
    await appendEvent("message_received", envelope);
    const result = {
      status: "processed",
      kind: envelope.kind,
      metadata: envelope.metadata ?? null
    };
    await appendReport(summarizeRun(result));
    return result;
  }
}
