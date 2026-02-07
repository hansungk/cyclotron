#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

mkdir -p logs
failures=0

if [ ! -x ./target/release/cyclotron ]; then
    cargo build --release
fi

shopt -s nullglob
isa_tests=(test/isa-tests/*)
if [ ${#isa_tests[@]} -eq 0 ]; then
    echo "no isa tests found under test/isa-tests"
    exit 1
fi

for isatest in "${isa_tests[@]}"; do
    name=$(basename "$isatest")
    log="logs/${name}.out"
    echo "Running $name ..."

    if ! RUST_LOG=debug ./target/release/cyclotron isa.toml --binary-path "$isatest" > "$log" 2>&1; then
        echo "::error [fail] $isatest"
        failures=$((failures+1))
    else
        echo "[pass] $isatest"
    fi
done

if [ $failures -ne 0 ]; then
    echo "$failures isa tests failed"
    exit 1
else
    echo "all isa tests passed"
fi
