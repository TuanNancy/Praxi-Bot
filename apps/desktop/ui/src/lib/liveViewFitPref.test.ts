import assert from "node:assert/strict";
import { test } from "node:test";
import { DEFAULT_SCREEN_FIT, loadScreenFit, saveScreenFit, toggleScreenFit } from "./liveViewFitPref.ts";

function mapStorage(store = new Map<string, string>()): Pick<Storage, "getItem" | "setItem"> {
  return {
    getItem: (key) => store.get(key) ?? null,
    setItem: (key, value) => {
      store.set(key, value);
    }
  };
}

test("the screen fills its box (cover) unless the owner chose otherwise", () => {
  assert.equal(DEFAULT_SCREEN_FIT, "cover");
  const storage = mapStorage();
  assert.equal(loadScreenFit(storage), "cover");
  saveScreenFit("contain", storage);
  assert.equal(loadScreenFit(storage), "contain");
  saveScreenFit("cover", storage);
  assert.equal(loadScreenFit(storage), "cover");
  assert.equal(loadScreenFit(mapStorage(new Map([["praxi.liveView.screenFit", "stretch"]]))), "cover");
  assert.equal(toggleScreenFit("cover"), "contain");
  assert.equal(toggleScreenFit("contain"), "cover");
});

test("a blocked storage never breaks the choice", () => {
  const throwing: Pick<Storage, "getItem" | "setItem"> = {
    getItem: () => {
      throw new Error("blocked");
    },
    setItem: () => {
      throw new Error("blocked");
    }
  };
  assert.equal(loadScreenFit(throwing), "cover");
  assert.doesNotThrow(() => saveScreenFit("contain", throwing));
  assert.equal(loadScreenFit(null), "cover");
});
