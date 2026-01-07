#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PORT="${1:-8000}"

echo "Serving ${ROOT_DIR} on http://localhost:${PORT}"
python3 -m http.server "${PORT}" --bind 127.0.0.1 --directory "${ROOT_DIR}"
