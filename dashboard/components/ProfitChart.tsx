"use client";

import { useBotStore } from "@/store/botStore";
import {
  AreaChart,
  Area,
  XAxis,
  YAxis,
  Tooltip,
  ResponsiveContainer,
} from "recharts";

export function ProfitChart() {
  const history = useBotStore((s) => s.profitHistory);

  if (history.length < 2) {
    return (
      <div className="h-32 flex items-center justify-center text-text-muted text-sm">
        Awaiting data…
      </div>
    );
  }

  const data = history.map((p) => ({
    block: p.block,
    profit: p.profit_usds,
  }));

  const latestProfit = data[data.length - 1]?.profit ?? 0;

  return (
    <div className="space-y-1">
      <div className="flex items-baseline justify-between">
        <p className="stat-label">Session profit (USDS)</p>
        <p className="font-mono text-lg font-semibold text-accent-green">
          ${latestProfit.toLocaleString("en-US", { maximumFractionDigits: 0 })}
        </p>
      </div>
      <ResponsiveContainer width="100%" height={96}>
        <AreaChart data={data} margin={{ top: 4, right: 0, left: 0, bottom: 0 }}>
          <defs>
            <linearGradient id="profitGradient" x1="0" y1="0" x2="0" y2="1">
              <stop offset="5%"  stopColor="#22c55e" stopOpacity={0.25} />
              <stop offset="95%" stopColor="#22c55e" stopOpacity={0} />
            </linearGradient>
          </defs>
          <XAxis
            dataKey="block"
            hide
          />
          <YAxis hide domain={["auto", "auto"]} />
          <Tooltip
            contentStyle={{
              background: "#141720",
              border: "1px solid #252a38",
              borderRadius: 8,
              fontSize: 12,
            }}
            labelStyle={{ color: "#8a93a8" }}
            itemStyle={{ color: "#22c55e" }}
            formatter={(v: number) =>
              [`$${v.toLocaleString("en-US", { maximumFractionDigits: 0 })}`, "Profit"]
            }
            labelFormatter={(b) => `Block ${b}`}
          />
          <Area
            type="monotone"
            dataKey="profit"
            stroke="#22c55e"
            strokeWidth={1.5}
            fill="url(#profitGradient)"
            dot={false}
            activeDot={{ r: 3, fill: "#22c55e" }}
          />
        </AreaChart>
      </ResponsiveContainer>
    </div>
  );
}
