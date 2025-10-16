#!/usr/bin/env bash

mkdir -p logs
failures=0

for isatest in test/isa-tests/*; do
    name=$(basename "$isatest")
    log="logs/${name}.out"
    echo "Running $name ..."

    RUST_LOG=debug ./target/debug/cyclotron isa.toml --binary-path "$isatest" > "$log" 2>&1
    if [ $? -ne 0 ]; then
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