#!/usr/bin/env bash
# Run every spike gate scenario for one server, strictly sequentially, into
# bench/results/<scenario>-<server>.json.
#
# Usage: SERVER=rust|python bench/run_gates.sh [--smoke]
#
# SCENARIOS selects a subset, e.g. SCENARIOS="open openclose" (default: all five,
# in gate order); SERVER_CMD overrides the server command line, e.g. to run it under
# `env MALLOC_ARENA_MAX=2`. The pristine room set (bench100k, bench1k, bench1k-0..7) is
# built once into bench/work/; every scenario gets a fresh copy of it and a freshly
# seeded database.
set -euo pipefail

bench=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(dirname -- "$bench")
work="$bench/work"
results="$bench/results"
python_bin=${PYTHON:-/home/jeandet/Documents/prog/cocat-python-tests/.venv/bin/python}
python_cli=/home/jeandet/Documents/prog/cocat-python-tests/.venv/bin/cocat
rust_cli="$repo/target/release/cocat-server"
email=bench@test.org
password=pwd
rooms=(bench100k bench1k bench1k-0 bench1k-1 bench1k-2 bench1k-3 bench1k-4 bench1k-5 bench1k-6 bench1k-7)
scenarios=(${SCENARIOS:-open openclose fanout scaleout soak})
smoke=0
server=${SERVER:-}

for argument in "$@"; do
    case "$argument" in
        --smoke) smoke=1 ;;
        python | rust) server=$argument ;;
        *)
            echo "usage: SERVER=python|rust $0 [--smoke]" >&2
            exit 2
            ;;
    esac
done

case "$server" in
    python) cli=$python_cli; serve_command="$python_cli serve" ;;
    rust) cli=$rust_cli; serve_command="$rust_cli serve" ;;
    *)
        echo "set SERVER to python or rust" >&2
        exit 2
        ;;
esac
if [[ ! -x "$cli" ]]; then
    echo "$cli is missing (cargo build --release -p cocat-server for rust)" >&2
    exit 1
fi
serve_command=${SERVER_CMD:-$serve_command}
if [[ ! -x "$python_bin" ]]; then
    echo "$python_bin is missing (set PYTHON to a venv with cocat, pycrdt, websockets)" >&2
    exit 1
fi

prefix=
[[ $smoke == 1 ]] && prefix=smoke-
pristine="$work/${prefix}pristine"

build_pristine() {
    local big=(--catalogues 100 --events 1000) small=(--catalogues 10 --events 100)
    if [[ $smoke == 1 ]]; then
        big=(--catalogues 2 --events 20)
        small=("${big[@]}")
    fi
    "$python_bin" "$bench/make_room.py" --dir "$pristine" --room bench100k "${big[@]}"
    "$python_bin" "$bench/make_room.py" --dir "$pristine" --room bench1k "${small[@]}"
    for index in 0 1 2 3 4 5 6 7; do
        "$python_bin" "$bench/make_room.py" --dir "$pristine" --room "bench1k-$index" \
            "${small[@]}"
    done
}

if [[ ! -f "$pristine/bench1k-7.y" ]]; then
    mkdir -p "$pristine"
    build_pristine
fi

seed() {
    local db=$1 room
    rm -f "$db"
    "$cli" create-user --email "$email" --password "$password" --db_path "$db" >/dev/null
    for room in "${rooms[@]}"; do
        "$cli" create-room --owner "$email" --room-id "$room" --db_path "$db" >/dev/null
    done
}

run_scenario() {
    local scenario=$1
    local dir="$work/$prefix$scenario"
    local extra=()
    [[ $smoke == 1 ]] && extra+=(--smoke)
    rm -rf "$dir"
    mkdir -p "$dir/updates"
    cp "$pristine"/*.y "$dir/updates/"
    seed "$dir/bench.db"
    "$python_bin" "$bench/harness.py" "$scenario" \
        --server-cmd "$serve_command" \
        --db "$dir/bench.db" \
        --update-dir "$dir/updates" \
        --email "$email" \
        --password "$password" \
        --json "$results/$prefix$scenario-$server.json" \
        "${extra[@]}"
}

mkdir -p "$results"
for scenario in "${scenarios[@]}"; do
    echo "== $scenario ($server)"
    run_scenario "$scenario"
done
