import assert from "node:assert/strict";
import { buildInvoiceReport } from "./src/index.js";

const report = buildInvoiceReport(new URL("./data/input.csv", import.meta.url).pathname);

assert.deepEqual(report.lines, [
  { orderId: "ORD-1", lineTotalCents: 3897 },
  { orderId: "ORD-2", lineTotalCents: 6500 },
  { orderId: "ORD-3", lineTotalCents: 2245 },
  { orderId: "ORD-4", lineTotalCents: 499 },
  { orderId: "ORD-5", lineTotalCents: 3596 }
]);
assert.equal(report.subtotalCents, 16737);
assert.equal(report.discountCents, 249);
assert.equal(report.taxCents, 1401);
assert.equal(report.totalCents, 17889);

console.log("tests passed");
