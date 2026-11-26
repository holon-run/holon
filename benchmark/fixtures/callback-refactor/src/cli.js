import { renderOrdersSummary } from "./handlers.js";

export function printSummary(userId, callback) {
  renderOrdersSummary(userId, (err, text) => {
    if (err) return callback(err);
    callback(null, text.toUpperCase());
  });
}
