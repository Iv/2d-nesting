#!/usr/bin/env python3
"""Ranks the runs of a seed sweep by fabric used (sum of the used sheet lengths).

    python3 scripts/sweep_summary.py runs/sweep_<job id> [top N]
"""
import pathlib
import re
import sys


def parse(report: str):
    # Fabric consumed (free strips on top not counted) if the report has it, else the used length.
    fabric = re.search(r"Fabric consumed:\s+([\d.]+) m", report) or re.search(r"Fabric used:\s+([\d.]+) m", report)
    util = re.search(r"Utilisation:\s+([\d.]+)% of the used fabric", report)
    sheets = re.search(r"Sheets:\s+(\d+)", report)
    ok = "validation FAILED" not in report
    if not (fabric and util and sheets):
        return None
    # Panel tilt, summed over sheets: "tilted/panels max°" (empty if there are no panels).
    tilts = re.findall(r"Panels tilted:\s+(\d+) of (\d+), max (\d+)°", report)
    tilt = f"{sum(int(a) for a, _, _ in tilts)}/{sum(int(b) for _, b, _ in tilts)} {max(int(m) for _, _, m in tilts)}°" if tilts else ""
    return float(fabric.group(1)), float(util.group(1)), int(sheets.group(1)), ok, tilt


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
    # Fewer sheets (re-spreads) first, then less fabric.
    rows.sort(key=lambda x: (not x[0][3], x[0][2], x[0][0]))
    print(f"{len(rows)} runs, best {top}:")
    print(f"{'fabric, m':>10} {'util, %':>8} {'sheets':>6}  valid  {'tilted':>9}  run")
    for (fabric, util, sheets, ok, tilt), path in rows[:top]:
        print(f"{fabric:10.3f} {util:8.2f} {sheets:6d}  {'yes' if ok else 'NO ':5}  {tilt:>9}  {path}")
    fabrics = [r[0][0] for r in rows if r[0][3]]
    print(f"valid runs: {len(fabrics)}, fabric min {min(fabrics):.3f} / median {sorted(fabrics)[len(fabrics)//2]:.3f} / max {max(fabrics):.3f} m")


if __name__ == "__main__":
    main()
