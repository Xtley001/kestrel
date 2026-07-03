// app/api/control/route.ts
// Server-side relay for control commands to the bot control WebSocket (127.0.0.1:9102).
//
// Security model:
//   - HTTP-layer API key check before any command is forwarded.
//     Even though the bot does Ed25519 signature verification, an unauthenticated
//     HTTP endpoint allows automated probing with no backstop if the signing key leaks.
//   - In-memory rate limiting (10 requests/min per IP).
//   - The client sends a signed command (Ed25519).
//   - This server-side relay forwards it verbatim to the bot.
//   - The BOT performs full Ed25519 signature verification and rejects bad signatures.
//   - WS_CONTROL_URL is a server-only env var — never exposed to client bundles.
//   - BOT_SIGNING_KEY is NEVER used here; the client pre-signs its own commands.

import { NextRequest, NextResponse } from "next/server";
import { WebSocket } from "ws";

// Server-only: the bot control WebSocket URL.
// Defaults to loopback — never overridden to an external host.
const BOT_CONTROL_URL =
  process.env.WS_CONTROL_URL ?? "ws://127.0.0.1:9102";

/** Maximum command relay wait in milliseconds before timing out. */
const RELAY_TIMEOUT_MS = 5_000;

// ── In-memory rate limiter ────────────────────────────────────
// Simple sliding-window counter per IP. For production at scale, replace with
// Upstash Redis or next-rate-limit backed by a persistent store.
const RATE_WINDOW_MS = 60_000;
const RATE_MAX_REQUESTS = 10;
const ipHitMap = new Map<string, number[]>();

function isRateLimited(ip: string): boolean {
  const now = Date.now();
  const hits = (ipHitMap.get(ip) ?? []).filter((t) => now - t < RATE_WINDOW_MS);
  if (hits.length >= RATE_MAX_REQUESTS) return true;
  hits.push(now);
  ipHitMap.set(ip, hits);
  return false;
}

// ── Validate DASHBOARD_API_KEY at module load time ────────────
// Fail loudly if the operator forgot to set it — don't silently run unprotected.
const DASHBOARD_API_KEY = process.env.DASHBOARD_API_KEY;
if (!DASHBOARD_API_KEY || DASHBOARD_API_KEY.length < 32) {
  console.error(
    "STARTUP ERROR: DASHBOARD_API_KEY is not set or is shorter than 32 characters. " +
    "The /api/control route will reject all requests until this is configured."
  );
}

export async function POST(req: NextRequest): Promise<NextResponse> {
  // ── Rate limit check ────────────────────────────────────────
  const ip =
    req.headers.get("x-forwarded-for")?.split(",")[0]?.trim() ??
    req.headers.get("x-real-ip") ??
    "unknown";

  if (isRateLimited(ip)) {
    return NextResponse.json(
      { error: "Too many requests — try again in a minute" },
      { status: 429 }
    );
  }

  // ── API key authentication ───────────────────────────────────
  // x-api-key must match DASHBOARD_API_KEY exactly.
  // This provides an HTTP-layer backstop independent of Ed25519 verification in the bot.
  const providedKey = req.headers.get("x-api-key");
  if (!DASHBOARD_API_KEY || providedKey !== DASHBOARD_API_KEY) {
    return NextResponse.json({ error: "Unauthorized" }, { status: 401 });
  }

  // ── Parse body ────────────────────────────────────────────────────────────
  let body: unknown;
  try {
    body = await req.json();
  } catch {
    return NextResponse.json({ error: "Invalid JSON" }, { status: 400 });
  }

  // ── Schema check — bot will do full signature verification ────────────────
  const cmd = body as Record<string, unknown>;
  if (!cmd.command || !cmd.signature || !cmd.pubkey) {
    return NextResponse.json(
      { error: "Missing required fields: command, signature, pubkey" },
      { status: 400 }
    );
  }

  const commandStr = typeof cmd.command === "string" ? cmd.command : String(cmd.command);

  // ── Forward to bot via WebSocket ──────────────────────────────────────────
  // Open a short-lived WS connection, send the signed command, then close.
  // The bot will echo back the updated config after verifying the signature.
  const relayResult = await new Promise<{ ok: boolean; echo?: unknown; error?: string }>(
    (resolve) => {
      let settled = false;
      const settle = (result: { ok: boolean; echo?: unknown; error?: string }) => {
        if (!settled) {
          settled = true;
          resolve(result);
        }
      };

      const timeout = setTimeout(() => {
        settle({ ok: false, error: "Bot WS relay timeout" });
        try { ws.close(); } catch {}
      }, RELAY_TIMEOUT_MS);

      let ws: WebSocket;
      try {
        ws = new WebSocket(BOT_CONTROL_URL);
      } catch (e: unknown) {
        clearTimeout(timeout);
        settle({ ok: false, error: `WS connect error: ${e}` });
        return;
      }

      ws.on("open", () => {
        try {
          ws.send(JSON.stringify(cmd));
        } catch (e: unknown) {
          clearTimeout(timeout);
          settle({ ok: false, error: `WS send error: ${e}` });
          ws.close();
        }
      });

      ws.on("message", (data: Buffer) => {
        clearTimeout(timeout);
        try {
          const echo = JSON.parse(data.toString());
          settle({ ok: true, echo });
        } catch {
          settle({ ok: true, echo: data.toString() });
        }
        ws.close();
      });

      ws.on("error", (err: Error) => {
        clearTimeout(timeout);
        settle({ ok: false, error: `WS error: ${err.message}` });
      });

      ws.on("close", () => {
        clearTimeout(timeout);
        settle({ ok: true });
      });
    }
  );

  if (!relayResult.ok) {
    return NextResponse.json(
      { error: relayResult.error ?? "Relay failed", relayed: false },
      { status: 502 }
    );
  }

  return NextResponse.json(
    { relayed: true, command: commandStr, echo: relayResult.echo ?? null },
    { status: 200 }
  );
}
