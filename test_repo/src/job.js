import { retryRequest } from "../src/client";

export function runJob() {
  return retryRequest(2);
}
