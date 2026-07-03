import type { Config } from "tailwindcss";

const config: Config = {
  darkMode: "class",
  content: [
    "./pages/**/*.{js,ts,jsx,tsx,mdx}",
    "./components/**/*.{js,ts,jsx,tsx,mdx}",
    "./app/**/*.{js,ts,jsx,tsx,mdx}",
    "./store/**/*.{js,ts,jsx,tsx}",
  ],
  theme: {
    extend: {
      colors: {
        // ── Kestrel design system — exact values from ──────
        // DO NOT diverge from these hex values.
        bg: {
          base:    "#0a0a0a",  // --bg:        page substrate
          surface: "#131313",  // --surface:   navigation, top bar
          raised:  "#1a1a1a",  // --surface-2: cards, panels
          border:  "#222222",  // --border:    all dividing lines
        },
        text: {
          primary:   "#e8e8e8",  // --text:  primary body text
          secondary: "#888888",  // --label: field labels, secondary text
          muted:     "#555555",  // --muted: timestamps, metadata
        },
        accent: {
          green:  "#00e676",  // --green:     profit, landed, healthy, active
          "green-dim": "#00c853",  // --green-dim: secondary green states
          red:    "#ff3d3d",  // --red:       losses, reverts, alerts, paused
          amber:  "#ffab00",  // --amber:     warnings, Aave usage, near-threshold
          purple: "#a855f7",  // --purple:    secondary highlights / badges
        },
      },
      fontFamily: {
        // JetBrains Mono is self-hosted via next/font/local (air-gap requirement)
        // The CSS variable --font-mono is injected by layout.tsx
        mono: ["var(--font-mono)", "monospace"],
        sans: ["system-ui", "sans-serif"],
      },
      borderRadius: {
        pill: "3px",    // status pills — 
        card: "4px",    // cards/panels
      },
      boxShadow: {
        card: "0 1px 3px 0 rgba(0,0,0,0.4), 0 0 0 1px rgba(255,255,255,0.04)",
      },
      animation: {
        "fade-in": "fadeIn 0.2s ease-out",
        "pulse-slow": "pulse 3s cubic-bezier(0.4, 0, 0.6, 1) infinite",
      },
      keyframes: {
        fadeIn: { "0%": { opacity: "0" }, "100%": { opacity: "1" } },
      },
    },
  },
  plugins: [],
};

export default config;
