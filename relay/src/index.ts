// Styrman push relay (Cloudflare Worker).
//
// Lets people who install Styrman get iPhone notifications without their own Apple
// developer key: their Mac asks this relay to deliver a short alert ("Claude · repo",
// "Needs your approval") to their phone's APNs token. The relay holds the publisher's
// APNs key as a secret. It never sees session content: requests are limited to a title,
// a short body and ids, and anything else is rejected.
//
//   POST /v1/register  → {install_id, secret}      (a Mac registers once)
//   POST /v1/push      Authorization: Bearer <install_id>.<secret>, body: PushRequest
//   POST /v1/live      same auth, body: LiveRequest (Live Activity: counts and
//                      agent · folder · status rows, nothing else)
//   GET  /v1/health

import { LIMITS, apnsPayload, b64url, livePayload, parseLive, parsePush, sameHex, sha256Hex, signApnsJwt } from "./lib";

export interface Env {
  INSTALLS: KVNamespace; // install_id → sha256(secret); also rate-limit counters
  APNS_KEY: string; // contents of AuthKey_XXXX.p8 (secret)
  APNS_KEY_ID: string;
  APNS_TEAM_ID: string;
  APNS_TOPIC: string; // the app's bundle id
}

const json = (v: unknown, status = 200) =>
  new Response(JSON.stringify(v), { status, headers: { "content-type": "application/json" } });

let jwtCache: { token: string; at: number } | undefined;

async function apnsJwt(env: Env): Promise<string> {
  const now = Math.floor(Date.now() / 1000);
  if (jwtCache && now - jwtCache.at < 40 * 60) return jwtCache.token;
  const token = await signApnsJwt(env.APNS_KEY, env.APNS_KEY_ID, env.APNS_TEAM_ID, now);
  jwtCache = { token, at: now };
  return token;
}

/** A fixed-window counter in KV (approximate; good enough to stop abuse). */
async function overLimit(env: Env, key: string, limit: number): Promise<boolean> {
  const bucket = `rl:${key}:${Math.floor(Date.now() / 3_600_000)}`;
  const n = parseInt((await env.INSTALLS.get(bucket)) ?? "0", 10);
  if (n >= limit) return true;
  await env.INSTALLS.put(bucket, String(n + 1), { expirationTtl: 7200 });
  return false;
}

async function register(req: Request, env: Env): Promise<Response> {
  const ip = req.headers.get("cf-connecting-ip") ?? "unknown";
  if (await overLimit(env, `reg:${ip}`, LIMITS.registrationsPerIpPerHour)) return json({ error: "slow down" }, 429);
  const rand = (n: number) => b64url(crypto.getRandomValues(new Uint8Array(n)));
  const id = rand(12);
  const secret = rand(32);
  await env.INSTALLS.put(`install:${id}`, await sha256Hex(secret));
  return json({ install_id: id, secret });
}

/** The Mac's install id, or an error response (bad credentials, or over its hourly limit). */
async function authorize(req: Request, env: Env): Promise<string | Response> {
  const auth = req.headers.get("authorization") ?? "";
  const m = /^Bearer ([A-Za-z0-9_-]+)\.([A-Za-z0-9_-]+)$/.exec(auth);
  if (!m) return json({ error: "unauthorized" }, 401);
  const [, id, secret] = m;
  const stored = await env.INSTALLS.get(`install:${id}`);
  if (!stored || !sameHex(stored, await sha256Hex(secret))) return json({ error: "unauthorized" }, 401);
  if (await overLimit(env, `push:${id}`, LIMITS.perInstallPerHour)) return json({ error: "slow down" }, 429);
  return id;
}

/** Send to APNs and turn its answer into the relay's result. */
async function deliver(token: string, apnsEnv: string, headers: Record<string, string>, payload: unknown): Promise<Response> {
  const host = apnsEnv === "production" ? "api.push.apple.com" : "api.sandbox.push.apple.com";
  const r = await fetch(`https://${host}/3/device/${token}`, { method: "POST", headers, body: JSON.stringify(payload) });
  if (r.ok) return json({ result: "sent" });
  const reason = ((await r.json().catch(() => ({}))) as { reason?: string }).reason ?? `HTTP ${r.status}`;
  // The phone uninstalled the app, or it's a sandbox/production mismatch.
  if (r.status === 410 || reason === "BadDeviceToken" || reason === "Unregistered") {
    return json({ result: "bad_token", reason });
  }
  return json({ result: "failed", reason }, 502);
}

async function live(req: Request, env: Env): Promise<Response> {
  const id = await authorize(req, env);
  if (id instanceof Response) return id;
  let body: unknown;
  try {
    body = await req.json();
  } catch {
    return json({ error: "bad json" }, 400);
  }
  const p = parseLive(body);
  if (typeof p === "string") return json({ error: p }, 400);
  return deliver(p.token, p.env, {
    authorization: `bearer ${await apnsJwt(env)}`,
    "apns-topic": `${env.APNS_TOPIC}.push-type.liveactivity`,
    "apns-push-type": "liveactivity",
    "apns-priority": String(p.priority),
    "content-type": "application/json",
  }, livePayload(p, Math.floor(Date.now() / 1000)));
}

async function push(req: Request, env: Env): Promise<Response> {
  const id = await authorize(req, env);
  if (id instanceof Response) return id;

  let body: unknown;
  try {
    body = await req.json();
  } catch {
    return json({ error: "bad json" }, 400);
  }
  const p = parsePush(body);
  if (typeof p === "string") return json({ error: p }, 400);

  const headers: Record<string, string> = {
    authorization: `bearer ${await apnsJwt(env)}`,
    "apns-topic": env.APNS_TOPIC,
    "apns-push-type": "alert",
    "apns-priority": "10",
    "content-type": "application/json",
  };
  if (p.collapse_id) headers["apns-collapse-id"] = p.collapse_id;
  return deliver(p.token, p.env, headers, apnsPayload(p));
}

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const { pathname } = new URL(req.url);
    if (req.method === "GET" && pathname === "/v1/health") return json({ ok: true });
    if (req.method === "POST" && pathname === "/v1/register") return register(req, env);
    if (req.method === "POST" && pathname === "/v1/push") return push(req, env);
    if (req.method === "POST" && pathname === "/v1/live") return live(req, env);
    return json({ error: "not found" }, 404);
  },
};
