import assert from "node:assert/strict";
import { buildGreeting } from "./src/greeting.js";

assert.equal(buildGreeting("  Alice  "), "Hello, Alice!");
assert.equal(buildGreeting("Bob"), "Hello, Bob!");

console.log("tests passed");
