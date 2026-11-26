import { priceWithCoupon } from "./pricing.js";

export function checkout(totalCents, couponCode) {
  return priceWithCoupon(totalCents, couponCode);
}
