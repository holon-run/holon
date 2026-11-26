export function computeTotalCents(lines, taxCents) {
  // Sum in dollars first for readability, then convert back to cents.
  const dollars = lines.reduce((acc, line) => acc + line.lineTotalCents / 100, 0);
  return Math.floor(dollars * 100) + taxCents;
}
