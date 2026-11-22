#!/usr/bin/env python3
"""Generate a Taunicorn-only benchmark PNG and update the README section."""

from __future__ import annotations

import json
import platform
import sys
from datetime import datetime, timezone
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

START = "<!-- TAUNICORN_BENCHMARK_START -->"
END = "<!-- TAUNICORN_BENCHMARK_END -->"
PLOT_PATH = Path("docs/benchmarks/taunicorn.png")
README_PATH = Path("README.md")

# The names are the pytest test function names, not abbreviated labels.
TESTS = (
    ("test_benchmark_send", "Send"),
    ("test_benchmark_receive", "Receive"),
    ("test_benchmark_roundtrip", "Roundtrip"),
)


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: update_benchmark_readme.py <benchmark-json>")

    raw = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    benchmarks = raw.get("benchmarks", [])
    by_name = {record["name"]: record for record in benchmarks}
    missing = [name for name, _ in TESTS if name not in by_name]
    if missing:
        raise SystemExit(f"Missing required Taunicorn benchmarks: {missing}")

    entries = []
    for test_name, label in TESTS:
        record = by_name[test_name]
        stats = record["stats"]
        median_us = float(stats["median"]) * 1_000_000
        mean_us = float(stats["mean"]) * 1_000_000
        min_us = float(stats["min"]) * 1_000_000
        max_us = float(stats["max"]) * 1_000_000
        if median_us <= 0:
            raise SystemExit(f"Invalid median for {test_name}")
        entries.append((label, median_us, mean_us, min_us, max_us))

    PLOT_PATH.parent.mkdir(parents=True, exist_ok=True)
    labels = [e[0] for e in entries]
    medians = [e[1] for e in entries]
    fig, ax = plt.subplots(figsize=(9, 3.8))
    bars = ax.barh(labels, medians)
    ax.invert_yaxis()
    ax.set_xlabel("Median latency (µs) · lower is better")
    ax.set_title("Taunicorn · 1 KiB · Linux · Python API")
    ax.grid(axis="x", alpha=0.2)
    ax.set_axisbelow(True)
    ax.set_xlim(0, max(medians) * 1.27)
    for bar, median in zip(bars, medians):
        ax.text(
            bar.get_width() + max(medians) * 0.025,
            bar.get_y() + bar.get_height() / 2,
            f"{median:,.1f} µs",
            va="center",
        )
    fig.tight_layout()
    fig.savefig(PLOT_PATH, dpi=180)
    plt.close(fig)

    measured_at = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M UTC")
    rows = "\n".join(
        f"| {label} | {median:,.1f} | {mean:,.1f} | {minimum:,.1f} | {maximum:,.1f} |"
        for label, median, mean, minimum, maximum in entries
    )
    section = f"""{START}
## Taunicorn Benchmarks

![Taunicorn IPC benchmark latency](docs/benchmarks/taunicorn.png)

| Operation (1 KiB) | Median (µs) | Mean (µs) | Min (µs) | Max (µs) |
|---|---:|---:|---:|---:|
{rows}

Measured: **{measured_at}** · **Ubuntu 24.04** · **Python {platform.python_version()}** · **1,000 rounds per operation**.

> Python-side latency including PyO3/Tokio and `asyncio.Runner.run()`; not isolated IPC kernel latency. Results vary with CI runner load. Only Taunicorn is benchmarked.
{END}"""

    original = README_PATH.read_text(encoding="utf-8")
    if START in original or END in original:
        if original.count(START) != 1 or original.count(END) != 1:
            raise SystemExit("README benchmark markers must occur exactly once each")
        before, remainder = original.split(START, 1)
        _, after = remainder.split(END, 1)
        if before.endswith("\n\n"):
            separator = ""
        elif before.endswith("\n"):
            separator = "\n"
        else:
            separator = "\n\n"
        updated = before + separator + section + after
    else:
        updated = original.rstrip() + "\n\n" + section + "\n"
    README_PATH.write_text(updated, encoding="utf-8")
    print(f"Updated {README_PATH} and {PLOT_PATH}")


if __name__ == "__main__":
    main()
