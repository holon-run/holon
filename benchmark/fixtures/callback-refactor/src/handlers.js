import { getUserOrders } from "./api.js";

export function renderOrdersSummary(userId, callback) {
  getUserOrders(userId, (err, orders) => {
    if (err) return callback(err);
    callback(null, `user ${userId} has ${orders.length} orders`);
  });
}

export function renderFirstOrder(userId, callback) {
  getUserOrders(userId, (err, orders) => {
    if (err) return callback(err);
    callback(null, orders[0] ? orders[0].item : "none");
  });
}
