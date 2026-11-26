import { readFileSync } from "node:fs";
import { parseInvoiceCsv } from "./parse.js";
import { computeTaxCents } from "./tax.js";
import { computeTotalCents } from "./total.js";

export function buildInvoiceReport(csvPath) {
  const rows = parseInvoiceCsv(readFileSync(csvPath, "utf8"));
  const lines = rows.map((row) => ({
    orderId: row.orderId,
    lineTotalCents: row.quantity * row.unitPriceCents
  }));
  const subtotalCents = rows.reduce((acc, row) => acc + row.quantity * row.unitPriceCents, 0);
  const discountCents = rows.reduce((acc, row) => acc + row.discountCents, 0);
  const taxCents = computeTaxCents(subtotalCents, discountCents);
  const totalCents = computeTotalCents(lines, taxCents);
  return {
    lines,
    subtotalCents,
    discountCents,
    taxCents,
    totalCents
  };
}
