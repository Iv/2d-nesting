#!/usr/bin/env python3
"""Ranks the runs of a seed sweep by fabric used (sum of the used sheet lengths).

    python3 scripts/sweep_summary.py runs/sweep_<job id> [top N]
"""
import pathlib
import re
import sys


def parse(report: str):
    fabric = re.search(r"Fabric used:\s+([\d.]+) m", report)
    util = re.search(r"Utilisation:\s+([\d.]+)% of the used fabric", report)
    sheets = re.search(r"Sheets:\s+(\d+)", report)
    ok = "validation FAILED" not in report
    if not (fabric and util and sheets):
        return None
    return float(fabric.group(1)), float(util.group(1)), int(sheets.group(1)), ok


def main():
    root = pathlib.Path(sys.argv[1])
    top = int(sys.argv[2]) if len(sys.argv) > 2 else 10
    rows = []
    for rep in sorted(root.glob("*/report.txt")):
        r = parse(rep.read_text())
        if r:
            rows.append((r, rep.parent))
    if not rows:
        sys.exit(f"no finished runs under {root}")
    rows.sort(key=lambda x: (not x[0][3], x[0][0]))
    print(f"{len(rows)} runs, best {top}:")
    print(f"{'fabric, m':>10} {'util, %':>8} {'sheets':>6}  valid  run")
    for (fabric, util, sheets, ok), path in rows[:top]:
        print(f"{fabric:10.3f} {util:8.2f} {sheets:6d}  {'yes' if ok else 'NO ':5}  {path}")
    fabrics = [r[0][0] for r in rows if r[0][3]]
    print(f"valid runs: {len(fabrics)}, fabric min {min(fabrics):.3f} / median {sorted(fabrics)[len(fabrics)//2]:.3f} / max {max(fabrics):.3f} m")


if __name__ == "__main__":
    main()
