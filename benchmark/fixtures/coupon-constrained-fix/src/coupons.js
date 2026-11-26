export function normalizeCoupon(code) {
  return code;
}

export function applyCouponTotal(totalCents, code) {
  const normalized = normalizeCoupon(code);
  if (normalized === "SAVE10") {
    return totalCents * 0.9;
  }
  if (normalized === "FIVE") {
    return totalCents - 500;
  }
  return totalCents;
}
