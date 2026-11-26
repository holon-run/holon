import assert from "node:assert/strict";
import * as api from "./src/api.js";
import { renderOrdersSummary, renderFirstOrder } from "./src/handlers.js";
import { ordersTotalCents } from "./src/report.js";
import { printSummary } from "./src/cli.js";

assert.equal(api.getUserOrders, undefined, "old callback API must be removed");
assert.equal(typeof api.fetchCustomerOrders, "function");

assert.deepEqual(await api.fetchCustomerOrders("u1"), [
  { id: "o-100", item: "keyboard", amountCents: 4900 },
  { id: "o-101", item: "mouse", amountCents: 1900 }
]);
assert.deepEqual(await api.fetchCustomerOrders("u2"), [{ id: "o-200", item: "monitor", amountCents: 34900 }]);
assert.deepEqual(await api.fetchCustomerOrders("u3"), []);
await assert.rejects(() => api.fetchCustomerOrders("nope"), /unknown user/);

assert.equal(await renderOrdersSummary("u1"), "user u1 has 2 orders");
assert.equal(await renderFirstOrder("u1"), "keyboard");
assert.equal(await renderFirstOrder("u3"), "none");
assert.equal(await ordersTotalCents("u1"), 6800);
assert.equal(await printSummary("u1"), "USER U1 HAS 2 ORDERS");

console.log("tests passed");
