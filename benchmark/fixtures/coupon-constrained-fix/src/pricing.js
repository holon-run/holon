import { applyCouponTotal } from "./coupons.js";

export function priceWithCoupon(totalCents, code) {
  return applyCouponTotal(totalCents, code);
}
