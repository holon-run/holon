export function normalizeName(name) {
  return name.trim().toLowerCase();
}

export function buildGreeting(name) {
  return `Hello, ${normalizeName(name)}!`;
}
