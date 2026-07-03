"use client";

// app/execution/page.tsx
// / Execution pipeline detail view.
// Added: recent bundle log table, flash provider breakdown bars.
// — all values from live WS stream only.

import { useBotStore, BundleLogEntry, FlashProviderBreakdown } from "@/store/botStore";
import { BuilderTable } from "@/components/BuilderTable";
import { LatencyBar } from "@/components/LatencyBar";
import { StatCard } from "@/components/StatCard";
import { clsx } from "clsx";
import { format } from "date-fns";

// ── helpers ──────────────────────────────────────────────────────────────────

function fmt18(raw: string): string {
  const n = parseFloat(raw) / 1e18;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000)     return `${(n / 1_000).toFixed(1)}K`;
  return n.toFixed(2);
}

// ── FlashBreakdownBars ────────────────────────────────────────────────────────
// Flash provider usage distribution.
// Colours : green=Balancer, amber=Aave, red=Morpho.

function FlashBreakdownBars({ breakdown }: { breakdown: FlashProviderBreakdown }) {
  const total = breakdown.balancer + breakdown.aave + breakdown.morpho;
  if (total === 0) {
    return (
      <p className="text-xs text-text-muted">No bundles submitted yet</p>
    );
  }
  const pct = (n: number) => ((n / total) * 100).toFixed(1);

  const rows: { label: string; count: number; color: string }[] = [
    { label: "Balancer (0% fee)",     count: breakdown.balancer, color: "bg-accent-green" },
    { label: "Aave (0.05% fee)",      count: breakdown.aave,     color: "bg-accent-amber" },
    { label: "Morpho (variable fee)", count: breakdown.morpho,   color: "bg-accent-red"   },
  ];

  return (
    <div className="space-y-3">
      {rows.map((r) => (
        <div key={r.label} className="space-y-1">
          <div className="flex items-center justify-between text-xs">
            <span className="text-text-secondary">{r.label}</span>
            <span className="font-mono text-text-primary">
              {r.count} ({pct(r.count)}%)
            </span>
          </div>
          <div className="h-1 w-full bg-bg-border rounded-full overflow-hidden">
            <div
              className={clsx("h-full rounded-full transition-all", r.color)}
              style={{ width: `${pct(r.count)}%` }}
            />
          </div>
        </div>
      ))}
    </div>
  );
}

// ── BundleLogTable ────────────────────────────────────────────────────────────
// Recent bundle log 
// Shows: block, flash size, profit, flash provider, builder target, landed.

function BundleLogTable({ entries }: { entries: BundleLogEntry[] }) {
  if (entries.length === 0) {
    return (
      <p className="text-xs text-text-muted py-4 text-center">
        No bundles submitted this session
      </p>
    );
  }

  return (
    <div className="overflow-x-auto">
      <table className="w-full text-xs font-mono">
        <thead>
          <tr className="text-text-muted border-b border-bg-border">
            <th className="text-left pb-2 pr-3">Block</th>
            <th className="text-right pb-2 pr-3">Flash size</th>
            <th className="text-right pb-2 pr-3">Profit</th>
            <th className="text-left pb-2 pr-3">Provider</th>
            <th className="text-left pb-2 pr-3">Builders</th>
            <th className="text-center pb-2">Status</th>
          </tr>
        </thead>
        <tbody>
          {entries.map((e, i) => (
            <tr
              key={i}
              className="border-b border-white/[0.03] hover:bg-bg-raised transition-colors"
            >
              <td className="py-1.5 pr-3 text-text-secondary">
                #{e.block.toLocaleString()}
              </td>
              <td className="py-1.5 pr-3 text-right text-text-primary">
                {fmt18(e.flash_size_usds)}
              </td>
              <td className="py-1.5 pr-3 text-right text-accent-green">
                +{fmt18(e.profit_usds)}
              </td>
              <td className="py-1.5 pr-3 text-text-secondary">
                {e.flash_provider}
              </td>
              <td className="py-1.5 pr-3 text-text-muted">
                {e.builders.join(", ")}
              </td>
              <td className="py-1.5 text-center">
                {e.reverted ? (
                  <span className="px-1.5 py-0.5 rounded bg-accent-red/10 text-accent-red text-[9px] uppercase tracking-wider">
                    Reverted
                  </span>
                ) : e.landed ? (
                  <span className="px-1.5 py-0.5 rounded bg-accent-green/10 text-accent-green text-[9px] uppercase tracking-wider">
                    Landed
                  </span>
                ) : (
                  <span className="px-1.5 py-0.5 rounded bg-bg-border text-text-muted text-[9px] uppercase tracking-wider">
                    Missed
                  </span>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

// ── Page ─────────────────────────────────────────────────────────────────────

export default function ExecutionPage() {
  const metrics = useBotStore((s) => s.metrics);

  if (!metrics) {
    return (
      <div className="flex items-center justify-center h-64 text-text-muted text-sm">
        Awaiting bot data…
      </div>
    );
  }

  const {
    bundles_submitted,
    bundles_landed,
    bundles_reverted,
    gas_spent_usd,
    latency,
    whale_detections,
    recent_bundles,
    flash_provider_breakdown,
  } = metrics;

  const landingRate =
    bundles_submitted > 0
      ? ((bundles_landed / bundles_submitted) * 100).toFixed(1)
      : "—";

  const totalPipelineMs = Object.values(latency).reduce((a, b) => a + b, 0);

  // Fallback breakdown when bot hasn't sent it yet
  const breakdown: FlashProviderBreakdown = flash_provider_breakdown ?? {
    balancer: 0,
    aave: 0,
    morpho: 0,
  };

  const bundleLog: BundleLogEntry[] = recent_bundles ?? [];

  return (
    <div className="space-y-6 animate-fade-in">
      <div>
        <h1 className="text-xl font-semibold text-text-primary">Execution</h1>
        <p className="text-sm text-text-muted mt-0.5">
          Bundle submission, builder performance, and pipeline detail
        </p>
      </div>

      {/* Top stats */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
        <StatCard label="Submitted"    value={bundles_submitted.toString()} mono />
        <StatCard label="Landed"       value={bundles_landed.toString()} trend="up" mono />
        <StatCard label="Reverted"     value={bundles_reverted.toString()} trend={bundles_reverted > 0 ? "down" : "neutral"} mono />
        <StatCard label="Landing rate" value={`${landingRate}%`} />
      </div>

      {/* Builder performance + Latency */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <div className="card space-y-4">
          <h2 className="text-sm font-medium text-text-secondary uppercase tracking-wider">
            Builder performance
          </h2>
          <BuilderTable />
        </div>

        <div className="card space-y-4">
          <LatencyBar latency={latency} />
          <div className="border-t border-bg-border pt-2">
            <div className="flex items-center justify-between text-xs text-text-muted mt-1">
              <span>Total pipeline</span>
              <span className="font-mono">{totalPipelineMs.toFixed(1)} ms</span>
            </div>
          </div>
        </div>
      </div>

      {/* Flash provider breakdown */}
      <div className="card space-y-4">
        <h2 className="text-sm font-medium text-text-secondary uppercase tracking-wider">
          Flash provider usage
        </h2>
        <FlashBreakdownBars breakdown={breakdown} />
        <p className="text-[10px] text-text-muted">
          Balancer (0% fee) is the primary provider. Aave (0.05% fee) and Morpho are fallbacks.
        </p>
      </div>

      {/* Recent bundle log */}
      <div className="card space-y-4">
        <div className="flex items-center justify-between">
          <h2 className="text-sm font-medium text-text-secondary uppercase tracking-wider">
            Recent bundle log
          </h2>
          <span className="text-xs text-text-muted font-mono">
            {bundleLog.length} entries
          </span>
        </div>
        <BundleLogTable entries={bundleLog} />
      </div>

      {/* Whale detections */}
      <div className="card">
        <p className="stat-label mb-2">Whale detections</p>
        <p className="font-mono text-2xl text-text-primary">
          {whale_detections}
        </p>
        <p className="text-xs text-text-muted mt-1">
          Pending mempool — MEV-Share backrun bundles pre-built for $5M+ swaps
        </p>
      </div>

      {/* Submission notes */}
      <div className="card">
        <p className="stat-label mb-4">Submission notes</p>
        <ul className="text-sm text-text-secondary space-y-2">
          <li>
            • Bundles submitted to all 6 builders simultaneously via{" "}
            <code className="font-mono text-xs bg-bg-raised px-1 rounded">join_all</code>
          </li>
          <li>• Flashbots, Titan, BeaverBuild, Rsync, Builder0x69, bloXroute</li>
          <li>
            • MEV-Share back-runs via{" "}
            <code className="font-mono text-xs bg-bg-raised px-1 rounded">mev_sendBundle</code>{" "}
            (v0.1, hints: calldata + function_selector)
          </li>
          <li>
            •{" "}
            <code className="font-mono text-xs bg-bg-raised px-1 rounded">SUBMISSION_ENABLED=false</code>{" "}
            — paper trading unless explicitly enabled
          </li>
        </ul>
      </div>
    </div>
  );
}
