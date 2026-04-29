import assert from "node:assert/strict";
import { renderProfile } from "./src/render.js";

assert.equal(
  renderProfile({ name: "  Alice  ", role: "admin" }),
  "Alice (admin)"
);
assert.equal(
  renderProfile({ name: "Bob", role: "member" }),
  "Bob (member)"
);

console.log("tests passed");
