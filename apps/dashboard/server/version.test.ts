import { expect, test } from "bun:test";
import { formatVersionLine } from "./version";

test("formatVersionLine produces the EXACT --version contract line", () => {
  expect(formatVersionLine("0.1.0", "ed259c4f7856", "x86_64-unknown-linux-gnu")).toBe(
    "jurisearch-dashboard 0.1.0 (ed259c4f7856, x86_64-unknown-linux-gnu)",
  );
});

test("formatVersionLine reflects the override commit verbatim", () => {
  expect(formatVersionLine("0.1.0", "deadbeefcafe", "x86_64-unknown-linux-gnu")).toBe(
    "jurisearch-dashboard 0.1.0 (deadbeefcafe, x86_64-unknown-linux-gnu)",
  );
});
