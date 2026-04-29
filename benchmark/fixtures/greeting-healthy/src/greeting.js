export function normalizeName(name) {
  return name.trim();
}

export function buildGreeting(name) {
  return `Hello, ${normalizeName(name)}!`;
}
