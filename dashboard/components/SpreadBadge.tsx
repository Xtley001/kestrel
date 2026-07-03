import { clsx } from "clsx";

interface SpreadBadgeProps {
  bps: number;
  direction?: "DISCOUNT" | "PREMIUM";
  size?: "sm" | "md";
}

export function SpreadBadge({ bps, direction, size = "md" }: SpreadBadgeProps) {
  const colour =
    bps >= 15 ? "badge-red"    :
    bps >= 8  ? "badge-amber"  :
    bps >= 5  ? "badge-green"  :
    "badge-muted";

  const dir = direction === "PREMIUM" ? "▲" : direction === "DISCOUNT" ? "▼" : "";

  return (
    <span className={clsx(
      "inline-flex items-center gap-1 rounded-pill font-mono font-semibold",
      colour,
      size === "sm" ? "text-xs px-2 py-0.5" : "text-sm px-2.5 py-1"
    )}>
      {dir && <span className="text-[10px]">{dir}</span>}
      {bps.toFixed(1)} bps
    </span>
  );
}
