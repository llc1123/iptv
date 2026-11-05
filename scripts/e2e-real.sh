#!/usr/bin/env bash
set -Eeuo pipefail

if [[ "${IPTV_RUN_REAL_E2E:-}" == "0" ]]; then
    unset IPTV_ENABLE_REAL_E2E
elif [[ -n "${IPTV_RUN_REAL_E2E+x}" ]]; then
    export IPTV_ENABLE_REAL_E2E=$IPTV_RUN_REAL_E2E
fi

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
exec "$ROOT/scripts/test-real-e2e.sh" "$@"
