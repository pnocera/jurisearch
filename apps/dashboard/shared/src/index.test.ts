import { expect, test } from "bun:test";
import { DASHBOARD_NAME } from "./index";

test("DASHBOARD_NAME is the Juridia brand string", () => {
  expect(DASHBOARD_NAME).toBe("Juridia — Update Server");
});
