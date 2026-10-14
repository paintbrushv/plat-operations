// Unit tests for the browser workbench's pure sign-in decisions
// (boxscore/src/exact/workbench/app.js). Node's built-in runner, no
// dependencies:
//
//   node --test scripts/workbench_unit_test.mjs
//
// Under node app.js exports these helpers and wires nothing (it has no
// document); in a browser the export block is skipped. Synthetic values only.
import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const app = require("../boxscore/src/exact/workbench/app.js");
const { tokenState, tokenRecord, classifyCallback, authorizeParams, EXPIRED, MISMATCH, STALE, PENDING_MAX_MS } = app;

const NOW = 1_800_000_000_000;
const ISSUER = "https://idp.example.test/realms/acme";
const pending = (state, startedAt = NOW - 1000) => ({ verifier: "v".repeat(64), state, started_at: startedAt });
const query = (s) => new URLSearchParams(s);

test("the messages are the page's", () => {
  assert.equal(EXPIRED, "Your sign-in expired; sign in again.");
  assert.equal(MISMATCH, "Sign-in response did not match this tab; start again.");
  assert.equal(STALE, "The sign-in took too long; start again.");
  assert.equal(PENDING_MAX_MS, 600000);
});

test("a token is valid until expires_at and expired from then on", () => {
  const stored = { access_token: "synthetic-token", expires_at: NOW + 60000 };
  assert.equal(tokenState(stored, NOW), "valid");
  assert.equal(tokenState(stored, NOW + 59999), "valid");
  assert.equal(tokenState(stored, NOW + 60000), "expired");
  assert.equal(tokenState(stored, NOW + 3600000), "expired");
});

test("an expired marker stays expired; nothing stored is none", () => {
  assert.equal(tokenState({ expired: true }, NOW), "expired");
  assert.equal(tokenState(null, NOW), "none");
  assert.equal(tokenState({}, NOW), "none");
  assert.equal(tokenState({ access_token: 42 }, NOW), "none");
});

test("a token without expires_in has no client-side expiry", () => {
  const record = tokenRecord({ access_token: "synthetic-token", token_type: "Bearer" }, NOW);
  assert.deepEqual(record, { stored: { access_token: "synthetic-token", expires_at: null } });
  assert.equal(tokenState(record.stored, NOW + 365 * 86400000), "valid");
});

test("expires_in sets expires_at from now", () => {
  const record = tokenRecord({ access_token: "synthetic-token", token_type: "bearer", expires_in: 300 }, NOW);
  assert.equal(record.stored.expires_at, NOW + 300000);
  assert.equal(tokenState(record.stored, NOW + 299999), "valid");
  assert.equal(tokenState(record.stored, NOW + 300000), "expired");
  // Zero, negative or non-numeric lifetimes are ignored, not trusted.
  for (const life of [0, -5, "300"]) {
    const r = tokenRecord({ access_token: "t", token_type: "Bearer", expires_in: life }, NOW);
    assert.equal(r.stored.expires_at, null, String(life));
  }
});

test("an answer without a bearer access token is refused", () => {
  for (const body of [
    {},
    { access_token: "synthetic-token" },
    { access_token: "synthetic-token", token_type: "DPoP" },
    { access_token: "with space", token_type: "Bearer" },
    { access_token: 7, token_type: "Bearer" },
  ]) {
    const record = tokenRecord(body, NOW);
    assert.equal(record.stored, undefined, JSON.stringify(body));
    assert.match(record.problem, /no bearer access token/);
  }
});

test("a matching callback yields the code and verifier", () => {
  const answer = classifyCallback(query("code=synthetic-code&state=abc"), pending("abc"), ISSUER, NOW);
  assert.deepEqual(answer, { kind: "code", code: "synthetic-code", verifier: "v".repeat(64) });
});

test("a state that is not this tab's is a mismatch", () => {
  const answer = classifyCallback(query("code=c&state=other"), pending("abc"), ISSUER, NOW);
  assert.deepEqual(answer, { kind: "refused", message: MISMATCH });
});

test("a replay (no pending entry) is a mismatch", () => {
  assert.deepEqual(classifyCallback(query("code=c&state=abc"), null, ISSUER, NOW), { kind: "refused", message: MISMATCH });
  assert.deepEqual(classifyCallback(query("code=c&state=abc"), {}, ISSUER, NOW), { kind: "refused", message: MISMATCH });
});

test("a callback without state is a mismatch, code or error", () => {
  assert.deepEqual(classifyCallback(query("code=c"), pending("abc"), ISSUER, NOW), { kind: "refused", message: MISMATCH });
  assert.deepEqual(classifyCallback(query("error=access_denied"), pending("abc"), ISSUER, NOW), { kind: "refused", message: MISMATCH });
});

test("a pending entry older than ten minutes is stale", () => {
  const old = pending("abc", NOW - PENDING_MAX_MS - 1);
  assert.deepEqual(classifyCallback(query("code=c&state=abc"), old, ISSUER, NOW), { kind: "refused", message: STALE });
  const edge = pending("abc", NOW - PENDING_MAX_MS);
  assert.equal(classifyCallback(query("code=c&state=abc"), edge, ISSUER, NOW).kind, "code");
  const undated = { verifier: "v", state: "abc" };
  assert.deepEqual(classifyCallback(query("code=c&state=abc"), undated, ISSUER, NOW), { kind: "refused", message: STALE });
});

test("an iss from another issuer is a mismatch (RFC 9207)", () => {
  const other = query("code=c&state=abc&iss=" + encodeURIComponent("https://evil.example.test/realms/acme"));
  assert.deepEqual(classifyCallback(other, pending("abc"), ISSUER, NOW), { kind: "refused", message: MISMATCH });
  const same = query("code=c&state=abc&iss=" + encodeURIComponent(ISSUER));
  assert.equal(classifyCallback(same, pending("abc"), ISSUER, NOW).kind, "code");
  const slash = query("code=c&state=abc&iss=" + encodeURIComponent(ISSUER + "/"));
  assert.equal(classifyCallback(slash, pending("abc"), ISSUER, NOW).kind, "code");
  // Without iss the state check alone decides.
  assert.equal(classifyCallback(query("code=c&state=abc"), pending("abc"), ISSUER, NOW).kind, "code");
});

test("the identity provider's error is kept as text, markup and all", () => {
  const markup = "<img src=x onerror=alert(1)>";
  const q = new URLSearchParams();
  q.set("error", "access_denied");
  q.set("error_description", markup);
  q.set("state", "abc");
  const answer = classifyCallback(q, pending("abc"), ISSUER, NOW);
  assert.equal(answer.kind, "refused");
  assert.equal(answer.message, "The identity provider refused the sign-in: access_denied: " + markup);
});

test("the authorize request carries S256 and resource", () => {
  const auth = {
    client_id: "boxscore-workbench",
    redirect_uri: "https://boxscore.example.test/",
    scope: "openid boxscore:calc",
    audience: "https://boxscore.example.test",
  };
  const params = authorizeParams(auth, "synthetic-state", "synthetic-challenge");
  assert.deepEqual(params, [
    ["response_type", "code"],
    ["client_id", "boxscore-workbench"],
    ["redirect_uri", "https://boxscore.example.test/"],
    ["scope", "openid boxscore:calc"],
    ["state", "synthetic-state"],
    ["code_challenge", "synthetic-challenge"],
    ["code_challenge_method", "S256"],
    ["resource", "https://boxscore.example.test"],
  ]);
  // As the page applies them to auth.json's authorization_endpoint.
  const url = new URL("https://idp.example.test/realms/acme/protocol/openid-connect/auth");
  params.forEach((p) => url.searchParams.set(p[0], p[1]));
  assert.equal(url.searchParams.get("code_challenge_method"), "S256");
  assert.equal(url.searchParams.get("resource"), "https://boxscore.example.test");
  assert.equal(url.origin + url.pathname, "https://idp.example.test/realms/acme/protocol/openid-connect/auth");
});
