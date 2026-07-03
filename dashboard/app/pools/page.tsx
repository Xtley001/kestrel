"use client";

// app/pools/page.tsx
// : Pools dashboard page.
// Shows: real-time spread per pool, protocol rate vs DEX price,
// optimal trade size (binary search result), actionable indicator,
// pool depth, session stats, profit sparkline, latency bar.
// rate cache age highlighted amber when >1 block; spread bps time-series chart added.

import { useBotStore } from "@/store/botStore";
import { StatCard } from "@/components/StatCard";
import { SpreadBadge } from "@/components/SpreadBadge";
import { ProfitChart } from "@/components/ProfitChart";
import { LatencyBar } from "@/components/LatencyBar";
import { BuilderTable } from "@/components/BuilderTable";
import { clsx } from "clsx";
import { Activity, Zap } from "lucide-react";
import { formatDistanceToNow } from "date-fns";
import { useRef, useEffect } from "react";
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
  Legend,
} from "recharts";

// ── helpers ──────────────────────────────────────────────────────────────────

function formatBigNum(raw: string): string {
  const n = parseFloat(raw) / 1e18;
  if (n >= 1_000_000) return `$${(n / 1_000_000).toFixed(2)}M`;
  if (n >= 1_000)     return `$${(n / 1_000).toFixed(1)}K`;
  return `$${n.toFixed(2)}`;
}

function formatRate(raw: string): string {
  const n = parseFloat(raw) / 1e18;
  return n.toFixed(8);
}

function formatSize(raw: string): string {
  const n = parseFloat(raw) / 1e18;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M USDS`;
  if (n >= 1_000)     return `${(n / 1_000).toFixed(0)}K USDS`;
  return `${n.toFixed(0)} USDS`;
}

// ── SpreadHistory — per-pool spread bps time-series chart ────────────────
// Maintains a rolling window of spread bps samples keyed by pool name.
// Data is accumulated from the live WS stream on every block.
const MAX_HISTORY = 120; // ~2 minutes of block history

function SpreadHistory({ pools }: { pools: import("@/store/botStore").PoolSpreadState[] }) {
  // history: { block: number; [poolName: string]: number }[]
  const historyRef = useRef<Record<string, number>[]>([]);
  const blockRef   = useRef<number>(0);

  // Append a new data point on each render when the block changes
  const latestBlock = pools[0]?.pool_name ? (pools.reduce((_, p) => p, pools[0]) as unknown as { block?: number }) : null;

  // Build this block's data point
  const dataPoint: Record<string, number> = { block: blockRef.current };
  for (const p of pools) {
    dataPoint[p.pool_name] = p.spread_bps;
  }

  // Only push a new sample when it's actually different from the last
  const last = historyRef.current[historyRef.current.length - 1];
  const isNew = !last || pools.some((p) => last[p.pool_name] !== p.spread_bps);
  if (isNew && pools.length > 0) {
    historyRef.current = [
      ...historyRef.current.slice(-(MAX_HISTORY - 1)),
      dataPoint,
    ];
  }

  const history = historyRef.current;
  if (history.length < 2) {
    return (
      <div className="text-xs text-text-muted text-center py-6">
        Accumulating spread history…
      </div>
    );
  }

  // One line per pool — use the accent-green for the primary pool
  const poolNames = pools.map((p) => p.pool_name);
  const COLORS = ["#00e676", "#ffab00", "#ff3d3d"];

  return (
    <ResponsiveContainer width="100%" height={160}>
      <LineChart data={history} margin={{ top: 4, right: 4, left: -20, bottom: 0 }}>
        <XAxis
          dataKey="block"
          tick={{ fontSize: 9, fill: "#555555" }}
          tickFormatter={(v: number) => `#${v}`}
          interval="preserveStartEnd"
        />
        <YAxis
          tick={{ fontSize: 9, fill: "#555555" }}
          tickFormatter={(v: number) => `${v}bps`}
          width={42}
        />
        <Tooltip
          contentStyle={{ background: "#1a1a1a", border: "1px solid #222222", fontSize: 11 }}
          formatter={(v: number, name: string) => [`${v} bps`, name]}
        />
        {poolNames.map((name, i) => (
          <Line
            key={name}
            type="monotone"
            dataKey={name}
            stroke={COLORS[i % COLORS.length]}
            strokeWidth={1.5}
            dot={false}
            isAnimationActive={false}
          />
        ))}
        <Legend
          wrapperStyle={{ fontSize: 10, color: "#888888" }}
          iconType="plainline"
        />
      </LineChart>
    </ResponsiveContainer>
  );
}

export default function PoolsPage() {
  const metrics     = useBotStore((s) => s.metrics);
  const wsStatus    = useBotStore((s) => s.wsStatus);
  const lastUpdated = useBotStore((s) => s.lastUpdated);

  if (wsStatus === "connecting") {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="text-center space-y-3">
          <div className="w-6 h-6 border-2 border-accent-green border-t-transparent rounded-full animate-spin mx-auto" />
          <p className="text-text-secondary text-sm">Connecting to bot…</p>
        </div>
      </div>
    );
  }

  if (!metrics) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="text-center space-y-2">
          <Activity size={32} className="text-text-muted mx-auto" />
          <p className="text-text-secondary">No data — bot offline or no blocks yet</p>
          <p className="text-text-muted text-sm">Status: {wsStatus}</p>
        </div>
      </div>
    );
  }

  const { pools, bundles_submitted, bundles_landed, bundles_reverted,
          session_profit_usds, gas_spent_usd, latency, whale_detections,
          block_number, chains, rate_cache_age_blocks } = metrics;

  const landingRate = bundles_submitted > 0
    ? ((bundles_landed / bundles_submitted) * 100).toFixed(1)
    : "—";

  return (
    <div className="space-y-6 animate-fade-in">
      {/* Header */}
      <div className="flex items-center justify-between">
        <div>
          <h1 className="text-xl font-semibold text-text-primary">Pool Monitor</h1>
          <p className="text-sm text-text-muted mt-0.5">
            Block {block_number.toLocaleString()} ·{" "}
            {lastUpdated ? formatDistanceToNow(lastUpdated, { addSuffix: true }) : "—"}
          </p>
        </div>
        <div className="flex items-center gap-2">
          {chains.map((c) => (
            <div
              key={c.chain}
              className={clsx(
                "flex items-center gap-1.5 px-3 py-1.5 rounded-pill text-xs font-mono border",
                c.active
                  ? "border-accent-green/30 text-accent-green bg-accent-green/5"
                  : "border-bg-border text-text-muted"
              )}
            >
              <span className={clsx("w-1.5 h-1.5 rounded-full", c.active ? "bg-accent-green" : "bg-text-muted")} />
              {c.chain}
            </div>
          ))}
        </div>
      </div>

      {/* Top stats row */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
        <StatCard
          label="Session profit"
          value={formatBigNum(session_profit_usds)}
          trend="up"
          highlight={parseFloat(session_profit_usds) > 0}
        />
        <StatCard
          label="Landing rate"
          value={`${landingRate}%`}
          sub={`${bundles_landed}/${bundles_submitted} bundles`}
        />
        <StatCard
          label="On-chain reverts"
          value={bundles_reverted.toString()}
          trend={bundles_reverted > 0 ? "down" : "neutral"}
        />
        <StatCard
          label="Whale detections"
          value={whale_detections.toString()}
          sub="pending mempool"
        />
      </div>

      {/* Pool spread cards */}
      <section>
        <h2 className="text-sm font-medium text-text-secondary mb-3 uppercase tracking-wider">
          Monitored Pools
        </h2>
        {pools.length === 0 ? (
          <div className="card text-text-muted text-sm text-center py-8">
            No pool data in last block
          </div>
        ) : (
          <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4">
            {pools.map((pool, i) => (
              <PoolCard key={`${pool.pool_name}-${pool.chain}`} pool={pool} />
            ))}
          </div>
        )}
      </section>

      {/* Bottom row: profit chart + latency + builders */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        {/* Profit sparkline */}
        <div className="card lg:col-span-1">
          <ProfitChart />
        </div>

        {/* Latency breakdown */}
        <div className="card lg:col-span-1">
          <LatencyBar latency={latency} />
        </div>

        {/* Builder landing rates */}
        <div className="card lg:col-span-1">
          <p className="stat-label mb-3">Builder performance</p>
          <BuilderTable />
        </div>
      </div>

      {/* Gas & rate cache */}
      <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
        <StatCard label="Gas spent today"  value={formatBigNum(gas_spent_usd)} />
        {/* rate cache age highlighted amber when > 1 block */}
        <div className={clsx(
          "card flex flex-col gap-1 p-3",
          rate_cache_age_blocks > 1 && "border border-accent-amber/40 bg-accent-amber/5"
        )}>
          <span className="text-xs text-text-secondary uppercase tracking-wider">Rate cache age</span>
          <span className={clsx(
            "font-mono text-lg font-semibold",
            rate_cache_age_blocks > 1 ? "text-accent-amber" : "text-text-primary"
          )}>
            {rate_cache_age_blocks} block{rate_cache_age_blocks !== 1 ? "s" : ""}
          </span>
          {rate_cache_age_blocks > 1 && (
            <span className="text-[10px] text-accent-amber uppercase tracking-wider">
              ⚠ stale
            </span>
          )}
        </div>
        <StatCard label="Bundles submitted" value={bundles_submitted.toString()} mono />
        <StatCard label="Latest block"      value={block_number.toLocaleString()} mono />
      </div>

      {/* Spread bps time-series chart */}
      <section>
        <h2 className="text-sm font-medium text-text-secondary mb-3 uppercase tracking-wider">
          Spread History
        </h2>
        <SpreadHistory pools={pools} />
      </section>
    </div>
  );
}

// ── Pool card ───────────────────────────────────────────────────────────────

function PoolCard({ pool }: { pool: import("@/store/botStore").PoolSpreadState }) {
  return (
    <div className={clsx(
      "card space-y-3 transition-all",
      pool.actionable && "glow-green border-accent-green/20"
    )}>
      {/* Header */}
      <div className="flex items-start justify-between">
        <div>
          <p className="font-semibold text-text-primary text-sm">{pool.pool_name}</p>
          <p className="text-xs text-text-muted">{pool.chain}</p>
        </div>
        <SpreadBadge bps={pool.spread_bps} direction={pool.direction} />
      </div>

      {/* Rate comparison */}
      <div className="space-y-1.5">
        <div className="flex items-center justify-between text-xs">
          <span className="text-text-muted">Protocol rate</span>
          <span className="font-mono text-text-primary">{formatRate(pool.protocol_rate)}</span>
        </div>
        <div className="flex items-center justify-between text-xs">
          <span className="text-text-muted">DEX price</span>
          <span className={clsx(
            "font-mono",
            pool.direction === "DISCOUNT" ? "text-accent-amber" : "text-accent-purple"
          )}>
            {formatRate(pool.dex_price)}
          </span>
        </div>
      </div>

      <div className="divider" />

      {/* Optimal size + depth */}
      <div className="space-y-1.5">
        <div className="flex items-center justify-between text-xs">
          <span className="text-text-muted">Optimal size</span>
          <span className="font-mono text-text-secondary">{formatSize(pool.optimal_size)}</span>
        </div>
        <div className="flex items-center justify-between text-xs">
          <span className="text-text-muted">Pool depth</span>
          <span className="font-mono text-text-secondary">{formatSize(pool.pool_depth)}</span>
        </div>
      </div>

      {/* Actionable indicator */}
      <div className={clsx(
        "flex items-center gap-1.5 text-xs px-2 py-1 rounded-md",
        pool.actionable
          ? "bg-accent-green/10 text-accent-green"
          : "bg-bg-raised text-text-muted"
      )}>
        <Zap size={12} />
        {pool.actionable ? "Actionable" : "Below threshold"}
      </div>
    </div>
  );
}
