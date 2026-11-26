import { test } from "node:test";
import assert from "node:assert/strict";
import { apnsPayload, b64url, parsePush, pemBody, sameHex, sha256Hex, signApnsJwt } from "../src/lib.ts";

const ok = { token: "a".repeat(64), env: "sandbox", title: "Claude · styrman", body: "Needs your approval" };

test("valid requests pass, with optional ids", () => {
  const p = parsePush({ ...ok, session: "01ABC", collapse_id: "perm-1", category: "PERMISSION" });
  assert.equal(typeof p, "object");
  assert.deepEqual(apnsPayload(p as any), {
    aps: { alert: { title: ok.title, body: ok.body }, sound: "default", "thread-id": "01ABC", category: "PERMISSION", "mutable-content": 1 },
    session: "01ABC",
  });
});

test("an approval carries its request id, and wakes the phone's extension", () => {
  const p = parsePush({ ...ok, session: "01ABC", category: "PERMISSION", request: "r-1" });
  assert.deepEqual(apnsPayload(p as any), {
    aps: { alert: { title: ok.title, body: ok.body }, sound: "default", "thread-id": "01ABC", category: "PERMISSION", "mutable-content": 1 },
    session: "01ABC",
    request: "r-1",
  });
  assert.match(parsePush({ ...ok, request: "x".repeat(65) }) as string, /request/);
});

test("anything beyond a short alert is rejected (no session content through the relay)", () => {
  assert.equal(parsePush({ ...ok, extra: "code" }), "unexpected field: extra");
  assert.match(parsePush({ ...ok, body: "x".repeat(241) }) as string, /body/);
  assert.equal(parsePush({ ...ok, token: "not-hex" }), "bad token");
  assert.equal(parsePush({ ...ok, env: "prod" }), "env must be sandbox or production");
  assert.equal(parsePush("nope"), "expected a JSON object");
});

test("encoding helpers", async () => {
  assert.equal(b64url("hi?>"), "aGk_Pg");
  assert.deepEqual([...pemBody("-----BEGIN PRIVATE KEY-----\nAAEC\n-----END PRIVATE KEY-----\n")], [0, 1, 2]);
  assert.equal(await sha256Hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  assert.ok(sameHex("ab12", "ab12"));
  assert.ok(!sameHex("ab12", "ab13"));
  assert.ok(!sameHex("ab12", "ab1"));
});

test("APNs JWT: ES256 with a .p8-style (PKCS#8 PEM) P-256 key, verifiable", async () => {
  const pair = await crypto.subtle.generateKey({ name: "ECDSA", namedCurve: "P-256" }, true, ["sign", "verify"]);
  const der = new Uint8Array(await crypto.subtle.exportKey("pkcs8", pair.privateKey));
  const pem = `-----BEGIN PRIVATE KEY-----\n${btoa(String.fromCharCode(...der)).match(/.{1,64}/g)!.join("\n")}\n-----END PRIVATE KEY-----\n`;
  const jwt = await signApnsJwt(pem, "KEYID12345", "TEAMID1234", 1700000000);
  const [h, c, s] = jwt.split(".");
  const dec = (x: string) => JSON.parse(Buffer.from(x, "base64url").toString());
  assert.deepEqual(dec(h), { alg: "ES256", kid: "KEYID12345" });
  assert.deepEqual(dec(c), { iss: "TEAMID1234", iat: 1700000000 });
  const sig = Buffer.from(s, "base64url");
  assert.equal(sig.length, 64, "raw r||s");
  const valid = await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, pair.publicKey, sig, new TextEncoder().encode(`${h}.${c}`));
  assert.ok(valid);
});

test("Live Activity requests carry only counts and agent · folder · status", async () => {
  const { parseLive, livePayload } = await import("../src/lib.ts");
  const ok = {
    token: "b".repeat(64), env: "production", event: "update", priority: 10,
    state: { working: 1, needs: 1, rows: [{ agent: "Codex", repo: "lighthouse", status: "needs_approval", session: "01ABC", since: 786000000.5 }] },
  };
  const p = parseLive(ok);
  assert.equal(typeof p, "object");
  const out = livePayload(p as any, 100) as any;
  assert.equal(out.aps.event, "update");
  assert.equal(out.aps.alert.body, "Codex · lighthouse needs you");
  assert.deepEqual(out.aps["content-state"], ok.state);
  assert.equal((livePayload({ ...(p as any), event: "start" }, 1) as any).aps["attributes-type"], "StyrmanActivityAttributes");
  assert.equal(parseLive({ ...ok, state: { ...ok.state, activity: "rm -rf" } }), "unexpected state field: activity");
  const row = ok.state.rows[0];
  assert.equal(parseLive({ ...ok, state: { ...ok.state, rows: [{ ...row, status: "z" }] } }), "bad status");
  assert.equal(parseLive({ ...ok, state: { ...ok.state, rows: [{ ...row, title: "secret" }] } }), "unexpected row field: title");
  assert.equal(parseLive({ ...ok, state: { ...ok.state, rows: [{ ...row, line: "cargo test" }] } }), "unexpected row field: line");
});
