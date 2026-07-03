/** @type {import('next').NextConfig} */
const nextConfig = {
  // Dashboard is accessed via WireGuard tunnel - no public exposure.
  // All API routes are server-side only.
  output: "standalone",
};

export default nextConfig;
