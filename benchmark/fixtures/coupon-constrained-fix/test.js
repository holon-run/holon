import assert from "node:assert/strict";
import { checkout } from "./src/index.js";

assert.equal(checkout(3500, "SAVE10"), 3150);
assert.equal(checkout(3490, "SAVE10"), 3141);
assert.equal(checkout(3500, "  save10  "), 3150);
assert.equal(checkout(3500, "Save10"), 3150);
assert.equal(checkout(300, "FIVE"), 0);
assert.equal(checkout(3500, "FIVE"), 3000);
assert.equal(checkout(3500, "NOPE"), 3500);

console.log("tests passed");
