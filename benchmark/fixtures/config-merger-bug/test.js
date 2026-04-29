import assert from "node:assert/strict";
import { buildConfig } from "./src/index.js";

const config = buildConfig({
  ui: {
    density: "compact"
  },
  notifications: {
    sms: false
  }
});

assert.equal(config.ui.theme, "light");
assert.equal(config.ui.density, "compact");
assert.equal(config.notifications.email, true);
assert.equal(config.notifications.sms, false);

console.log("tests passed");
