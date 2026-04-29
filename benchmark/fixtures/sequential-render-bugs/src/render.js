import { normalizeName } from "./name.js";
import { formatRole } from "./role.js";

export function renderProfile(profile) {
  return `${normalizeName(profile.name)} (${formatRole(profile.role)})`;
}
