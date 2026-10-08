#!/usr/bin/env python3
"""Collects benchmark results and fails on regressions (M0-07).

Usage:
  scripts/bench-compare.py collect [--criterion DIR] [--app JSON] --out FILE
  scripts/bench-compare.py compare BASELINE CURRENT [--threshold 0.10]

`collect` merges criterion medians (ns) and app metrics into one flat JSON
object: {"<metric>": value, ...}. All metrics are "lower is better".

`compare` exits 1 if any metric present in both files regressed by more than
the threshold. Missing baseline (first run) is not an error.
"""

import argparse
import json
import sys
from pathlib import Path


def collect_criterion(root: Path) -> dict[str, float]:
    out: dict[str, float] = {}
    if not root.is_dir():
        return out
    for est in sorted(root.glob("**/new/estimates.json")):
        meta = est.parent / "benchmark.json"
        if not meta.is_file():
            continue
        full_id = json.loads(meta.read_text())["full_id"]
        median = json.loads(est.read_text())["median"]["point_estimate"]
        out[f"criterion/{full_id}_ns"] = float(median)
    return out


def collect_app(path: Path | None) -> dict[str, float]:
    if path is None or not path.is_file():
        return {}
    data = json.loads(path.read_text())
    return {
        f"app/{k}": float(v)
        for k, v in data.items()
        if k in ("startup_ms", "footprint_mb") and v is not None
    }


def cmd_collect(args: argparse.Namespace) -> int:
    metrics = collect_criterion(Path(args.criterion))
    metrics.update(collect_app(Path(args.app) if args.app else None))
    if not metrics:
        print("no benchmark results found", file=sys.stderr)
        return 1
    Path(args.out).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out).write_text(json.dumps(metrics, indent=2, sort_keys=True) + "\n")
    print(f"collected {len(metrics)} metrics -> {args.out}")
    return 0


def cmd_compare(args: argparse.Namespace) -> int:
    base_path, cur_path = Path(args.baseline), Path(args.current)
    current = json.loads(cur_path.read_text())
    if not base_path.is_file():
        print(f"no baseline at {base_path}; skipping comparison")
        return 0
    baseline = json.loads(base_path.read_text())

    regressions = []
    print(f"{'metric':<52} {'baseline':>14} {'current':>14} {'delta':>8}")
    for key in sorted(current):
        cur = current[key]
        base = baseline.get(key)
        if base is None or base <= 0:
            print(f"{key:<52} {'-':>14} {cur:>14.2f} {'new':>8}")
            continue
        delta = (cur - base) / base
        flag = ""
        if delta > args.threshold:
            flag = "  REGRESSION"
            regressions.append(key)
        print(f"{key:<52} {base:>14.2f} {cur:>14.2f} {delta:>+7.1%}{flag}")

    if regressions:
        print(
            f"\n{len(regressions)} metric(s) regressed > {args.threshold:.0%}: "
            + ", ".join(regressions),
            file=sys.stderr,
        )
        return 1
    print(f"\nno regressions > {args.threshold:.0%}")
    return 0


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)

    c = sub.add_parser("collect")
    c.add_argument("--criterion", default="target/criterion")
    c.add_argument("--app", default=None)
    c.add_argument("--out", required=True)
    c.set_defaults(func=cmd_collect)

    m = sub.add_parser("compare")
    m.add_argument("baseline")
    m.add_argument("current")
    m.add_argument("--threshold", type=float, default=0.10)
    m.set_defaults(func=cmd_compare)

    args = p.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
