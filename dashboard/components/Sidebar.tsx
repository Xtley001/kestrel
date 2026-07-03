"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useBotStore } from "@/store/botStore";
import {
  LayoutGrid,
  Zap,
  SlidersHorizontal,
  Bell,
  Activity,
} from "lucide-react";
import { clsx } from "clsx";

const NAV = [
  { href: "/pools",     label: "Pools",     icon: LayoutGrid },
  { href: "/execution", label: "Execution", icon: Zap },
  { href: "/controls",  label: "Controls",  icon: SlidersHorizontal },
  { href: "/alerts",    label: "Alerts",    icon: Bell },
];

export function Sidebar() {
  const pathname = usePathname();
  const wsStatus = useBotStore((s) => s.wsStatus);
  const unread   = useBotStore((s) => s.unacknowledgedAlerts);

  return (
    <aside className="w-56 shrink-0 flex flex-col bg-bg-surface border-r border-bg-border min-h-screen">
      {/* Logo */}
      <div className="px-5 py-5 border-b border-bg-border">
        <div className="flex items-center gap-2.5">
          <Activity size={20} className="text-accent-green" />
          <span className="font-semibold text-text-primary tracking-tight">Kestrel</span>
        </div>
        <p className="text-[10px] text-text-muted mt-1 uppercase tracking-wider">MEV Dashboard</p>
      </div>

      {/* Nav */}
      <nav className="flex-1 px-3 py-4 flex flex-col gap-1">
        {NAV.map(({ href, label, icon: Icon }) => {
          const active = pathname.startsWith(href);
          return (
            <Link
              key={href}
              href={href}
              className={clsx(
                "flex items-center gap-3 px-3 py-2.5 rounded-lg text-sm transition-colors",
                active
                  ? "bg-bg-raised text-text-primary"
                  : "text-text-secondary hover:text-text-primary hover:bg-bg-raised/50"
              )}
            >
              <Icon size={16} />
              <span>{label}</span>
              {label === "Alerts" && unread > 0 && (
                <span className="ml-auto text-[10px] bg-accent-red text-white rounded-full w-4 h-4 flex items-center justify-center font-mono font-bold">
                  {unread > 9 ? "9+" : unread}
                </span>
              )}
            </Link>
          );
        })}
      </nav>

      {/* Connection status */}
      <div className="px-5 py-4 border-t border-bg-border">
        <div className="flex items-center gap-2">
          <span
            className={clsx(
              "w-2 h-2 rounded-full",
              wsStatus === "connected"   && "bg-accent-green animate-pulse-slow",
              wsStatus === "connecting"  && "bg-accent-amber animate-pulse",
              wsStatus === "error"       && "bg-accent-red",
              wsStatus === "disconnected"&& "bg-text-muted"
            )}
          />
          <span className="text-xs text-text-secondary capitalize">{wsStatus}</span>
        </div>
        <p className="text-[10px] text-text-muted mt-1">ws://127.0.0.1:9101</p>
      </div>
    </aside>
  );
}
