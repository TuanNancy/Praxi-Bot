import assert from "node:assert/strict";
import { test } from "node:test";
import type { FillSecretInfo } from "../api/types.ts";
import { isValidSecretName } from "./secretDetect.ts";
import {
  filterVaultItems,
  normalizeOrigin,
  parseVaultCsv,
  pinnedBotIdentities,
  siteLabelOf,
  suggestSecretName,
} from "./vault.ts";

function item(over: Partial<FillSecretInfo>): FillSecretInfo {
  return {
    name: "login/github",
    kind: "login",
    site_name: null,
    login_id: null,
    note: null,
    origins: [],
    programs: [],
    bot_ids: [],
    ask_before_use: true,
    has_value: true,
    updated_at_ms: 0,
    ...over,
  };
}

test("normalizeOrigin keeps scheme/host/port and drops paths or wildcards", () => {
  assert.equal(normalizeOrigin("github.com/login"), "https://github.com");
  assert.equal(normalizeOrigin("http://localhost:8080/a?b=1"), "http://localhost:8080");
  assert.equal(normalizeOrigin("https://*.github.com"), null);
  assert.equal(normalizeOrigin("ftp://example.com"), null);
  assert.equal(normalizeOrigin("https://user@example.com"), null);
});

test("suggestSecretName produces valid unique names", () => {
  const first = suggestSecretName("GitHub.com", "login", []);
  assert.equal(first, "login/github.com");
  assert.ok(isValidSecretName(first));

  const second = suggestSecretName("GitHub.com", "login", ["login/github.com"]);
  assert.equal(second, "login/github.com-2");
  assert.ok(isValidSecretName(second));

  const other = suggestSecretName("My API Key!", "other", []);
  assert.equal(other, "secret/my-api-key");
  assert.ok(isValidSecretName(other));
});

test("filterVaultItems separates All / Logins / Other and searches fields", () => {
  const items = [
    item({ name: "login/github", kind: "login", site_name: "GitHub", login_id: "alice@ex.com", origins: ["https://github.com"] }),
    item({ name: "secret/stripe", kind: "other", site_name: "Stripe", note: "billing key" }),
  ];
  assert.equal(filterVaultItems(items, "all", "").length, 2);
  assert.equal(filterVaultItems(items, "login", "").length, 1);
  assert.equal(filterVaultItems(items, "other", "").length, 1);
  assert.equal(filterVaultItems(items, "all", "alice").length, 1);
  assert.equal(filterVaultItems(items, "all", "billing").length, 1);
  assert.equal(siteLabelOf(item({ name: "login/x", origins: ["https://www.notion.so"] })), "notion.so");
});

test("parseVaultCsv parses Chrome exports including quoted notes", () => {
  const csv = [
    "name,url,username,password,note",
    'GitHub,https://github.com/login,alice@example.com,"p@ss""word",work account',
    'EmptyPass,https://empty.test,bob,,skip me',
    '"Line, Note",http://localhost:3000,dev,dev-pass-1,"line1\nline2"',
  ].join("\n");
  const parsed = parseVaultCsv(csv);
  assert.equal(parsed.format, "chrome");
  assert.equal(parsed.skipped, 1);
  assert.equal(parsed.rows.length, 2);
  assert.deepEqual(parsed.rows[0], {
    siteName: "GitHub",
    origin: "https://github.com",
    loginId: "alice@example.com",
    password: 'p@ss"word',
    note: "work account",
  });
  assert.equal(parsed.rows[1].origin, "http://localhost:3000");
  assert.equal(parsed.rows[1].note, "line1\nline2");
});

test("parseVaultCsv parses Bitwarden exports and skips non-login entries", () => {
  const csv = [
    "folder,favorite,type,name,notes,fields,reprompt,login_uri,login_username,login_password,login_totp",
    'Work,0,login,Linear,"team login",,0,https://linear.app/signin,a@b.co,secret-123,',
    'Personal,0,note,Recovery Codes,"codes here",,0,,,,',
  ].join("\n");
  const parsed = parseVaultCsv(csv);
  assert.equal(parsed.format, "bitwarden");
  assert.equal(parsed.skipped, 1);
  assert.equal(parsed.rows.length, 1);
  assert.equal(parsed.rows[0].siteName, "Linear");
  assert.equal(parsed.rows[0].origin, "https://linear.app");
  assert.equal(parsed.rows[0].loginId, "a@b.co");
  assert.equal(parsed.rows[0].password, "secret-123");
});

test("pinnedBotIdentities prioritizes bot-bound and agentmail accounts", () => {
  const bots = [{ id: "b1", name: "Atlas" }];
  const accounts = [
    { name: "shared", provider: "imap_smtp" as const, address: "team@co.com", bot: null, password_set: true },
    { name: "atlas-mail", provider: "agentmail" as const, address: "atlas@agentmail.to", bot: "b1", password_set: true },
  ];
  const pinned = pinnedBotIdentities(accounts as never, bots);
  assert.equal(pinned[0].account.address, "atlas@agentmail.to");
  assert.equal(pinned[0].botName, "Atlas");
});
