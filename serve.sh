#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PORT="${1:-8000}"

port_in_use() {
  local port="$1"

  if command -v lsof >/dev/null 2>&1; then
    lsof -iTCP:"${port}" -sTCP:LISTEN -t >/dev/null 2>&1
  elif command -v ss >/dev/null 2>&1; then
    ss -ltn "sport = :${port}" 2>/dev/null | tail -n +2 | grep -q .
  elif command -v fuser >/dev/null 2>&1; then
    fuser "${port}/tcp" >/dev/null 2>&1
  else
    python3 - "${port}" <<'PY'
import socket
import sys

port = int(sys.argv[1])
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
try:
  sock.bind(("127.0.0.1", port))
  in_use = False
except OSError:
  in_use = True
finally:
  sock.close()

sys.exit(0 if in_use else 1)
PY
  fi
}

START_PORT="${PORT}"
while port_in_use "${PORT}"; do
  PORT="$((PORT + 1))"
done

if [[ "${PORT}" != "${START_PORT}" ]]; then
  echo "Port ${START_PORT} is in use; using ${PORT} instead."
fi

echo "Serving ${ROOT_DIR} on http://localhost:${PORT}"
python3 -m http.server "${PORT}" --bind 127.0.0.1 --directory "${ROOT_DIR}"
