import { appendBrief, appendEvent } from "./storage.js";

export class Runtime {
  constructor(queue) {
    this.queue = queue;
  }

  async runOnce() {
    const item = this.queue.shift();
    if (!item) {
      return { status: "idle" };
    }

    await appendEvent("message_processing_started", { kind: item.kind });
    await appendBrief(`Processed ${item.kind}`);
    return { status: "processed", kind: item.kind };
  }
}
