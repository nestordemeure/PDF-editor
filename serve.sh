#!/usr/bin/env bash
# Serve the app locally: ./serve.sh [port]  (default 8000; bumps to the next
# free port if taken). python3 is required — it also runs the server.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REQUESTED_PORT="${1:-8000}"

PORT="$(python3 - "${REQUESTED_PORT}" <<'PY'
import socket, sys
port = int(sys.argv[1])
while True:
    try:
        with socket.socket() as s:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            s.bind(("127.0.0.1", port))
        break
    except OSError:
        port += 1
print(port)
PY
)"

if [[ "${PORT}" != "${REQUESTED_PORT}" ]]; then
  echo "Port ${REQUESTED_PORT} is in use; using ${PORT} instead."
fi

echo "Serving ${ROOT_DIR} on http://localhost:${PORT}"
# no-cache so browsers revalidate on every load — otherwise Chrome can mix
# stale cached modules with fresh ones after the code changes
python3 - "${PORT}" "${ROOT_DIR}" <<'PY'
import sys
from http.server import ThreadingHTTPServer, SimpleHTTPRequestHandler

port, root = int(sys.argv[1]), sys.argv[2]

class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=root, **kwargs)

    def end_headers(self):
        self.send_header("Cache-Control", "no-cache")
        super().end_headers()

ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
PY
