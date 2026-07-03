// app/api/metrics/route.ts
// Server-side proxy for Prometheus metrics endpoint.
// Fetches from bot's 127.0.0.1:9090/metrics — never exposed to browser directly.
// Dashboard polls this endpoint for Prometheus-format data if needed.

import { NextResponse } from "next/server";

const PROMETHEUS_URL =
  process.env.PROMETHEUS_URL ?? "http://127.0.0.1:9090/metrics";

export async function GET() {
  try {
    const res = await fetch(PROMETHEUS_URL, {
      // No caching — always fresh metrics
      cache: "no-store",
      signal: AbortSignal.timeout(5_000),
    });

    if (!res.ok) {
      return NextResponse.json(
        { error: `Prometheus returned ${res.status}` },
        { status: 502 }
      );
    }

    const text = await res.text();
    return new NextResponse(text, {
      status: 200,
      headers: {
        "Content-Type": "text/plain; version=0.0.4",
        "Cache-Control": "no-store",
      },
    });
  } catch (err: any) {
    return NextResponse.json(
      { error: "Failed to reach Prometheus endpoint", detail: err.message },
      { status: 503 }
    );
  }
}
