"use client";

import { PipelineLatency } from "@/store/botStore";
import { clsx } from "clsx";

interface LatencyBarProps {
  latency: PipelineLatency;
}

const STAGES: { key: keyof PipelineLatency; label: string; target: number }[] = [
  { key: "ipc_recv_ms",     label: "IPC recv",      target: 1 },
  { key: "pool_update_ms",  label: "Pool update",   target: 2 },
  { key: "rate_cache_ms",   label: "Rate cache",    target: 0.5 },
  { key: "binary_search_ms",label: "Binary search", target: 5 },
  { key: "revm_sim_ms",     label: "REVM sim",      target: 10 },
  { key: "bundle_sign_ms",  label: "Bundle sign",   target: 1 },
  { key: "submit_all_ms",   label: "Submit all",    target: 15 },
];

export function LatencyBar({ latency }: LatencyBarProps) {
  const total = STAGES.reduce((sum, s) => sum + latency[s.key], 0);

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between">
        <p className="stat-label">Pipeline latency</p>
        <p className="font-mono text-xs text-text-secondary">{total.toFixed(1)} ms total</p>
      </div>

      {/* Stacked bar */}
      <div className="h-2 rounded-full overflow-hidden flex gap-px bg-bg-raised">
        {STAGES.map((stage) => {
          const pct = total > 0 ? (latency[stage.key] / total) * 100 : 0;
          const over = latency[stage.key] > stage.target * 2;
          return (
            <div
              key={stage.key}
              title={`${stage.label}: ${latency[stage.key].toFixed(2)}ms`}
              style={{ width: `${pct}%` }}
              className={clsx(
                "h-full transition-all",
                over ? "bg-accent-amber" : "bg-accent-green/60"
              )}
            />
          );
        })}
      </div>

      {/* Stage breakdown */}
      <div className="grid grid-cols-2 gap-x-6 gap-y-1">
        {STAGES.map((stage) => {
          const val = latency[stage.key];
          const over = val > stage.target * 2;
          return (
            <div key={stage.key} className="flex items-center justify-between text-xs">
              <span className="text-text-muted">{stage.label}</span>
              <span className={clsx("font-mono", over ? "text-accent-amber" : "text-text-secondary")}>
                {val.toFixed(1)}ms
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
