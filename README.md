# 2d-nesting

Command-line nesting of flat parts (pattern pieces, paraglider ribs and panels, …) onto
fixed-size sheets of fabric. DXF in, DXF out. Written in Rust; the packing engine is
[sparrow](https://github.com/JeroenGar/sparrow) on top of
[jagua-rs](https://github.com/JeroenGar/jagua-rs).

- One part per DXF file, quantities from a small manifest.
- Parts are nested by their **outer contour**; everything inside (holes, cut-outs, marks,
  open lines) travels with the part into the output.
- Fixed sheets, e.g. 1560 × 5800 mm (the cutting table): sheets are filled one after another.
- Allowed rotations: base angles (default 0/90/180/270) plus an optional ± tolerance.
- Gap between parts. No gap is added at the sheet edge: set the edge margin through the sheet size.
- Every layout is checked independently of the solver (no overlaps, inside the sheet, gap kept).
- Reproducible mode: a fixed seed plus an iteration budget gives the same layout every time.

## Build

Requires Rust ≥ 1.90 and a checkout of sparrow next to this repository (`../sparrow`), which
is used as a path dependency.

```bash
git clone https://github.com/JeroenGar/sparrow ../sparrow
cargo build --release
```

The binary is `target/release/nest`. It builds on Linux, Windows and macOS.

## Quick start

```bash
# 1. If all parts sit in one assembly DXF, one block per part, split it into part files:
nest extract JUMBO5_38.dxf --blocks 'rib*' --out ribs

# 2. Edit ribs/parts.toml (quantities), then nest:
nest run ribs --out out --layers 0 --gap 3 --rotation-tolerance 10 --rotation-step 2 --time-limit 20 --seed 1
```

Output in `runs/<input>_<date>_<time>/` (or `--out`):

| file | content |
|---|---|
| `sheet_N.dxf` | one DXF per sheet: layer `SHEET` (sheet rectangle), `PARTS` (part geometry after rotation and shift, inner geometry kept), `LABELS` (part name in the part) |
| `sheet_N.svg` | preview of the sheet |
| `index.html` | all previews on one page |
| `report.txt` | the report that is also printed to the console |
| `steps/` | SVG of every intermediate layout (strip, repairs, probes, final sheet) |
| `command.txt` | the command line of the run |

The exit code is 0 on success, 2 if the independent validation found a violation, 1 on errors.

## Input

A folder with one DXF per part; the file name (without extension) is the part name.

Read: `LWPOLYLINE` and `POLYLINE` (with bulges), chains of `LINE`/`ARC`/open polylines, `CIRCLE`,
`ELLIPSE`, `SPLINE`, and all of these inside `INSERT` blocks. Curves are approximated by
polylines within `--tolerance`. `$INSUNITS` is honoured (mm, cm, dm, m, inch, foot, micron);
an unset value is taken as mm with a warning, any other unit is an error.

How a part is assembled:

1. Pieces are joined end to end into chains (`--join-tolerance`).
2. The largest closed contour is the outer contour. If the open pieces, joined with the coarser
   `--close-gaps`, form a larger ring, that ring is the outer contour (a warning says so).
3. Everything else must lie inside the outer contour (marks may stick out by up to 2 mm).
   Contours outside it mean the file holds more than one part, which is an error naming the file
   and layer.

Use `--layers` / `--ignore-layers` to choose which DXF layers make up the part.

### Manifest

`parts.toml` (or `parts.csv`) in the input folder. Without a manifest every part is needed once.

```toml
[parts]
rib1 = 2
rib2 = { qty = 2 }
```

```csv
name,qty
rib1,2
rib2,2
```

A part missing from the manifest gets quantity 1 (with a warning); quantity 0 leaves it out.

## Parameters of `nest run`

| option | default | meaning |
|---|---|---|
| `--width` | 1560 | sheet width, mm (across the fabric) |
| `--length` | 5800 | sheet length, mm (along the fabric) |
| `-g, --gap` | 0 | gap between parts, mm |
| `-r, --rotations` | 0,90,180,270 | base rotation angles, degrees; `0` disables rotation |
| `--rotation-tolerance` | 0 | each base angle ± this, degrees |
| `--rotation-step` | 1 | step within the tolerance, degrees |
| `--strategy` | balanced | how parts are distributed over sheets: `balanced` or `greedy` |
| `-t, --time-limit` | 60 | search time per sheet, s |
| `-i, --iterations` | – | iteration budget per sheet instead of time (reproducible) |
| `-s, --seed` | random | random seed (printed in the report) |
| `-w, --workers` | 3 | solver threads; keep fixed for reproducibility |
| `--tolerance` | 0.2 | arc/spline approximation, mm |
| `--join-tolerance` | 0.05 | joining segment ends into chains, mm |
| `--close-gaps` | 1 | largest gap bridged when assembling the outer contour, mm |
| `--layers` | all | read only these layers |
| `--ignore-layers` | – | skip these layers |
| `--strict-open` | off | any open line is an error, even inside a part |
| `-o, --out` | `runs/<input>_<date>_<time>` | output folder |
| `-v` | – | progress output (`-vv` for solver debug output) |

`nest extract <assembly.dxf> --blocks <patterns> --out <dir>` saves every block whose name matches
(`*`, `?`, case-insensitive) as `<dir>/<block>.dxf` and writes `<dir>/parts.toml` with quantity 1
and the insert count of each block as a comment.

## How sheets are filled

sparrow solves strip packing: a strip of fixed width whose length it minimises. Sheets are filled on
top of that. A sheet need not be filled to its full length, but it is always compacted so its right
edge is straight: fabric beyond a straight cut is reusable and is not counted as waste. The report's
utilisation is parts area / (sheet width × sum of the used lengths).

`--strategy balanced` (default):

1. The sheet count is estimated from the total area.
2. Parts are dealt over the sheets in serpentine order (1..N, N..1, …), largest first, long parts
   and short parts separately. Every sheet gets long and short pieces that pair up along its length
   (ribs nose to tail) plus its share of small parts.
3. Each sheet is nested. If it is too long, the parts reaching past the sheet end are moved to an
   overflow pool and the sheet is nested again. Free area is then filled with overflow parts.
4. Whatever is left goes through the greedy loop below.

`--strategy greedy`:

1. All remaining parts are nested into a strip of the sheet width; if it fits, it is the last sheet.
2. Otherwise the parts lying entirely within the sheet length make a starting sheet, which is grown
   by adding remaining parts one at a time (largest first) while a short run still fits.
3. The sheet is compacted and the rest goes on to the next sheet.

`--time-limit` is the budget per sheet, shared by the runs for that sheet.
Parts that fit the sheet at no allowed angle are reported as unplaced.

## Validation

`validate.rs` checks every sheet with the [geo](https://crates.io/crates/geo) crate, on the
original contours and without any solver code: every part inside the sheet, no two outer contours
overlapping, and at least `--gap` between them (tolerance 0.01 mm for the solver's f32
arithmetic). The result is in the report.

## Reproducibility

With `--time-limit` the search stops on the clock, so two runs with the same seed can differ.
With `--seed N --iterations K` (and the same `--workers`) the search stops after a fixed number of
steps and the clock is not used anywhere, so the layout is identical every time.

## Seed sweeps on a slurm cluster

Results depend on the seed, so many seeds in parallel and the best layout of them is a cheap win.

```bash
mkdir -p runs/slurm
sbatch --array=1-64 scripts/slurm/sweep.sbatch <input dir> --layers 0 -g 3 -t 20
python3 scripts/sweep_summary.py runs/sweep_<job id>
```

Each array task is one run with seed = task id and 3 solver threads; the summary ranks the runs by
fabric used. `examples/layout_stats.rs` measures an existing hand-made layout the same way:

```bash
cargo run --release --example layout_stats -p nest-core -- 1560 0 sheet1.dxf sheet2.dxf
```

## Tests

```bash
cargo test
```

Synthetic DXF files (rectangles, an L-shape from separate lines, a frame with holes, a part made
of lines and arcs, a rounded rectangle with bulges, an ellipse, a spline part, an inch-unit file)
are generated in `crates/nest-core/tests/common`; the tests cover import, the full run, validation,
reproducibility, rotation tolerance and multiple sheets.

In test builds the debug assertions of sparrow and jagua-rs are disabled (see `Cargo.toml`): they
are very slow, and one of them (`weighted loss should never increase`, sparrow `worker.rs`) fires
on long mixed runs. Layout correctness is covered by the independent validation.

## Structure

- `crates/nest-core` — all logic: DXF reading (`dxf_in`), geometry (`geom`), manifest, sparrow
  wrapper (`solver`), validation, DXF/SVG output, report, block extraction. A GUI can be built on
  this crate alone.
- `crates/nest-cli` — the `nest` command (clap).

## Licenses

This project: MIT.

| dependency | license |
|---|---|
| sparrow 0.3.0 (master `6f7b9d3`) | MIT |
| jagua-rs 1.1.0 | MPL-2.0 (used unmodified) |
| dxf 0.6.2 | MIT |
| geo 0.33 | MIT OR Apache-2.0 |
| clap, serde, toml, anyhow, log, env_logger, rand, ctrlc | MIT OR Apache-2.0 |
| csv | Unlicense OR MIT |

Before publishing, the path dependency on `../sparrow` should become a git dependency pinned to a
commit, so the repository builds without a sibling checkout.
