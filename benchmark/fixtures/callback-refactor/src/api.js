const ORDERS = {
  u1: [
    { id: "o-100", item: "keyboard", amountCents: 4900 },
    { id: "o-101", item: "mouse", amountCents: 1900 }
  ],
  u2: [{ id: "o-200", item: "monitor", amountCents: 34900 }],
  u3: []
};

export function getUserOrders(userId, callback) {
  setTimeout(() => {
    if (!Object.prototype.hasOwnProperty.call(ORDERS, userId)) {
      callback(new Error("unknown user: " + userId));
      return;
    }
    callback(null, ORDERS[userId].slice());
  }, 0);
}
