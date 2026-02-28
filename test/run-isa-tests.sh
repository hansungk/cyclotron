#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

TIMING_FLAG=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --timing)
            TIMING_FLAG="--timing"
            shift
            ;;
        *)
            echo "Usage: $0 [--timing]"
            exit 2
            ;;
    esac
done

mkdir -p logs
failures=0
passes=0

cargo build --release >/dev/null

for isatest in test/isa-tests/*; do
    test -x "$isatest" || continue
    name=$(basename "$isatest")
    log="logs/${name}.out"
    echo "Running $name ..."

    if ! RUST_LOG=debug ./target/release/cyclotron isa.toml --binary-path "$isatest" ${TIMING_FLAG:+$TIMING_FLAG} > "$log" 2>&1; then
        echo "::error [fail] $isatest"
        failures=$((failures+1))
    else
        echo "[pass] $isatest"
        passes=$((passes+1))
    fi
done

if [ $failures -ne 0 ]; then
    echo "isa summary: ${passes} passed, ${failures} failed"
    exit 1
else
    echo "isa summary: ${passes} passed, ${failures} failed"
fi
