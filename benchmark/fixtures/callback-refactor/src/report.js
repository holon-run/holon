import { getUserOrders } from "./api.js";

export function ordersTotalCents(userId, callback) {
  getUserOrders(userId, (err, orders) => {
    if (err) return callback(err);
    callback(null, orders.reduce((acc, order) => acc + order.amountCents, 0));
  });
}
