"use client";

import { useBotStore } from "@/store/botStore";
import { X, AlertTriangle, CheckCircle, Info } from "lucide-react";

export function Toaster() {
  const alerts = useBotStore((s) => s.alerts);
  const acknowledge = useBotStore((s) => s.acknowledgeAlert);

  const visible = alerts.filter((a) => !a.acknowledged).slice(0, 4);

  if (visible.length === 0) return null;

  return (
    <div className="fixed bottom-4 right-4 z-50 flex flex-col gap-2 w-80">
      {visible.map((alert) => (
        <div
          key={alert.id}
          className={`
            card flex items-start gap-3 p-3 animate-slide-up
            ${alert.severity === "red"   ? "border-accent-red/40"   : ""}
            ${alert.severity === "amber" ? "border-accent-amber/40" : ""}
            ${alert.severity === "green" ? "border-accent-green/40" : ""}
          `}
        >
          <div className="shrink-0 mt-0.5">
            {alert.severity === "red"   && <AlertTriangle size={16} className="text-accent-red" />}
            {alert.severity === "amber" && <AlertTriangle size={16} className="text-accent-amber" />}
            {alert.severity === "green" && <CheckCircle   size={16} className="text-accent-green" />}
          </div>
          <div className="flex-1 min-w-0">
            <p className="text-xs text-text-secondary uppercase tracking-wide">{alert.chain}</p>
            <p className="text-sm text-text-primary mt-0.5 leading-snug">{alert.message}</p>
          </div>
          <button
            onClick={() => acknowledge(alert.id)}
            className="shrink-0 text-text-muted hover:text-text-secondary transition-colors"
          >
            <X size={14} />
          </button>
        </div>
      ))}
    </div>
  );
}
