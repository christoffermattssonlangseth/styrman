import { test } from "node:test";
import assert from "node:assert/strict";
import { apnsPayload, b64url, parsePush, pemBody, sameHex, sha256Hex, signApnsJwt } from "../src/lib.ts";

const ok = { token: "a".repeat(64), env: "sandbox", title: "Claude · styrman", body: "Needs your approval" };

test("valid requests pass, with optional ids", () => {
  const p = parsePush({ ...ok, session: "01ABC", collapse_id: "perm-1", category: "PERMISSION" });
  assert.equal(typeof p, "object");
  assert.deepEqual(apnsPayload(p as any), {
    aps: { alert: { title: ok.title, body: ok.body }, sound: "default", "thread-id": "01ABC", category: "PERMISSION" },
    session: "01ABC",
  });
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
