"use client";

// app/controls/page.tsx
// / Operator control panel.
// fixes:
//  - Optimistic pending state on every command (UI reflects intent immediately)
//  - 5-second revert timeout: if bot doesn't echo confirmation, UI reverts
//  - 4 additional threshold sliders: daily gas limit, binary search ceiling,
//    pre-sign threshold, fire threshold (Controls spec)
// All commands signed Ed25519 (tweetnacl) before sending to /api/control.

import { useState, useRef, useCallback, useEffect } from "react";
import { useBotStore } from "@/store/botStore";
import { clsx } from "clsx";
import nacl from "tweetnacl";

type CommandStatus = "idle" | "pending" | "confirmed" | "timeout" | "error";

interface CommandResult {
  command: string;
  status: CommandStatus;
  ts: number;
}

function encodeHex(buf: Uint8Array): string {
  return Array.from(buf).map((b) => b.toString(16).padStart(2, "0")).join("");
}

function decodeHex(hex: string): Uint8Array {
  if (hex.startsWith("0x")) hex = hex.slice(2);
  const arr = new Uint8Array(hex.length / 2);
  for (let i = 0; i < arr.length; i++) {
    arr[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  }
  return arr;
}

// ── SliderControl ─────────────────────────────────────────────────────────────
interface SliderControlProps {
  label: string;
  sub?: string;
  value: string;
  onChange: (v: string) => void;
  min: number;
  max: number;
  step?: number;
  unit?: string;
  disabled?: boolean;
}

function SliderControl({
  label, sub, value, onChange, min, max, step = 1, unit = "", disabled,
}: SliderControlProps) {
  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between text-xs">
        <div>
          <span className="text-text-primary font-medium">{label}</span>
          {sub && <span className="text-text-muted ml-2">{sub}</span>}
        </div>
        <span className="font-mono text-text-primary">
          {value}
          {unit}
        </span>
      </div>
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        disabled={disabled}
        className={clsx(
          "w-full h-[3px] rounded-full appearance-none cursor-pointer",
          "bg-bg-border [&::-webkit-slider-thumb]:appearance-none",
          "[&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:h-3",
          "[&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-accent-green",
          "[&::-webkit-slider-thumb]:cursor-pointer",
          disabled && "opacity-40 cursor-not-allowed"
        )}
      />
      <div className="flex justify-between text-[10px] text-text-muted font-mono">
        <span>{min}{unit}</span>
        <span>{max}{unit}</span>
      </div>
    </div>
  );
}

// ── CommandStatusBadge ─────────────────────────────────────────────────────────
function CommandStatusBadge({ status }: { status: CommandStatus }) {
  return (
    <span className={clsx(
      "font-mono text-[10px] uppercase tracking-wider px-1.5 py-0.5 rounded",
      status === "confirmed" && "bg-accent-green/10 text-accent-green",
      status === "pending"   && "bg-accent-amber/10 text-accent-amber animate-pulse",
      status === "timeout"   && "bg-accent-red/10 text-accent-red",
      status === "error"     && "bg-accent-red/10 text-accent-red",
      status === "idle"      && "bg-bg-raised text-text-muted",
    )}>
      {status}
    </span>
  );
}

// ── ControlsPage ──────────────────────────────────────────────────────────────
export default function ControlsPage() {
  const [privateKeyHex, setPrivateKeyHex] = useState("");
  const [keyLoaded, setKeyLoaded]         = useState(false);
  const [results, setResults]             = useState<CommandResult[]>([]);
  const keyRef = useRef<nacl.SignKeyPair | null>(null);

  // ── Threshold state (6 sliders ) ─────────────────
  const [minSpreadBps,   setMinSpreadBps]   = useState("5");
  const [minProfitUsd,   setMinProfitUsd]   = useState("1000");
  const [dailyGasLimitE, setDailyGasLimit]  = useState("0.5");   // ETH
  const [searchCeilingM, setSearchCeiling]  = useState("20");    // USD millions
  const [preSignBps,     setPreSignBps]     = useState("8");
  const [fireBps,        setFireBps]        = useState("15");

  // ── Bot / chain toggles ────────────────────────────────────────────────────
  const [paused, setPaused]               = useState(false);
  const [ethEnabled, setEthEnabled]       = useState(true);
  const [arbEnabled, setArbEnabled]       = useState(true);
  const [baseEnabled, setBaseEnabled]     = useState(true);
  const [dir2On, setDir2On]               = useState(true);

  const submissionEnabled = useBotStore((s) => s.submissionEnabled);
  const setSubmission     = useBotStore((s) => s.setSubmissionEnabled);

  // ── Key loading ───────────────────────────────────────────────────────────
  const loadKey = () => {
    try {
      const secretKey = decodeHex(privateKeyHex.trim());
      if (secretKey.length !== 64) throw new Error("Key must be 64 bytes (Ed25519 secret key)");
      keyRef.current = nacl.sign.keyPair.fromSecretKey(secretKey);
      setKeyLoaded(true);
      setPrivateKeyHex(""); // Clear from state immediately
    } catch (e: unknown) {
      alert(`Key error: ${(e as Error).message}`);
    }
  };

  // ── Send command ─────────────────────────────────────────────────────────
  // Optimistic pending → confirmed/timeout pattern.
  // If the /api/control relay echoes a confirmation within 5 seconds the
  // status flips to "confirmed".  If not, it reverts to "timeout" to signal
  // the operator that the change may not have taken effect.
  const sendCommand = useCallback(
    async (command: string, params: Record<string, unknown>) => {
      if (!keyRef.current) {
        alert("Load signing key first");
        return;
      }

      const payload = JSON.stringify({ command, params });
      const encoded = new TextEncoder().encode(payload);
      const signed  = nacl.sign.detached(encoded, keyRef.current.secretKey);

      const msg = {
        command,
        params,
        signature: encodeHex(signed),
        pubkey:    encodeHex(keyRef.current.publicKey),
      };

      const ts = Date.now();

      // Optimistic pending state — UI shows intent immediately
      setResults((prev) =>
        [{ command, status: "pending" as CommandStatus, ts }, ...prev].slice(0, 30)
      );

      // 5-second revert timeout — if no confirmation, flip to timeout
      const revertTimer = setTimeout(() => {
        setResults((prev) =>
          prev.map((r) => (r.ts === ts && r.status === "pending" ? { ...r, status: "timeout" } : r))
        );
      }, 5_000);

      try {
        const res = await fetch("/api/control", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(msg),
        });

        if (res.ok) {
          clearTimeout(revertTimer);
          setResults((prev) =>
            prev.map((r) => (r.ts === ts ? { ...r, status: "confirmed" } : r))
          );
        } else {
          clearTimeout(revertTimer);
          setResults((prev) =>
            prev.map((r) => (r.ts === ts ? { ...r, status: "error" } : r))
          );
        }
      } catch {
        clearTimeout(revertTimer);
        setResults((prev) =>
          prev.map((r) => (r.ts === ts ? { ...r, status: "error" } : r))
        );
      }
    },
    []
  );

  // ── Toggle helper ─────────────────────────────────────────────────────────
  const toggle = (
    command: string,
    current: boolean,
    setter: (v: boolean) => void,
    paramKey: string
  ) => {
    const next = !current;
    setter(next);
    sendCommand(command, { [paramKey]: next });
  };

  return (
    <div className="space-y-6 animate-fade-in max-w-2xl">
      <div>
        <h1 className="text-xl font-semibold text-text-primary">Controls</h1>
        <p className="text-sm text-text-muted mt-0.5">
          Signed operator commands — all messages verified Ed25519 by bot
        </p>
      </div>

      {/* ── Key loading ────────────────────────────────────────────────── */}
      <div className="card space-y-3">
        <p className="text-xs text-text-secondary uppercase tracking-wider font-medium">
          Signing key
        </p>
        {keyLoaded ? (
          <div className="flex items-center gap-2 text-sm">
            <span className="w-2 h-2 rounded-full bg-accent-green" />
            <span className="text-accent-green text-xs">
              Key loaded — in browser memory only, never persisted
            </span>
          </div>
        ) : (
          <div className="flex gap-2">
            <input
              type="password"
              placeholder="Ed25519 secret key (hex, 128 chars)"
              value={privateKeyHex}
              onChange={(e) => setPrivateKeyHex(e.target.value)}
              className="flex-1 bg-bg-raised border border-bg-border rounded px-3 py-2 text-sm font-mono text-text-primary placeholder-text-muted focus:outline-none focus:border-accent-green/30"
            />
            <button
              onClick={loadKey}
              className="px-4 py-2 bg-accent-green/10 border border-accent-green/20 text-accent-green rounded text-sm hover:bg-accent-green/20 transition-colors"
            >
              Load
            </button>
          </div>
        )}
        <p className="text-[10px] text-text-muted">
          Page reload clears the key. Never enters the server — signing happens client-side.
        </p>
      </div>

      {/* ── Bot control ────────────────────────────────────────────────── */}
      <div className="card space-y-4">
        <p className="text-xs text-text-secondary uppercase tracking-wider font-medium">
          Bot control
        </p>

        {/* Submission enabled */}
        <div className="flex items-center justify-between">
          <div>
            <p className="text-sm text-text-primary">Live submission</p>
            <p className="text-xs text-text-muted mt-0.5">
              Enable to submit bundles on-chain
            </p>
          </div>
          <button
            onClick={() => {
              const next = !submissionEnabled;
              setSubmission(next);
              sendCommand("set_submission", { enabled: next });
            }}
            className={clsx(
              "w-9 h-5 rounded-full relative transition-colors shrink-0",
              submissionEnabled ? "bg-accent-green" : "bg-bg-border"
            )}
          >
            <span className={clsx(
              "absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform",
              submissionEnabled ? "left-4" : "left-0.5"
            )} />
          </button>
        </div>

        {/* Pause / resume */}
        <div className="flex items-center justify-between border-t border-bg-border pt-3">
          <div>
            <p className="text-sm text-text-primary">
              Bot {paused ? "paused" : "running"}
            </p>
            <p className="text-xs text-text-muted mt-0.5">
              Pausing stops all bundle submissions immediately
            </p>
          </div>
          <button
            onClick={() => toggle("pause_resume", paused, setPaused, "paused")}
            className={clsx(
              "px-4 py-1.5 rounded text-sm border transition-colors shrink-0",
              paused
                ? "bg-accent-green/10 border-accent-green/20 text-accent-green hover:bg-accent-green/20"
                : "bg-accent-amber/10 border-accent-amber/20 text-accent-amber hover:bg-accent-amber/20"
            )}
          >
            {paused ? "Resume" : "Pause"}
          </button>
        </div>
      </div>

      {/* ── Per-chain enables ───────────────────────────────────────────── */}
      <div className="card space-y-4">
        <p className="text-xs text-text-secondary uppercase tracking-wider font-medium">
          Chain enables
        </p>
        {[
          { label: "Ethereum", state: ethEnabled, setter: setEthEnabled, key: "eth_enabled" },
          { label: "Arbitrum", state: arbEnabled, setter: setArbEnabled, key: "arb_enabled" },
          { label: "Base",     state: baseEnabled, setter: setBaseEnabled, key: "base_enabled" },
        ].map((c) => (
          <div key={c.key} className="flex items-center justify-between">
            <span className="text-sm text-text-primary font-mono">{c.label}</span>
            <button
              onClick={() => toggle("set_chain_enabled", c.state, c.setter, c.key)}
              className={clsx(
                "w-9 h-5 rounded-full relative transition-colors shrink-0",
                c.state ? "bg-accent-green" : "bg-bg-border"
              )}
            >
              <span className={clsx(
                "absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform",
                c.state ? "left-4" : "left-0.5"
              )} />
            </button>
          </div>
        ))}
      </div>

      {/* ── Feature toggles ─────────────────────────────────────────────── */}
      <div className="card space-y-4">
        <p className="text-xs text-text-secondary uppercase tracking-wider font-medium">
          Feature toggles
        </p>


        <div className="flex items-center justify-between border-t border-bg-border pt-3">
          <div>
            <p className="text-sm text-text-primary">Direction-2 arb</p>
            <p className="text-xs text-text-muted">Premium (DEX &gt; protocol) monitoring</p>
          </div>
          <button
            onClick={() => toggle("set_direction2", dir2On, setDir2On, "enabled")}
            className={clsx(
              "w-9 h-5 rounded-full relative transition-colors shrink-0",
              dir2On ? "bg-accent-green" : "bg-bg-border"
            )}
          >
            <span className={clsx("absolute top-0.5 w-4 h-4 rounded-full bg-white transition-transform", dir2On ? "left-4" : "left-0.5")} />
          </button>
        </div>
      </div>

      {/* ── Threshold sliders ────────────────────────────────────────────── */}
      {/* All 6 thresholds Controls spec */}
      <div className="card space-y-6">
        <p className="text-xs text-text-secondary uppercase tracking-wider font-medium">
          Thresholds
        </p>

        <SliderControl
          label="Min spread"
          sub="below which no bundle is built"
          value={minSpreadBps}
          onChange={setMinSpreadBps}
          min={1}
          max={50}
          unit=" bps"
        />
        <SliderControl
          label="Min profit"
          sub="passed as minProfit to contract"
          value={minProfitUsd}
          onChange={setMinProfitUsd}
          min={100}
          max={50_000}
          step={100}
          unit=" USDS"
        />
        <SliderControl
          label="Daily gas limit"
          sub="hard budget ceiling"
          value={dailyGasLimitE}
          onChange={setDailyGasLimit}
          min={0.1}
          max={5}
          step={0.1}
          unit=" ETH"
        />
        <SliderControl
          label="Binary search ceiling"
          sub="max flash loan size"
          value={searchCeilingM}
          onChange={setSearchCeiling}
          min={1}
          max={50}
          unit="M USDS"
        />
        <SliderControl
          label="Pre-sign threshold"
          sub="build tx above this spread"
          value={preSignBps}
          onChange={setPreSignBps}
          min={1}
          max={30}
          unit=" bps"
        />
        <SliderControl
          label="Fire threshold"
          sub="submit pre-signed tx above this"
          value={fireBps}
          onChange={setFireBps}
          min={preSignBps ? parseInt(preSignBps) + 1 : 2}
          max={50}
          unit=" bps"
        />

        <button
          onClick={() =>
            sendCommand("set_thresholds", {
              min_spread_bps:       parseInt(minSpreadBps),
              min_profit_usds:      parseInt(minProfitUsd),
              daily_gas_limit_eth:  parseFloat(dailyGasLimitE),
              search_ceiling_usd:   parseInt(searchCeilingM) * 1_000_000,
              pre_sign_threshold:   parseInt(preSignBps),
              fire_threshold:       parseInt(fireBps),
            })
          }
          disabled={!keyLoaded}
          className={clsx(
            "w-full py-2 rounded text-sm border transition-colors",
            keyLoaded
              ? "bg-bg-raised border-bg-border text-text-primary hover:bg-bg-border"
              : "opacity-40 cursor-not-allowed bg-bg-raised border-bg-border text-text-muted"
          )}
        >
          Apply all thresholds
        </button>
      </div>

      {/* ── Command log ─────────────────────────────────────────────────── */}
      {results.length > 0 && (
        <div className="card space-y-2">
          <p className="text-xs text-text-secondary uppercase tracking-wider font-medium">
            Command log
          </p>
          {results.map((r, i) => (
            <div
              key={i}
              className="flex items-center justify-between text-xs font-mono border-b border-white/[0.03] pb-1 last:border-0 last:pb-0"
            >
              <span className="text-text-secondary truncate pr-4">{r.command}</span>
              <CommandStatusBadge status={r.status} />
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
