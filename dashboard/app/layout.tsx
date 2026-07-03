import type { Metadata } from "next";
import { JetBrains_Mono } from "next/font/google";
import "./globals.css";
import { WsProvider } from "@/components/WsProvider";
import { Toaster } from "@/components/Toaster";

// Typography. next/font/google downloads JetBrains Mono at BUILD time and self-hosts
// the files in the bundle — there are no Google Fonts requests at runtime, so the
// dashboard still functions on an isolated machine. All numeric data and status badges
// use this family via the --font-mono CSS variable.
const jetbrainsMono = JetBrains_Mono({
  subsets: ["latin"],
  weight: ["400", "500", "700"],
  variable: "--font-mono",
  display: "swap",
});

export const metadata: Metadata = {
  title: "Kestrel — MEV Dashboard",
  description: "Yield-bearing stablecoin arbitrage monitor",
  robots: "noindex, nofollow", // Never indexed — private operational tool
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en" className="dark">
      <body
        className={`${jetbrainsMono.variable} font-sans bg-bg-base text-text-primary antialiased min-h-screen`}
      >
        {/* WebSocket connection provider — mounts once, feeds Zustand store */}
        <WsProvider />
        {children}
        <Toaster />
      </body>
    </html>
  );
}
