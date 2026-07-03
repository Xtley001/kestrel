"use client";

import { useBotStore } from "@/store/botStore";

export function BuilderTable() {
  const builders = useBotStore((s) => s.metrics?.builders ?? []);

  if (builders.length === 0) {
    return (
      <div className="text-sm text-text-muted text-center py-6">
        No builder data yet
      </div>
    );
  }

  return (
    <div className="overflow-x-auto">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b border-bg-border">
            <th className="text-left py-2 px-3 text-xs text-text-muted uppercase tracking-wider">Builder</th>
            <th className="text-right py-2 px-3 text-xs text-text-muted uppercase tracking-wider">Submitted</th>
            <th className="text-right py-2 px-3 text-xs text-text-muted uppercase tracking-wider">Landed</th>
            <th className="text-right py-2 px-3 text-xs text-text-muted uppercase tracking-wider">Rate</th>
          </tr>
        </thead>
        <tbody>
          {builders.map((b) => {
            const rate = b.submitted > 0 ? (b.landed / b.submitted) * 100 : 0;
            return (
              <tr key={b.name} className="border-b border-bg-border/50 hover:bg-bg-raised/30 transition-colors">
                <td className="py-2 px-3 font-mono text-text-primary">{b.name}</td>
                <td className="py-2 px-3 text-right font-mono text-text-secondary">{b.submitted}</td>
                <td className="py-2 px-3 text-right font-mono text-text-secondary">{b.landed}</td>
                <td className="py-2 px-3 text-right font-mono">
                  <span className={rate > 50 ? "text-accent-green" : rate > 20 ? "text-accent-amber" : "text-accent-red"}>
                    {rate.toFixed(1)}%
                  </span>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
