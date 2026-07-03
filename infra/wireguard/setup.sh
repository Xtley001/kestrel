#!/usr/bin/env bash
# infra/wireguard/setup.sh
# Server firewall + WireGuard setup.
# Run once on the co-location server (Equinix NY5 or equivalent).
# Requires root. Test on a staging instance first.

set -euo pipefail

echo "=== Kestrel server hardening ==="

# ── Step 1: System update ────────────────────────────────────────────────────
apt-get update -q
apt-get install -y -q wireguard ufw fail2ban

# ── Step 2: UFW firewall rules ───────────────────────────────────────────────
# Reset to defaults
ufw --force reset

# Default deny all inbound/outbound
ufw default deny incoming
ufw default allow outgoing

# Allow SSH (change port if using non-standard SSH)
ufw allow 22/tcp

# Allow WireGuard — ONLY external port exposed
ufw allow 51820/udp

# IMPORTANT: dashboard (3000), Prometheus (9090), bot WS (9101, 9102)
# are bound to 127.0.0.1 only — NOT opened in firewall.

ufw --force enable
echo "UFW rules applied"

# ── Step 3: Enable IP forwarding ────────────────────────────────────────────
grep -q "net.ipv4.ip_forward=1" /etc/sysctl.conf || \
    echo "net.ipv4.ip_forward=1" >> /etc/sysctl.conf
sysctl -p

# ── Step 4: WireGuard key generation ────────────────────────────────────────
if [ ! -f /etc/wireguard/server_private.key ]; then
    wg genkey > /etc/wireguard/server_private.key
    wg pubkey < /etc/wireguard/server_private.key > /etc/wireguard/server_public.key
    chmod 600 /etc/wireguard/server_private.key
    echo "WireGuard server keys generated"
    echo "Server public key:"
    cat /etc/wireguard/server_public.key
fi

# ── Step 5: Enable WireGuard service ────────────────────────────────────────
systemctl enable  wg-quick@wg0
# NOTE: Populate /etc/wireguard/wg0.conf first (from wg0.conf.example)
# Then run: systemctl start wg-quick@wg0

# ── Step 6: fail2ban for SSH ─────────────────────────────────────────────────
systemctl enable  fail2ban
systemctl start   fail2ban

echo ""
echo "=== Setup complete ==="
echo "Next steps:"
echo "  1. Populate /etc/wireguard/wg0.conf from infra/wireguard/wg0.conf.example"
echo "  2. Run: systemctl start wg-quick@wg0"
echo "  3. Start bot: cargo run --release -p kestrel"
echo "  4. Start dashboard: cd dashboard && npm start"
echo "  5. Access dashboard via tunnel: http://10.0.0.1:3000"
