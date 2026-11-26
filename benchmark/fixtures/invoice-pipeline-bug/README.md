# Invoice pipeline

`src/index.js` builds an invoice report from `data/input.csv`.

Rules (normative):

- CSV columns: `order_id,product,quantity,unit_price_cents,discount_cents`.
- Product names are quoted and may contain commas.
- `line_total_cents = quantity * unit_price_cents` (integer cents).
- `subtotal_cents` = sum of all line totals.
- `discount_cents` = sum of all per-row discounts.
- Tax applies to the discounted base:
  `tax_base = subtotal - discount`,
  `tax_cents = round_half_up(tax_base * 0.085)`.
- `total_cents = tax_base + tax_cents`.
- All arithmetic stays in integer cents; no floating point money.

`node test.js` must exit 0 with the correct report.
