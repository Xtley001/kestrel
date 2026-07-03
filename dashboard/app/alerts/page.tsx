"use client";

import { useBotStore } from "@/store/botStore";
import { AlertTriangle, CheckCircle, Info, X, Trash2, Server } from "lucide-react";
import { clsx } from "clsx";
import { format } from "date-fns";

// ── NodeHealthPanel ───────────────────────────────────────────────────
// : node health indicator — per-chain block lag, WS status.
function NodeHealthPanel() {
  const metrics   = useBotStore((s) => s.metrics);
  const wsStatus  = useBotStore((s) => s.wsStatus);

  if (!metrics) {
    return (
      <div className="card flex items-center gap-3 text-sm text-text-muted">
        <Server size={16} />
        <span>Node health unavailable — bot offline</span>
      </div>
    );
  }

  const { chains, rate_cache_age_blocks, block_number } = metrics;

  return (
    <div className="card space-y-3">
      <div className="flex items-center gap-2">
        <Server size={14} className="text-text-secondary" />
        <p className="text-sm font-medium text-text-secondary uppercase tracking-wider">
          Node health
        </p>
      </div>

      {/* WS connection status */}
      <div className="flex items-center justify-between text-xs">
        <span className="text-text-muted">Bot WebSocket</span>
        <span className={clsx(
          "font-mono uppercase tracking-wider px-1.5 py-0.5 rounded text-[9px]",
          wsStatus === "connected"     && "bg-accent-green/10 text-accent-green",
          wsStatus === "connecting"    && "bg-accent-amber/10 text-accent-amber",
          wsStatus === "disconnected"  && "bg-accent-red/10 text-accent-red",
          wsStatus === "error"         && "bg-accent-red/10 text-accent-red",
        )}>
          {wsStatus}
        </span>
      </div>

      {/* Per-chain status */}
      {chains.map((c) => {
        // Lag is estimated as the gap between the chain's reported block and the
        // latest block number from the ETH chain (as a rough reference).
        const lag = block_number - c.latest_block;
        const lagging = lag > 2;
        return (
          <div key={c.chain} className="flex items-center justify-between text-xs">
            <div className="flex items-center gap-2">
              <span className={clsx(
                "w-1.5 h-1.5 rounded-full",
                c.active ? "bg-accent-green" : "bg-text-muted"
              )} />
              <span className="text-text-secondary font-mono">{c.chain}</span>
            </div>
            <div className="flex items-center gap-3">
              <span className="text-text-muted font-mono">
                block {c.latest_block.toLocaleString()}
              </span>
              {lagging ? (
                <span className="text-accent-amber text-[9px] uppercase tracking-wider">
                  +{lag} lag
                </span>
              ) : (
                <span className="text-accent-green text-[9px] uppercase tracking-wider">
                  synced
                </span>
              )}
            </div>
          </div>
        );
      })}

      {/* Rate cache age */}
      <div className="flex items-center justify-between text-xs border-t border-bg-border pt-2">
        <span className="text-text-muted">Protocol rate cache age</span>
        <span className={clsx(
          "font-mono",
          rate_cache_age_blocks > 1 ? "text-accent-amber" : "text-accent-green"
        )}>
          {rate_cache_age_blocks} block{rate_cache_age_blocks !== 1 ? "s" : ""}
          {rate_cache_age_blocks > 1 && " ⚠"}
        </span>
      </div>
    </div>
  );
}

export default function AlertsPage() {
  const alerts      = useBotStore((s) => s.alerts);
  const acknowledge = useBotStore((s) => s.acknowledgeAlert);
  const clear       = useBotStore((s) => s.clearAlerts);
  const unread      = useBotStore((s) => s.unacknowledgedAlerts);

  return (
    <div className="space-y-6 animate-fade-in max-w-3xl">
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold text-text-primary">Alerts</h1>
          <p className="text-sm text-text-muted mt-0.5">
            {unread > 0 ? `${unread} unacknowledged` : "All caught up"}
          </p>
        </div>
        {alerts.length > 0 && (
          <button
            onClick={clear}
            className="flex items-center gap-1.5 px-3 py-1.5 text-xs text-text-muted hover:text-text-secondary border border-bg-border rounded-lg transition-colors"
          >
            <Trash2 size={12} />
            Clear all
          </button>
        )}
      </div>

      {/* Node health indicator */}
      <NodeHealthPanel />

      {/* Alert log */}
      {alerts.length === 0 ? (
        <div className="card text-center py-12 text-text-muted">
          <CheckCircle size={32} className="mx-auto mb-3 text-accent-green/40" />
          <p className="text-sm">No alerts</p>
        </div>
      ) : (
        <div className="space-y-2">
          {alerts.map((alert) => (
            <div
              key={alert.id}
              className={clsx(
                "card flex items-start gap-3 transition-opacity",
                alert.acknowledged && "opacity-50"
              )}
            >
              <div className="shrink-0 mt-0.5">
                {alert.severity === "red"   && <AlertTriangle size={16} className="text-accent-red" />}
                {alert.severity === "amber" && <AlertTriangle size={16} className="text-accent-amber" />}
                {alert.severity === "green" && <CheckCircle   size={16} className="text-accent-green" />}
              </div>

              <div className="flex-1 min-w-0 space-y-1">
                <div className="flex items-center gap-2">
                  <span className={clsx(
                    "text-[10px] uppercase tracking-wider px-1.5 py-0.5 rounded",
                    alert.severity === "red"   && "bg-accent-red/10 text-accent-red",
                    alert.severity === "amber" && "bg-accent-amber/10 text-accent-amber",
                    alert.severity === "green" && "bg-accent-green/10 text-accent-green",
                  )}>
                    {alert.severity}
                  </span>
                  <span className="text-xs text-text-muted font-mono">{alert.chain}</span>
                  <span className="text-xs text-text-muted ml-auto">
                    {format(new Date(alert.timestamp_ms), "HH:mm:ss")}
                  </span>
                </div>
                <p className="text-sm text-text-primary leading-snug">{alert.message}</p>
              </div>

              {!alert.acknowledged && (
                <button
                  onClick={() => acknowledge(alert.id)}
                  className="shrink-0 text-text-muted hover:text-text-secondary transition-colors mt-0.5"
                >
                  <X size={14} />
                </button>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
