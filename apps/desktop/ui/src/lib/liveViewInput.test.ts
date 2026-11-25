import assert from "node:assert/strict";
import { test } from "node:test";
import {
  chunkTextInput,
  classifyKeyDown,
  classifyKeyUp,
  computeContainedImageRect,
  computeImageRect,
  domButtonToInputButton,
  isAllowedKeyCode,
  KeyForwarder,
  type KeyLikeEvent,
  LiveInputQueue,
  mapClientPointToNormalized,
  normalizeWheelDelta,
  truncateUtf8
} from "./liveViewInput.ts";

test("object-fit: contain maps image bounds and excludes letterbox bars unless clamping drag", () => {
  // 1000x800 box containing 1000x500 image -> centered vertically (y=150..650)
  const rect = computeContainedImageRect(1000, 800, 1000, 500);
  assert.deepEqual(rect, { x: 0, y: 150, width: 1000, height: 500 });

  const box = { left: 100, top: 50, width: 1000, height: 800 };
  // Top letterbox bar (y = 50 + 50 = 100, before rendered top 50 + 150 = 200) -> rejected
  assert.equal(mapClientPointToNormalized(600, 100, box, 1000, 500), null);
  // Exact center -> (0.5, 0.5)
  const mid = mapClientPointToNormalized(600, 450, box, 1000, 500);
  assert.ok(mid);
  assert.ok(Math.abs(mid.x - 0.5) < 1e-6);
  assert.ok(Math.abs(mid.y - 0.5) < 1e-6);

  // Active capture clamps outside drag cleanly to [0, 1]
  assert.deepEqual(
    mapClientPointToNormalized(1500, 10, box, 1000, 500, { clamp: true }),
    { x: 1, y: 0 }
  );

  // Narrow viewport with wide image vs tall image in wide viewport (horizontal pillarbox)
  const pillar = mapClientPointToNormalized(110, 450, box, 400, 800);
  assert.equal(pillar, null);
});

test("LiveInputQueue coalesces pointer moves only and preserves clicks, keys and text", () => {
  const q = new LiveInputQueue();
  q.enqueue({
    type: "pointer",
    phase: "move",
    x: 0.1,
    y: 0.1,
    button: null,
    buttons: 0,
    modifiers: []
  });
  q.enqueue({
    type: "pointer",
    phase: "move",
    x: 0.25,
    y: 0.35,
    button: null,
    buttons: 0,
    modifiers: []
  });
  q.enqueue({
    type: "pointer",
    phase: "down",
    x: 0.25,
    y: 0.35,
    button: "left",
    buttons: 1,
    modifiers: []
  });
  q.enqueue({
    type: "pointer",
    phase: "up",
    x: 0.25,
    y: 0.35,
    button: "left",
    buttons: 0,
    modifiers: []
  });
  q.enqueue({
    type: "pointer",
    phase: "move",
    x: 0.5,
    y: 0.5,
    button: null,
    buttons: 0,
    modifiers: []
  });

  const batch = q.drainBatch();
  assert.equal(batch.length, 4);
  assert.equal(batch[0].type, "pointer");
  if (batch[0].type === "pointer") {
    assert.equal(batch[0].phase, "move");
    assert.equal(batch[0].x, 0.25);
  }
  assert.equal(batch[1].type, "pointer");
  assert.equal(batch[2].type, "pointer");
  assert.equal(batch[3].type, "pointer");
});

test("bounded queue overflows safely to reset and drainBatch enforces 32 events & 16 KiB", () => {
  const q = new LiveInputQueue(5);
  for (let i = 0; i < 6; i += 1) {
    q.enqueue({ type: "text", text: `line-${i}` });
  }
  assert.deepEqual(q.drainBatch(), [{ type: "reset" }]);

  const bigQ = new LiveInputQueue(100);
  for (let i = 0; i < 40; i += 1) {
    bigQ.enqueue({ type: "text", text: `k-${i}` });
  }
  const b1 = bigQ.drainBatch();
  const b2 = bigQ.drainBatch();
  assert.equal(b1.length, 32);
  assert.equal(b2.length, 8);
});

test("DOM code allowlist, IME composition suppression, wheel clamp, and UTF-8 safety", () => {
  assert.equal(isAllowedKeyCode("KeyA"), true);
  assert.equal(isAllowedKeyCode("Digit9"), true);
  assert.equal(isAllowedKeyCode("F12"), true);
  assert.equal(isAllowedKeyCode("BracketLeft"), true);
  assert.equal(isAllowedKeyCode("F13"), false);
  assert.equal(isAllowedKeyCode("Unidentified"), false);
  assert.equal(isAllowedKeyCode("MediaPlayPause"), false);

  // Active IME composition emits zero duplicate key events
  assert.deepEqual(
    classifyKeyDown(
      {
        code: "KeyS",
        key: "Process",
        location: 0,
        repeat: false,
        altKey: false,
        ctrlKey: false,
        metaKey: false,
        shiftKey: false,
        isComposing: true
      },
      true
    ),
    []
  );
  assert.deepEqual(
    classifyKeyUp(
      {
        code: "KeyS",
        key: "ş",
        location: 0,
        repeat: false,
        altKey: false,
        ctrlKey: false,
        metaKey: false,
        shiftKey: false,
        isComposing: true
      },
      true
    ),
    []
  );

  // Non-allowlisted single printable character falls back cleanly to text
  assert.deepEqual(
    classifyKeyDown(
      {
        code: "IntlBackslash",
        key: "ğ",
        location: 0,
        repeat: false,
        altKey: false,
        ctrlKey: false,
        metaKey: false,
        shiftKey: false
      },
      false
    ),
    [{ type: "text", text: "ğ" }]
  );

  assert.equal(normalizeWheelDelta(5000, 0), 1200);
  assert.equal(normalizeWheelDelta(-999, 2, 1000), -1200);
  assert.equal(domButtonToInputButton(0), "left");
  assert.equal(domButtonToInputButton(1), "middle");
  assert.equal(domButtonToInputButton(2), "right");
  assert.equal(domButtonToInputButton(4), null);

  const emoji = "😀".repeat(25);
  assert.ok(new TextEncoder().encode(truncateUtf8(emoji, 64)).byteLength <= 64);
  const longText = "ş".repeat(3000); // 6000 UTF-8 bytes
  const parts = chunkTextInput(longText);
  assert.equal(parts.length, 2);
  assert.ok(parts.every((p) => new TextEncoder().encode(p).byteLength <= 4096));
});

function keyEv(
  code: string,
  key: string,
  extra: Partial<KeyLikeEvent> = {}
): KeyLikeEvent {
  return {
    code,
    key,
    location: 0,
    repeat: false,
    altKey: false,
    ctrlKey: false,
    metaKey: false,
    shiftKey: false,
    ...extra
  };
}

test("Turkish Q keyboard: ş ğ ü ö ç ı and i travel as text, not as US-layout key codes", () => {
  // Physical keys of the Turkish Q layout and what they produce.
  const cases: Array<[string, string]> = [
    ["Semicolon", "ş"],
    ["BracketLeft", "ğ"],
    ["BracketRight", "ü"],
    ["Quote", "i"],
    ["Comma", "ö"],
    ["Period", "ç"],
    ["KeyI", "ı"],
    ["Semicolon", "Ş"],
    ["Quote", "İ"],
    ["Slash", "."],
    ["Backslash", ","]
  ];
  for (const [code, key] of cases) {
    assert.deepEqual(
      classifyKeyDown(keyEv(code, key, { shiftKey: key !== key.toLowerCase() }), false),
      [{ type: "text", text: key }],
      `${code} -> ${key}`
    );
  }
});

test("US-layout keys and named keys still travel as key events", () => {
  const down = classifyKeyDown(keyEv("KeyA", "a"), false);
  assert.equal(down.length, 1);
  assert.equal(down[0].type, "key");
  assert.equal(classifyKeyDown(keyEv("Enter", "Enter"), false)[0].type, "key");
  assert.equal(classifyKeyDown(keyEv("Space", " "), false)[0].type, "key");
  assert.equal(classifyKeyDown(keyEv("Semicolon", ";"), false)[0].type, "key");
  // shortcuts keep the physical code so the remote sees ctrl+c
  const ctrlC = classifyKeyDown(keyEv("KeyC", "c", { ctrlKey: true }), false);
  assert.equal(ctrlC[0].type, "key");
  assert.deepEqual(
    ctrlC[0].type === "key" ? ctrlC[0].modifiers : [],
    ["ctrl"]
  );
});

test("AltGr combinations (@ and euro) arrive as text even when reported as ctrl+alt", () => {
  assert.deepEqual(
    classifyKeyDown(keyEv("KeyQ", "@", { ctrlKey: true, altKey: true }), false),
    [{ type: "text", text: "@" }]
  );
  assert.deepEqual(
    classifyKeyDown(keyEv("KeyE", "€", { altKey: true, altGraph: true }), false),
    [{ type: "text", text: "€" }]
  );
  assert.deepEqual(
    classifyKeyDown(keyEv("Digit2", "@", { shiftKey: true }), false),
    [
      {
        type: "key",
        phase: "down",
        code: "Digit2",
        key: "@",
        location: 0,
        repeat: false,
        modifiers: ["shift"]
      }
    ],
    "shift+2 on a US layout is a plain key event"
  );
});

test("KeyForwarder: a text key sends no key up; an up never precedes a forwarded down", () => {
  const f = new KeyForwarder();
  assert.deepEqual(f.down(keyEv("Semicolon", "ş"), false), [
    { type: "text", text: "ş" }
  ]);
  assert.deepEqual(f.up(keyEv("Semicolon", "ş"), false), []);

  // key up for a key whose down was never forwarded is dropped
  assert.deepEqual(f.up(keyEv("ShiftLeft", "Shift"), false), []);

  const d = f.down(keyEv("KeyA", "a"), false);
  assert.equal(d.length, 1);
  const u = f.up(keyEv("KeyA", "a"), false);
  assert.equal(u.length, 1);
  assert.equal(u[0].type === "key" ? u[0].phase : "", "up");
  assert.deepEqual(f.up(keyEv("KeyA", "a"), false), [], "second up is dropped");

  f.down(keyEv("KeyB", "b"), false);
  f.reset();
  assert.deepEqual(f.up(keyEv("KeyB", "b"), false), []);
});

test("dead keys send no physical key (the composed text commits later); chords keep code + typed key", () => {
  assert.deepEqual(classifyKeyDown(keyEv("Quote", "Dead"), false), []);
  const f = new KeyForwarder();
  assert.deepEqual(f.down(keyEv("Quote", "Dead"), false), []);
  assert.deepEqual(f.up(keyEv("Quote", "Dead"), false), []);

  // Ctrl+Turkish-s on Semicolon: a key event with the physical code, the typed
  // key and the modifier (text cannot express a chord)
  const chord = classifyKeyDown(keyEv("Semicolon", "ş", { ctrlKey: true }), false);
  assert.deepEqual(chord, [
    {
      type: "key",
      phase: "down",
      code: "Semicolon",
      key: "ş",
      location: 0,
      repeat: false,
      modifiers: ["ctrl"]
    }
  ]);

  // auto-repeat: key events carry `repeat`, repeated characters repeat as text
  const rep = classifyKeyDown(keyEv("KeyA", "a", { repeat: true }), false);
  assert.equal(rep[0].type === "key" && rep[0].repeat, true);
  assert.deepEqual(classifyKeyDown(keyEv("Semicolon", "ş", { repeat: true }), false), [
    { type: "text", text: "ş" }
  ]);
});

test("object-fit: cover fills the box, crops the centre-aligned edges and still maps clicks exactly", () => {
  // 1000x800 box showing a 1920x1080 screen: scale = max(1000/1920, 800/1080) = 0.7407...
  // so the picture is 1422.2 x 800, centred horizontally (x = -211.1).
  const rect = computeImageRect(1000, 800, 1920, 1080, "cover");
  assert.ok(rect);
  assert.ok(Math.abs(rect.height - 800) < 1e-6);
  assert.ok(Math.abs(rect.width - (1920 * 800) / 1080) < 1e-6);
  assert.ok(Math.abs(rect.x - (1000 - rect.width) / 2) < 1e-6);
  assert.equal(rect.y, 0);
  // `contain` keeps the whole screen with bars above and below.
  const contained = computeImageRect(1000, 800, 1920, 1080, "contain");
  assert.ok(contained);
  assert.ok(Math.abs(contained.width - 1000) < 1e-6);
  assert.ok(contained.y > 0);

  const box = { left: 100, top: 50, width: 1000, height: 800 };
  // The middle of the box is the middle of the screen.
  const mid = mapClientPointToNormalized(600, 450, box, 1920, 1080, { fit: "cover" });
  assert.ok(mid);
  assert.ok(Math.abs(mid.x - 0.5) < 1e-6 && Math.abs(mid.y - 0.5) < 1e-6);
  // The left edge of the box is NOT the left edge of the screen: the crop is skipped.
  const left = mapClientPointToNormalized(100, 450, box, 1920, 1080, { fit: "cover" });
  assert.ok(left);
  assert.ok(Math.abs(left.x - (-rect.x / rect.width)) < 1e-6);
  assert.ok(left.x > 0.14 && left.x < 0.16, `left edge maps to ${left.x}`);
  // Top-left corner of the box: x cropped by 14.8 %, y is the very top.
  const corner = mapClientPointToNormalized(100, 50, box, 1920, 1080, { fit: "cover" });
  assert.ok(corner);
  assert.equal(corner.y, 0);
  // Same click under `contain` lands elsewhere (it would hit the top bar: rejected).
  assert.equal(mapClientPointToNormalized(100, 50, box, 1920, 1080, { fit: "contain" }), null);
  // A point outside the element is rejected without capture, clamped with it.
  assert.equal(mapClientPointToNormalized(2000, 450, box, 1920, 1080, { fit: "cover" }), null);
  assert.deepEqual(
    mapClientPointToNormalized(2000, 450, box, 1920, 1080, { fit: "cover", clamp: true })?.x,
    1
  );
  // Same shape: cover and contain agree.
  const same = { left: 0, top: 0, width: 960, height: 540 };
  assert.deepEqual(
    mapClientPointToNormalized(480, 135, same, 1920, 1080, { fit: "cover" }),
    mapClientPointToNormalized(480, 135, same, 1920, 1080, { fit: "contain" })
  );
  // Tall box (portrait stage): the sides are cropped much harder.
  const tall = mapClientPointToNormalized(50, 400, { left: 0, top: 0, width: 100, height: 800 }, 1920, 1080, { fit: "cover" });
  assert.ok(tall);
  assert.ok(Math.abs(tall.x - 0.5) < 1e-6);
  assert.equal(computeImageRect(0, 800, 1920, 1080, "cover"), null);
});
