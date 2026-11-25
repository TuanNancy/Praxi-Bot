import assert from "node:assert/strict";
import { test } from "node:test";
import { cleanCode, isCodeValid, isHandoverGone, siteLabel } from "./handoverForm.ts";

test("accepts pasted codes with spaces or dashes and rejects anything else", () => {
  assert.equal(cleanCode(" 123 456\n"), "123456");
  assert.equal(cleanCode("123-456"), "123456");
  assert.equal(isCodeValid("123 456"), true);
  assert.equal(isCodeValid("AB12cd"), true);
  assert.equal(isCodeValid("12"), false);
  assert.equal(isCodeValid("1234567890123"), false);
  assert.equal(isCodeValid("12 34 <b>"), false);
});

test("shows the host the browser reported, not the port", () => {
  assert.equal(siteLabel("https://bank.example:443"), "bank.example");
  assert.equal(siteLabel("not a url"), "not a url");
});

test("knows the code that closes the card", () => {
  assert.equal(isHandoverGone("handover_gone"), true);
  assert.equal(isHandoverGone("handover_not_ready"), false);
});
