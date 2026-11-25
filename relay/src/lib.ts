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

/** A Live Activity update: counts and up to three agent · folder · status rows. */
export interface LiveRequest {
  token: string;
  env: "sandbox" | "production";
  event: "start" | "update" | "end";
  priority: 5 | 10;
  state: {
    working: number;
    needs: number;
    rows: { agent: string; repo: string; status: string; session: string; since: number }[];
  };
}

const STATUSES = new Set(["needs_approval", "waiting", "working", "error"]);

/** Validate a Live Activity request. Returns an error message, or the request. */
export function parseLive(v: unknown): LiveRequest | string {
  if (typeof v !== "object" || v === null) return "expected a JSON object";
  const o = v as Record<string, unknown>;
  const allowed = new Set(["token", "env", "event", "priority", "state"]);
  for (const k of Object.keys(o)) if (!allowed.has(k)) return `unexpected field: ${k}`;
  if (typeof o.token !== "string" || !/^[0-9a-f]{32,400}$/i.test(o.token)) return "bad token";
  if (o.env !== "sandbox" && o.env !== "production") return "env must be sandbox or production";
  if (o.event !== "start" && o.event !== "update" && o.event !== "end") return "bad event";
  if (o.priority !== 5 && o.priority !== 10) return "priority must be 5 or 10";
  const s = o.state as Record<string, unknown> | null;
  if (typeof s !== "object" || s === null) return "bad state";
  for (const k of Object.keys(s)) if (!["working", "needs", "rows"].includes(k)) return `unexpected state field: ${k}`;
  const count = (x: unknown) => Number.isInteger(x) && (x as number) >= 0 && (x as number) <= 999;
  if (!count(s.working) || !count(s.needs)) return "bad counts";
  if (!Array.isArray(s.rows) || s.rows.length > 3) return "bad rows";
  for (const r of s.rows as Record<string, unknown>[]) {
    if (typeof r !== "object" || r === null) return "bad row";
    // No "line": what an agent wants never goes through the relay.
    for (const k of Object.keys(r)) if (!["agent", "repo", "status", "session", "since"].includes(k)) return `unexpected row field: ${k}`;
    if (typeof r.session !== "string" || !/^[0-9A-Za-z_-]{1,64}$/.test(r.session)) return "bad session";
    if (typeof r.since !== "number" || !Number.isFinite(r.since)) return "bad since";
    if (typeof r.agent !== "string" || r.agent.length < 1 || r.agent.length > 20) return "bad agent";
    if (typeof r.repo !== "string" || r.repo.length < 1 || r.repo.length > 40) return "bad repo";
    if (typeof r.status !== "string" || !STATUSES.has(r.status)) return "bad status";
  }
  return o as unknown as LiveRequest;
}

/** The APNs payload for a Live Activity request (same shape styrd sends directly). */
export function livePayload(p: LiveRequest, nowSecs: number): Record<string, unknown> {
  const first = p.state.rows[0];
  const headline = !first
    ? "Your agents"
    : first.status === "working"
      ? `${first.agent} is working in ${first.repo}`
      : `${first.agent} · ${first.repo} needs you`;
  const aps: Record<string, unknown> = { timestamp: nowSecs, event: p.event, "content-state": p.state };
  if (p.event === "start") {
    aps["attributes-type"] = "StyrmanActivityAttributes";
    aps.attributes = {};
    aps.alert = { title: "Styrman", body: headline };
  } else if (p.event === "end") {
    aps["dismissal-date"] = nowSecs;
  } else if (p.state.needs > 0) {
    aps.alert = { title: "Styrman", body: headline };
  }
  return { aps };
}
