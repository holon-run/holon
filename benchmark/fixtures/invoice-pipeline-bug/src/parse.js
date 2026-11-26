export function parseInvoiceCsv(text) {
  const lines = text.split("\n").filter((line) => line.trim().length > 0);
  return lines.map((line) => {
    const fields = line.split(",");
    return {
      orderId: fields[0],
      product: fields[1],
      quantity: parseFloat(fields[2]),
      unitPriceCents: parseFloat(fields[3]),
      discountCents: parseFloat(fields[4])
    };
  });
}
