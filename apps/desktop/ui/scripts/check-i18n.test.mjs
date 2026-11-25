// Strict locales must have every English key; scaffold locales (English copies being translated
// part by part) only warn about missing keys. Everything else stays an error for both.
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { checkLocales, readScaffoldLocales } from "./check-i18n.mjs";

const EN = { a: { one: "One", two: "Hello {{name}}" }, b: "Bee", list: ["x", "y"] };

// Writes en.json plus the given locale files (objects are stringified, strings are written as is).
const check = (locales, scaffold) => {
  const dir = mkdtempSync(join(tmpdir(), "check-i18n-"));
  try {
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "en.json"), JSON.stringify(EN));
    for (const [code, content] of Object.entries(locales)) {
      writeFileSync(join(dir, `${code}.json`), typeof content === "string" ? content : JSON.stringify(content));
    }
    return checkLocales({ localeDir: dir, scaffold });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
};

const lagging = { a: { one: "Satu" }, list: ["x", "y"] }; // missing a.two and b

test("a strict locale fails on missing keys", () => {
  const { errors } = check({ tr: lagging }, []);
  assert.deepEqual(errors, ["missing in tr: a.two", "missing in tr: b"]);
});

test("a scaffold locale only warns about the same missing keys", () => {
  const { errors, warnings } = check({ id: lagging }, ["id"]);
  assert.deepEqual(errors, []);
  assert.ok(warnings.some((w) => w.startsWith("id (scaffold): 2 keys missing")));
  assert.ok(warnings.includes("warn missing in id (scaffold): a.two"));
});

test("a scaffold locale still fails on extra keys, kind mismatches and placeholder mismatches", () => {
  const bad = { a: { one: "Satu", two: "Halo {{nama}}", extra: "?" }, b: { nested: "?" }, list: ["x", "y"] };
  const { errors } = check({ id: bad }, ["id"]);
  assert.deepEqual(errors.sort(), [
    "extra in id: a.extra",
    "extra in id: b.nested",
    "kind mismatch in id: b (expected string, got object)",
    "placeholders differ in id: a.two (English has {{name}}, id has {{nama}})",
  ]);
});

test("malformed JSON is an error for scaffold and strict locales alike", () => {
  for (const scaffold of [[], ["id"]]) {
    const { errors } = check({ id: '{"a": ' }, scaffold);
    assert.equal(errors.length, 1);
    assert.match(errors[0], /^malformed JSON in id\.json/);
  }
});

test("a complete locale passes; a locale leaving the scaffold list becomes strict", () => {
  const complete = { a: { one: "Satu", two: "Halo {{name}}" }, b: "Lebah", list: ["x", "y"] };
  assert.deepEqual(check({ id: complete }, ["id"]).errors, []);
  assert.deepEqual(check({ id: lagging }, ["id"]).errors, []);
  assert.equal(check({ id: lagging }, []).errors.length, 2);
});

test("the scaffold list cannot name en or a locale without a file", () => {
  const { errors } = check({ id: lagging }, ["en", "xx", "id"]);
  assert.deepEqual(errors.sort(), [
    "scaffold-locales.json: en is the reference and cannot be a scaffold",
    "scaffold-locales.json: xx has no file in the locales folder",
  ]);
});

test("the shipped scaffold list only names locales that exist and never the strict ones", () => {
  const scaffold = readScaffoldLocales();
  assert.ok(scaffold.length > 0);
  for (const strict of ["en", "tr", "de", "es"]) assert.ok(!scaffold.includes(strict), `${strict} must stay strict`);
});
