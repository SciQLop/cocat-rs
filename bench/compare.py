#!/usr/bin/env python3
"""Print the spike gate table from bench/results, Rust next to Python.

Thresholds are the design's (docs/superpowers/specs/2026-11-05-cocat-rs-design.md,
phase 0), plus its hard NO-GO lines. A row run while the harness itself was busy
(harness_peak_core_fraction > 0.7) is listed as harness-bound: its number is not
the server's.
"""

import argparse
import json
from dataclasses import dataclass
from pathlib import Path

RESULTS = Path(__file__).resolve().parent / "results"
HARNESS_BOUND = 0.7
SCENARIOS = ("open", "openclose", "fanout", "scaleout", "soak")
SERVERS = ("rust", "python")


@dataclass(frozen=True)
class Gate:
    name: str
    scenario: str
    key: str
    limit: float
    unit: str
    lower_is_better: bool
    kind: str = "metric"
    base_unit: str = ""


GATES = (
    Gate("Room open, cold, p95", "open", "open_p95_ms", 1500, "ms", True),
    Gate("Fan-out p50, 50 clients", "fanout", "fanout_p50_ms", 10, "ms", True),
    Gate("Fan-out p99, 50 clients", "fanout", "fanout_p99_ms", 50, "ms", True),
    Gate("RSS, one open room", "open", "rss_open_mb", 600, "MB", True),
    Gate(
        "CPU, same fan-out run", "fanout", "cpu_seconds", 0.6, "x", True, "python", "s"
    ),
    Gate(
        "Scale-out vs single room",
        "scaleout",
        "updates_per_s",
        4.0,
        "x",
        False,
        "single",
        "updates/s",
    ),
    Gate(
        "Scale-out vs Python",
        "scaleout",
        "updates_per_s",
        1.5,
        "x",
        False,
        "python",
        "updates/s",
    ),
    Gate("Soak RSS drift, minute 5 to end", "soak", "rss_drift_pct", 5, "%", True),
    Gate(
        "Soak lost updates after restart",
        "soak",
        "lost_updates_after_restart",
        0,
        "updates",
        True,
    ),
    Gate("Open/close x20 RSS drift", "openclose", "rss_drift_pct", 5, "%", True),
)


def load(prefix: str, server: str) -> dict:
    loaded = {}
    for scenario in SCENARIOS:
        path = RESULTS / f"{prefix}{scenario}-{server}.json"
        if path.exists():
            loaded[scenario] = json.loads(path.read_text())
    return loaded


def ratio(numerator: float | None, denominator: float | None) -> float | None:
    if not numerator or not denominator:
        return None
    return round(numerator / denominator, 3)


def cell(value: float | None, unit: str) -> str:
    return "-" if value is None else f"{value:g} {unit}"


def verdict(quantity: float | None, gate: Gate) -> str:
    if quantity is None:
        return "missing"
    passes = quantity <= gate.limit if gate.lower_is_better else quantity >= gate.limit
    return "GO" if passes else "NO-GO"


def evaluate(gate: Gate, rust: dict, python: dict) -> tuple[str, str, str]:
    rust_run = rust.get(gate.scenario) or {}
    python_run = python.get(gate.scenario) or {}
    base = (
        python_run.get(gate.key)
        if gate.kind == "python"
        else rust_run.get("single_room_updates_per_s")
    )
    if gate.kind == "metric":
        quantity = rust_run.get(gate.key)
    else:
        quantity = ratio(rust_run.get(gate.key), base)
    return (
        cell(quantity, gate.unit),
        "-"
        if gate.kind == "single"
        else cell(python_run.get(gate.key), gate.base_unit or gate.unit),
        verdict(quantity, gate),
    )


def rule(gate: Gate) -> str:
    sign = "<=" if gate.lower_is_better else ">="
    return f"{sign} {gate.limit:g} {gate.unit}"


def hard_lines(rust: dict) -> list[str]:
    fanout = rust.get("fanout") or {}
    soak = rust.get("soak") or {}
    openclose = rust.get("openclose") or {}
    lost = (soak.get("lost_updates") or 0) + (
        soak.get("lost_updates_after_restart") or 0
    )
    drift = max(
        soak.get("rss_drift_pct") or 0,
        openclose.get("rss_drift_pct") or 0,
    )
    rss = (rust.get("open") or {}).get("rss_peak_mb") or 0
    crossed = []
    if (fanout.get("fanout_p99_ms") or 0) > 200:
        crossed.append(f"fan-out p99 {fanout['fanout_p99_ms']:g} ms > 200 ms")
    if rss > 640:
        crossed.append(f"RSS {rss:g} MB > 640 MB")
    if lost:
        crossed.append(f"{lost} lost updates")
    if drift > 10:
        crossed.append(f"RSS drift {drift:g} % > 10 %")
    return crossed


def harness_bound(prefix: str) -> list[str]:
    bound = []
    for server in SERVERS:
        for scenario, run in load(prefix, server).items():
            fraction = run.get("harness_peak_core_fraction")
            if fraction is not None and fraction > HARNESS_BOUND:
                bound.append(f"{scenario} ({server}): {fraction:g} cores")
    return bound


def table(prefix: str) -> str:
    rust = load(prefix, "rust")
    python = load(prefix, "python")
    lines = [
        "| Gate | GO | Rust | Python | Verdict |",
        "| --- | --- | --- | --- | --- |",
    ]
    go = True
    for gate in GATES:
        rust_cell, python_cell, result = evaluate(gate, rust, python)
        go = go and result == "GO"
        lines.append(
            f"| {gate.name} | {rule(gate)} | {rust_cell} | {python_cell} | {result} |"
        )
    hard = hard_lines(rust)
    go = go and not hard
    lines += [
        "",
        "Hard NO-GO lines crossed: " + ("; ".join(hard) or "none"),
        "Harness-bound rows: " + ("; ".join(harness_bound(prefix)) or "none"),
        "Overall: " + ("GO" if go else "NO-GO"),
    ]
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--smoke", action="store_true", help="smoke results")
    arguments = parser.parse_args()
    print(table("smoke-" if arguments.smoke else ""))


if __name__ == "__main__":
    main()
