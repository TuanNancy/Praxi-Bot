import assert from "node:assert/strict";
import { test } from "node:test";
import { canSave, effectKey, needsAcknowledgement } from "./skillFromChat.ts";

const base = { dirty: false, risk: "low", tainted: false, acknowledged: false, busy: false };

test("a clean, scanned draft can be saved without a tick", () => {
  assert.equal(canSave(base), true);
});

test("edited text must be scanned again before saving", () => {
  assert.equal(canSave({ ...base, dirty: true }), false);
});

test("risk or outside content needs the tick", () => {
  for (const over of [{ risk: "medium" }, { risk: "high" }, { tainted: true }]) {
    assert.equal(needsAcknowledgement(over.risk ?? "low", over.tainted ?? false), true);
    assert.equal(canSave({ ...base, ...over }), false);
    assert.equal(canSave({ ...base, ...over, acknowledged: true }), true);
  }
});

test("nothing is saved while busy; unknown effects get a safe label", () => {
  assert.equal(canSave({ ...base, busy: true }), false);
  assert.equal(effectKey("write"), "skillFromChat.effect.write");
  assert.equal(effectKey("surprise"), "skillFromChat.effect.unknown");
});
