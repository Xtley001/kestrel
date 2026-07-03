import { clsx } from "clsx";

interface StatCardProps {
  label: string;
  value: string;
  sub?: string;
  trend?: "up" | "down" | "neutral";
  highlight?: boolean;
  mono?: boolean;
}

export function StatCard({ label, value, sub, trend, highlight, mono }: StatCardProps) {
  return (
    <div className={clsx("card flex flex-col gap-1", highlight && "glow-green border-accent-green/20")}>
      <p className="stat-label">{label}</p>
      <p className={clsx("stat-number", mono && "font-mono", {
        "text-accent-green":  trend === "up",
        "text-accent-red":    trend === "down",
        "text-text-primary":  !trend || trend === "neutral",
      })}>
        {value}
      </p>
      {sub && <p className="text-xs text-text-muted">{sub}</p>}
    </div>
  );
}
