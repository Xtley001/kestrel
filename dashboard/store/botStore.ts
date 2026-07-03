// store/botStore.ts
// Zustand store for Kestrel bot state.
// Connects to bot WebSocket metrics endpoint (127.0.0.1:9101) via WireGuard tunnel.
// All state is in-memory — no localStorage (bot runs on server, not browser storage).

import { create } from "zustand";

// ── Type definitions (mirror of bot metrics_ws.rs) ─────────────────────────

export interface PoolSpreadState {
  pool_name: string;
  chain: string;
  protocol_rate: string;
  dex_price: string;
  spread_bps: number;
  direction: "DISCOUNT" | "PREMIUM";
  optimal_size: string;
  pool_depth: string;
  actionable: boolean;
}

export interface BuilderStats {
  name: string;
  submitted: number;
  landed: number;
}

export interface PipelineLatency {
  ipc_recv_ms: number;
  pool_update_ms: number;
  rate_cache_ms: number;
  binary_search_ms: number;
  revm_sim_ms: number;
  bundle_sign_ms: number;
  submit_all_ms: number;
}

export interface ChainStatus {
  chain: string;
  active: boolean;
  latest_block: number;
}

// Recent bundle log — one entry per attempted bundle
export interface BundleLogEntry {
  block: number;
  flash_size_usds: string;   // raw U256 string (18-dec)
  profit_usds: string;       // raw U256 string (18-dec)
  flash_provider: "Balancer" | "Aave" | "Morpho";
  builders: string[];        // builder names targeted
  landed: boolean;
  reverted: boolean;
  timestamp_ms: number;
}

// Flash provider usage distribution
export interface FlashProviderBreakdown {
  balancer: number;
  aave: number;
  morpho: number;
}

export interface BlockMetrics {
  type: string;
  block_number: number;
  pools: PoolSpreadState[];
  bundles_submitted: number;
  bundles_landed: number;
  bundles_reverted: number;
  session_profit_usds: string;
  gas_spent_usd: string;
  builders: BuilderStats[];
  latency: PipelineLatency;
  whale_detections: number;
  rate_cache_age_blocks: number;
  chains: ChainStatus[];
  // optional extended fields sent by bot (populated once bot sends them)
  recent_bundles?: BundleLogEntry[];
  flash_provider_breakdown?: FlashProviderBreakdown;
}

export type AlertSeverity = "green" | "amber" | "red";

export interface Alert {
  id: string;
  severity: AlertSeverity;
  chain: string;
  message: string;
  timestamp_ms: number;
  acknowledged: boolean;
}

export type ConnectionStatus = "connecting" | "connected" | "disconnected" | "error";

// ── Profit history for sparkline chart ─────────────────────────────────────

export interface ProfitPoint {
  block: number;
  profit_usds: number; // parsed from string
  timestamp: number;
}

// ── Store state + actions ──────────────────────────────────────────────────

interface BotStore {
  // Connection
  wsStatus: ConnectionStatus;
  lastUpdated: Date | null;

  // Bot metrics
  metrics: BlockMetrics | null;
  profitHistory: ProfitPoint[];   // Last 200 data points for sparklines
  alerts: Alert[];
  unacknowledgedAlerts: number;

  // UI state
  selectedChain: "all" | "ETH" | "ARB" | "BASE";
  submissionEnabled: boolean;

  // Actions
  setMetrics: (m: BlockMetrics) => void;
  addAlert: (alert: Omit<Alert, "id" | "acknowledged">) => void;
  acknowledgeAlert: (id: string) => void;
  clearAlerts: () => void;
  setWsStatus: (s: ConnectionStatus) => void;
  setSelectedChain: (c: BotStore["selectedChain"]) => void;
  setSubmissionEnabled: (enabled: boolean) => void;
}

export const useBotStore = create<BotStore>((set, get) => ({
  wsStatus: "disconnected",
  lastUpdated: null,
  metrics: null,
  profitHistory: [],
  alerts: [],
  unacknowledgedAlerts: 0,
  selectedChain: "all",
  submissionEnabled: false,

  setMetrics: (m) =>
    set((state) => {
      // Append to profit history — keep last 200 points.
      // the bot sends session_profit_usds as a plain USD decimal (e.g. "12.50"),
      // NOT an 18-decimal fixed-point string. Parse it directly — the old `/ 1e18`
      // divided an already-USD value into ~0, so profit always rendered as zero.
      const newPoint: ProfitPoint = {
        block: m.block_number,
        profit_usds: parseFloat(m.session_profit_usds) || 0,
        timestamp: Date.now(),
      };
      const history = [...state.profitHistory, newPoint].slice(-200);

      return {
        metrics: m,
        profitHistory: history,
        lastUpdated: new Date(),
      };
    }),

  addAlert: (alert) =>
    set((state) => {
      const newAlert: Alert = {
        ...alert,
        id: `${Date.now()}-${Math.random()}`,
        acknowledged: false,
      };
      return {
        alerts: [newAlert, ...state.alerts].slice(0, 100), // Keep last 100
        unacknowledgedAlerts: state.unacknowledgedAlerts + 1,
      };
    }),

  acknowledgeAlert: (id) =>
    set((state) => ({
      alerts: state.alerts.map((a) =>
        a.id === id ? { ...a, acknowledged: true } : a
      ),
      unacknowledgedAlerts: Math.max(0, state.unacknowledgedAlerts - 1),
    })),

  clearAlerts: () =>
    set({ alerts: [], unacknowledgedAlerts: 0 }),

  setWsStatus: (s) => set({ wsStatus: s }),

  setSelectedChain: (c) => set({ selectedChain: c }),

  setSubmissionEnabled: (enabled) => set({ submissionEnabled: enabled }),
}));
