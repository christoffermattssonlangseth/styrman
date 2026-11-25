// Pure parts of the Styrman push relay (no Workers APIs), so they can be tested in Node.

/** What a Mac may ask the relay to deliver. Everything else is rejected. */
export interface PushRequest {
  token: string; // APNs device token (hex)
  env: "sandbox" | "production";
  title: string;
  body: string;
  session?: string; // opens that session when the notification is tapped
  category?: string;
  collapse_id?: string;
  /** An approval's request id: the phone looks it up on the Mac itself (no command here). */
  request?: string;
}

export const LIMITS = {
  title: 120,
  body: 240,
  id: 64,
  perInstallPerHour: 200,
  registrationsPerIpPerHour: 10,
};

/** Validate a push request. Returns an error message, or the request. */
export function parsePush(v: unknown): PushRequest | string {
  if (typeof v !== "object" || v === null) return "expected a JSON object";
  const o = v as Record<string, unknown>;
  const allowed = new Set(["token", "env", "title", "body", "session", "category", "collapse_id", "request"]);
  for (const k of Object.keys(o)) if (!allowed.has(k)) return `unexpected field: ${k}`;
  if (typeof o.token !== "string" || !/^[0-9a-f]{32,200}$/i.test(o.token)) return "bad token";
  if (o.env !== "sandbox" && o.env !== "production") return "env must be sandbox or production";
  const str = (k: string, max: number, required: boolean): string | undefined | Error => {
    const x = o[k];
    if (x === undefined && !required) return undefined;
    if (typeof x !== "string" || x.length === 0 || x.length > max) return new Error(`${k}: 1-${max} characters`);
    return x;
  };
  const title = str("title", LIMITS.title, true);
  const body = str("body", LIMITS.body, true);
  const session = str("session", LIMITS.id, false);
  const category = str("category", LIMITS.id, false);
  const collapse = str("collapse_id", LIMITS.id, false);
  const request = str("request", LIMITS.id, false);
  for (const r of [title, body, session, category, collapse, request]) if (r instanceof Error) return r.message;
  return {
    token: o.token,
    env: o.env,
    title: title as string,
    body: body as string,
    session: session as string | undefined,
    category: category as string | undefined,
    collapse_id: collapse as string | undefined,
    request: request as string | undefined,
  };
}

/** The APNs payload for a request (same shape styrd sends directly). */
export function apnsPayload(p: PushRequest): Record<string, unknown> {
  const aps: Record<string, unknown> = { alert: { title: p.title, body: p.body }, sound: "default" };
  if (p.session) aps["thread-id"] = p.session;
  if (p.category) aps.category = p.category;
  if (p.request) aps["mutable-content"] = 1;
  const out: Record<string, unknown> = { aps };
  if (p.session) out.session = p.session;
  if (p.request) out.request = p.request;
  return out;
}

export function b64url(bytes: Uint8Array | string): string {
  const s = typeof bytes === "string" ? bytes : String.fromCharCode(...bytes);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** The DER body of a PEM (.p8) key. */
export function pemBody(pem: string): Uint8Array {
  const b64 = pem.replace(/-----[^-]+-----/g, "").replace(/\s+/g, "");
  const bin = atob(b64);
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}

export async function sha256Hex(s: string): Promise<string> {
  const d = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(s));
  return [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** Constant-time comparison of two hex strings. */
export function sameHex(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}

/** An APNs provider token: ES256 JWT signed with the team's .p8 key. */
export async function signApnsJwt(pem: string, keyId: string, teamId: string, now: number): Promise<string> {
  const key = await crypto.subtle.importKey("pkcs8", pemBody(pem), { name: "ECDSA", namedCurve: "P-256" }, false, [
    "sign",
  ]);
  const header = b64url(JSON.stringify({ alg: "ES256", kid: keyId }));
  const claims = b64url(JSON.stringify({ iss: teamId, iat: now }));
  const input = `${header}.${claims}`;
  // WebCrypto's ECDSA signature is raw r||s (64 bytes), which is what JWS ES256 wants.
  const sig = await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, key, new TextEncoder().encode(input));
  return `${input}.${b64url(new Uint8Array(sig))}`;
}
