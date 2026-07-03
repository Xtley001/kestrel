"use client";

// components/WsProvider.tsx
// Connects to bot WebSocket metrics endpoint on ws://127.0.0.1:9101
// (via WireGuard tunnel — never exposed publicly).
// Feeds all incoming messages into the Zustand botStore.
// Reconnects automatically with exponential backoff.
//
// ── / note ─────────────────────────────────────────────
// labels WS_METRICS_URL as "server only," but the intent is that the
// *port* (9101) is never exposed in a public firewall rule — not that the
// browser cannot know the URL.  The browser opens the WebSocket directly to
// ws://127.0.0.1:9101 through the WireGuard tunnel; there is no server-side
// proxy for streaming connections.  NEXT_PUBLIC_WS_METRICS_URL is therefore
// correctly a NEXT_PUBLIC_ variable: it must be visible to the browser bundle.
//
// WS_CONTROL_URL IS server-only (see api/control/route.ts) because control
// commands flow through the Next.js API route, not directly from the browser.
// That URL is never in any NEXT_PUBLIC_ variable.

import { useEffect, useRef } from "react";
import { useBotStore } from "@/store/botStore";

const WS_URL =
  process.env.NEXT_PUBLIC_WS_METRICS_URL ?? "ws://127.0.0.1:9101";

const INITIAL_RECONNECT_MS = 1_000;
const MAX_RECONNECT_MS = 30_000;

export function WsProvider() {
  const setMetrics   = useBotStore((s) => s.setMetrics);
  const addAlert     = useBotStore((s) => s.addAlert);
  const setWsStatus  = useBotStore((s) => s.setWsStatus);

  const wsRef        = useRef<WebSocket | null>(null);
  const retryDelay   = useRef(INITIAL_RECONNECT_MS);
  const retryTimer   = useRef<ReturnType<typeof setTimeout> | null>(null);
  const unmounted    = useRef(false);

  useEffect(() => {
    connect();
    return () => {
      unmounted.current = true;
      if (retryTimer.current) clearTimeout(retryTimer.current);
      wsRef.current?.close();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function connect() {
    if (unmounted.current) return;
    setWsStatus("connecting");

    const ws = new WebSocket(WS_URL);
    wsRef.current = ws;

    ws.onopen = () => {
      retryDelay.current = INITIAL_RECONNECT_MS;
      setWsStatus("connected");
    };

    ws.onmessage = (event) => {
      try {
        const msg = JSON.parse(event.data as string);

        if (msg.type === "block_metrics") {
          setMetrics(msg);
        } else if (msg.type === "alert") {
          addAlert({
            severity: msg.severity,
            chain:    msg.chain,
            message:  msg.message,
            timestamp_ms: msg.timestamp_ms,
          });
        }
      } catch {
        // Malformed message — discard silently
      }
    };

    ws.onerror = () => {
      setWsStatus("error");
    };

    ws.onclose = () => {
      if (unmounted.current) return;
      setWsStatus("disconnected");
      scheduleReconnect();
    };
  }

  function scheduleReconnect() {
    const delay = Math.min(retryDelay.current, MAX_RECONNECT_MS);
    retryDelay.current = Math.min(delay * 2, MAX_RECONNECT_MS);
    retryTimer.current = setTimeout(() => {
      if (!unmounted.current) connect();
    }, delay);
  }

  // This component renders nothing — it's a side-effect only provider
  return null;
}
