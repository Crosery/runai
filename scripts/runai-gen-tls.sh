#!/usr/bin/env bash
# runai-gen-tls.sh — generate a local CA + server cert + client trust chain
# for a self-deployed runai team server.
#
# PLANNING.md §2.3 item 2 ("强制 HTTPS"): team mode bound to a non-loopback
# host requires TLS material. This script produces the minimal usable set:
#
#   <OUTDIR>/
#     ca.key      — local CA private key (KEEP SECRET, never ship)
#     ca.crt      — local CA certificate (ship to every client as trust anchor)
#     server.key  — server private key (lives next to the runai binary)
#     server.crt  — server certificate, signed by ca.crt (--tls-cert path)
#     fingerprint — SHA-256 hex of server.crt's leaf cert (what /api/tls/fingerprint returns)
#
# Usage:
#   scripts/runai-gen-tls.sh                # writes to ./tls/, CN=runai.local, SAN=DNS:runai.local + IP:127.0.0.1
#   scripts/runai-gen-tls.sh --out /etc/runai-tls --host 10.0.0.5 --days 730
#
# After running:
#   runai server --mode team --host 10.0.0.5 --port 17888 \
#     --tls-cert /etc/runai-tls/server.crt --tls-key /etc/runai-tls/server.key
#
# Then on every client:
#   curl --cacert ca.crt https://runai.local:17888/api/tls/fingerprint
#
# Dependencies: openssl 1.1+ (already on macOS, every modern Linux distro,
# and Git for Windows). No runai binary needed to run this script.

set -euo pipefail

OUTDIR="./tls"
SERVER_HOST="runai.local"
DAYS=825   # CA/Browser Forum max for leaf certs is 825 days; safe default
EXTRA_IPS=()
EXTRA_DNS=()

usage() {
  cat <<'USAGE'
runai-gen-tls.sh — generate a local CA + server cert + fingerprint for
                   a self-deployed runai team server (PLANNING §2.3 item 2).

Usage:
  scripts/runai-gen-tls.sh [--out DIR] [--host HOSTNAME] [--days N]
                           [--ip IP]... [--dns NAME]...

Options:
  --out DIR     Output directory (default: ./tls). Created if missing.
  --host NAME   Primary CN + first SAN DNS entry (default: runai.local).
  --days N      Server cert validity in days (default: 825).
  --ip IP       Additional SAN IP entry (repeatable). 127.0.0.1 is always
                included for local healthchecks.
  --dns NAME    Additional SAN DNS entry (repeatable).
  -h, --help    Print this message.

Outputs in <OUTDIR>:
  ca.key        CA private key (keep secret; do NOT ship to clients)
  ca.crt        CA certificate (ship to every client as trust anchor)
  server.key    Server private key (--tls-key)
  server.crt    Server certificate (--tls-cert)
  fingerprint   SHA-256 hex of server.crt's leaf, matching /api/tls/fingerprint
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUTDIR="$2"; shift 2 ;;
    --host) SERVER_HOST="$2"; shift 2 ;;
    --days) DAYS="$2"; shift 2 ;;
    --ip) EXTRA_IPS+=("$2"); shift 2 ;;
    --dns) EXTRA_DNS+=("$2"); shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "runai-gen-tls: unknown arg: $1" >&2; usage >&2; exit 1 ;;
  esac
done

if ! command -v openssl >/dev/null 2>&1; then
  echo "runai-gen-tls: openssl not found in PATH; install openssl 1.1+ and retry" >&2
  exit 1
fi

mkdir -p "$OUTDIR"
cd "$OUTDIR"

CA_KEY="ca.key"
CA_CRT="ca.crt"
SERVER_KEY="server.key"
SERVER_CRT="server.crt"
SERVER_CSR="server.csr"
SERVER_EXT="server.ext"
FINGERPRINT_FILE="fingerprint"

echo "==> writing material to $(pwd)"

# 1) Local CA (10y validity — re-running this script regenerates everything,
#    but the CA is what clients pin, so we want it stable enough that one
#    cert rotation doesn't force a re-install on every client).
if [[ ! -f "$CA_KEY" ]]; then
  echo "==> generating local CA key"
  openssl genrsa -out "$CA_KEY" 4096 2>/dev/null
  chmod 600 "$CA_KEY"
fi
if [[ ! -f "$CA_CRT" ]]; then
  echo "==> generating local CA cert (10y validity)"
  openssl req -x509 -new -nodes -key "$CA_KEY" -sha256 -days 3650 \
    -out "$CA_CRT" \
    -subj "/CN=runai-local-CA" 2>/dev/null
fi

# 2) Server key + CSR.
echo "==> generating server key"
openssl genrsa -out "$SERVER_KEY" 2048 2>/dev/null
chmod 600 "$SERVER_KEY"

echo "==> generating server CSR (CN=$SERVER_HOST)"
openssl req -new -key "$SERVER_KEY" \
  -out "$SERVER_CSR" \
  -subj "/CN=$SERVER_HOST" 2>/dev/null

# 3) SAN extension file (modern TLS clients ignore CN, only SAN matters).
{
  echo "authorityKeyIdentifier=keyid,issuer"
  echo "basicConstraints=CA:FALSE"
  echo "keyUsage=digitalSignature, keyEncipherment"
  echo "extendedKeyUsage=serverAuth"
  echo "subjectAltName=@alt_names"
  echo ""
  echo "[alt_names]"
  # Primary DNS entry from --host
  echo "DNS.1 = $SERVER_HOST"
  i=2
  for d in "${EXTRA_DNS[@]:-}"; do
    [[ -z "$d" ]] && continue
    echo "DNS.$i = $d"
    i=$((i+1))
  done
  # 127.0.0.1 is always present so a local healthcheck can hit `https://127.0.0.1`.
  echo "IP.1 = 127.0.0.1"
  i=2
  for ip in "${EXTRA_IPS[@]:-}"; do
    [[ -z "$ip" ]] && continue
    echo "IP.$i = $ip"
    i=$((i+1))
  done
} > "$SERVER_EXT"

# 4) Sign the CSR with the local CA.
echo "==> signing server cert ($DAYS days)"
openssl x509 -req -in "$SERVER_CSR" \
  -CA "$CA_CRT" -CAkey "$CA_KEY" -CAcreateserial \
  -out "$SERVER_CRT" -days "$DAYS" -sha256 \
  -extfile "$SERVER_EXT" 2>/dev/null

# 5) Fingerprint — matches what `/api/tls/fingerprint` will return at runtime.
#    OpenSSL prints "SHA256 Fingerprint=AB:CD:..." with colons + uppercase;
#    runai serves lowercase + no separators. We normalize.
FP_RAW=$(openssl x509 -in "$SERVER_CRT" -noout -fingerprint -sha256 \
  | sed -e 's/^.*Fingerprint=//' -e 's/://g' \
  | tr 'A-Z' 'a-z')
printf '%s\n' "$FP_RAW" > "$FINGERPRINT_FILE"

# 6) Hand-off summary.
cat <<EOF

==================================================================
runai TLS material written to: $(pwd)

  ca.crt          ship this to every client (the trust anchor)
  ca.key          KEEP SECRET — never commit, never ship to clients
  server.crt      pass to runai via --tls-cert
  server.key      pass to runai via --tls-key
  fingerprint     SHA-256 of server.crt leaf — matches /api/tls/fingerprint

Server bring-up:
  runai server --mode team --host $SERVER_HOST --port 17888 \\
    --tls-cert $(pwd)/server.crt --tls-key $(pwd)/server.key

Client install (each teammate):
  curl --cacert $(pwd)/ca.crt https://$SERVER_HOST:17888/install | bash

Leaf SHA-256 fingerprint:
  $FP_RAW
==================================================================
EOF
