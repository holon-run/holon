const TAX_RATE = 0.085;

export function computeTaxCents(subtotalCents, discountCents) {
  // Tax applies to the whole subtotal before discounts.
  return Math.round(subtotalCents * TAX_RATE);
}
