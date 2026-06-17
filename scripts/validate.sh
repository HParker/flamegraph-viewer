#!/usr/bin/env bash
# Run the test suite and, when the real sample profiles are present, validate
# that each supported format parses.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "== cargo test =="
cargo test

echo
echo "== building validate =="
cargo build --quiet --bin validate

# Real-world profiles are large and not committed; validate them when available.
samples=(perf.json out.perf app-dev-boot-vernier.json)
ran_any=0
for sample in "${samples[@]}"; do
    if [[ -f "$sample" ]]; then
        echo
        echo "== validate $sample =="
        ./target/debug/validate "$sample"
        ran_any=1
    fi
done

if [[ "$ran_any" -eq 0 ]]; then
    echo
    echo "(no real sample profiles found; skipped real-file validation)"
fi
