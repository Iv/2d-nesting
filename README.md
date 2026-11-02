# 2d-nesting

Command-line nesting of flat parts (pattern pieces, paraglider ribs and panels, …) onto
fixed-size sheets of fabric. DXF in, DXF out. Written in Rust; the packing engine is
[sparrow](https://github.com/JeroenGar/sparrow) on top of
[jagua-rs](https://github.com/JeroenGar/jagua-rs).

This is not a general-purpose library for nesting anything onto anything. It is a specialised tool
for one job: laying out the large parts of paragliders (ribs, top and bottom panels) on fabric for
laser cutting. What it optimises follows from that job: panels kept along the grain, fabric saved
at the end of a sheet worth more than a free strip along the top, a straight right edge rather
than holes in the middle, left panels as mirrored right ones. Most of these rules are weights and
penalties in a few places (see [How sheets are filled](#how-sheets-are-filled)), so adapting it to
another kind of part with other priorities should not be hard.

On real gliders the parts consistently take 84 % or more of the fabric used, and up to 92 % in
some cases (see the examples below).

Example layouts for a real glider (Tayran 6, size 29), with DXF, SVG and reports:
[`examples/`](examples/README.md).

- One part per DXF file, quantities from a small manifest.
- Parts are nested by their **outer contour**; everything inside (holes, cut-outs, marks,
  open lines) travels with the part into the output.
- Fixed sheets, 1560 × 5800 mm by default (our laser table); any size is set with `--width` and
  `--length`. Sheets are filled one after another.
- Allowed rotations: base angles (default 0/90/180/270) plus an optional ± tolerance.
- Panels, when asked for, are cut along the grain: they are turned so their long axis lies along
  the fabric, and a deviation from that is penalised, the more the deviation, the more the penalty.
  Panels are marked per part in the manifest (`grain = true`) or by name (`--panels top,bot`);
  other parts (ribs) use the tolerance freely.
- A free strip along the top of a sheet is reusable fabric, valued below fabric saved at the end.
- Fillers: small parts of other jobs that go into free space without making a sheet longer.
- Left-hand panels: parts are drawn as right-hand ones, the manifest asks for mirrored copies.
- Gap between parts. No gap is added at the sheet edge: set the edge margin through the sheet size.
- Every layout is checked independently of the solver (no overlaps, inside the sheet, gap kept).
- Reproducible mode: a fixed seed plus an iteration budget gives the same layout every time.

## Build

Requires Rust ≥ 1.90. A patched copy of sparrow is in `vendor/sparrow` (see
`vendor/sparrow/VENDORED.md`).

```bash
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
| `sheet_N.dxf` | one DXF per sheet: layer `SHEET` (sheet rectangle) and every part as an `INSERT` of a block holding its source drawing unchanged (own layers, labels, marks; left copies are the same block mirrored). With `--outlines`: traced contours on layer `PARTS` and a large part name on `LABELS` instead |
| `sheet_N.svg` | preview of the sheet |
| `index.html` | all previews on one page, the report below them |
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
top5 = { qty = 1, left = 1 }   # one as drawn (right) and one mirrored (left)
bot5 = { right = 1, left = 1 } # `right` is the same as `qty`
top6 = { qty = 1, left = 1, grain = true }  # a panel: cut along the grain, see "Panel tilt"
```

`grain = true` marks a part as a panel, `grain = false` as a part free to tilt, whatever its name.
A `grain = ...` line before `[parts]` applies to every part that does not set it, e.g. a folder of
panels starts with `grain = true`. Without `grain` a part is a panel only if `--panels` names it.
In `parts.csv` panels are chosen with `--panels` only.

```csv
name,qty,left
rib1,2
top5,1,1
```

A part missing from the manifest gets quantity 1 (with a warning); quantity 0 leaves it out.

Parts are taken as drawn, i.e. right-hand. `left` copies are mirrored across the Y axis (outer
contour and everything inside) and appear as `<name>_L` in the layout and the report.

## Parameters of `nest run`

| option | default | meaning |
|---|---|---|
| `--width` | 1560 | sheet width, mm (across the fabric) |
| `--length` | 5800 | sheet length, mm (along the fabric) |
| `-g, --gap` | 0 | gap between parts, mm |
| `-r, --rotations` | 0,90,180,270 | base rotation angles, degrees; `0` disables rotation |
| `--rotation-tolerance` | 0 | each base angle ± this, degrees |
| `--rotation-step` | 1 | step within the tolerance, degrees |
| `--panels` | – | parts whose name contains one of these are panels (e.g. `top,bot`); `grain` in the manifest takes precedence |
| `--tilt-penalty` | 10 | a panel d° off a base angle costs this · d² mm of length; 0 = off |
| `--panel-tolerance` | 3 | largest tilt of a panel off a base angle, degrees (ribs use the whole `--rotation-tolerance`) |
| `--panel-axis` | long | panel grain: `long` (along the long axis) or `drawn` (drawing X axis) |
| `--min-strip` | 150 | a free strip on top, or free space open to the right end, at least this wide, mm, is reusable; 0 = off |
| `--strip-ratio` | 2 | fabric saved at the end is worth this many times the same area of free strip or space at the right end |
| `--fillers` | – | folder of filler parts for free space; may be repeated |
| `--outlines` | – | sheet DXF with traced contours and large name labels instead of the source drawings as blocks (for checking layouts) |
| `--strategy` | balanced | how parts are distributed over sheets: `balanced` or `greedy` |
| `--drafts` | 0 | quick drafts (1/10 of the time limit each) that decide which parts go on which sheet; the best one is polished with the full time limit per sheet. 0 = off |
| `-t, --time-limit` | 60 | search time per sheet, s |
| `-i, --iterations` | – | iteration budget per sheet instead of time (reproducible) |
| `-s, --seed` | random | random seed (printed in the report) |
| `-w, --workers` | 3 | solver threads; keep fixed for reproducibility |
| `--tolerance` | 0.2 | arc/spline approximation, mm |
| `--join-tolerance` | 0.05 | joining segment ends into chains, mm |
| `--close-gaps` | 1 | largest gap bridged when assembling the outer contour, mm |
| `--edge-tolerance` | 2 | how far inner geometry may stick out of the outer contour, mm |
| `--layers` | all | read only these layers |
| `--ignore-layers` | – | skip these layers |
| `--strict-open` | off | any open line is an error, even inside a part |
| `-o, --out` | `runs/<input>_<date>_<time>` | output folder |
| `-v` | – | progress output (`-vv` for solver debug output) |

`nest extract <assembly.dxf> --blocks <patterns> --out <dir>` saves every block whose name matches
(`*`, `?`, case-insensitive) as `<dir>/<block>.dxf` and writes `<dir>/parts.toml` with quantity 1
and the insert count of each block as a comment.

`nest extract-labelled <assembly.dxf> --headings TOP=top,BOT=bottom --out <dir>` is for parts that are
not blocks but loose outlines with a label inside (panels such as `A16` under a heading `TOP COLOR 1`):
each label's smallest closed contour becomes `<dir>/<section>_col<k>/<section>_<number><letter>.dxf`,
grouped by drawing column (`--split-x` gives the column borders explicitly, `--region` limits the
search). Each `parts.toml` asks for one right and one left (mirrored) copy of every panel and
marks them all as panels (`grain = true`).

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

### Drafts

Most of a run's time goes into deciding which parts go on which sheet (dealing, repairs, probes),
little into packing each sheet tightly. With `--drafts N` the whole procedure above runs N times
at 1/10 of the time limit with different seeds; the draft with the fewest sheets and least fabric
(+ tilt penalty) wins, and each of its sheets is nested again with the full time limit, its parts
staying on it. One such run is meant to replace a seed sweep, e.g. on a laptop.

### Panel tilt

Off unless asked for: a part is a panel if the manifest says `grain = true`, or, without `grain`,
if its name contains one of `--panels` (case-insensitive, e.g. `--panels top,bot`). The report
says how many parts are panels.

Panels should be cut straight: a strip of fabric left free beside them is better than tilted
panels. Both threads of the fabric are fine (warp and weft, i.e. 0/90/180/270°), any angle to
them is not.

Drawings often show panels at an angle, so before nesting every elongated panel (at least twice as
long as wide, `--panel-axis long`, the default) is turned by at most 45° so that its long axis (the
direction across which the panel is narrowest) lies along X. `-v` prints each turn. With
`--panel-axis drawn` the drawing's X axis is taken as the grain instead.
`cargo run --release --example axis -p nest-core -- <dir>` shows the long axis of every part as drawn.

Panels are tilted by at most `--panel-tolerance` (default 3°; with `--rotation-step 2` that is
0 and ±2°). Without the limit the search spends its time on denser layouts with steeper panels that
then lose to the penalty, and a longer time limit gains nothing. Within the limit a panel placed
d degrees off its base angle costs `--tilt-penalty` · d² mm (default 10: 2° = 40 mm, 5° = 250 mm). In sparrow (patched, `vendor/sparrow`):

- among collision-free positions for a panel, the least tilted wins; half of the samples are taken
  at straight angles;
- the initial placement counts the penalty as extra length.

Each solver run keeps every feasible layout it found, and the one with the least used length +
penalty is used (a layout that fits the sheet length always wins over one that does not). The report
shows per sheet how many panels are tilted and the largest tilt.

### Free strip on top and free space at the right end

A free band along the top of a sheet, at least `--min-strip` wide (150 mm), takes other parts later,
so it is not counted as waste. So is free space open to the right end of the layout: wherever the
parts end at least `--min-strip` short of the used length, over a stretch at least `--min-strip`
tall (a bottom band or a pocket left by a short part; the laser cut follows the parts there). A hole
between parts is waste. Free fabric is worth less than fabric saved at the end of the sheet, though:
`--strip-ratio` k (default 2) says that a metre of length saved is worth k metres of the same width
of free fabric. A sheet then consumes `used length − (strip · used length + space at the right end) / (k · width)`.
Among the layouts the solver finds, the one consuming least (plus the tilt penalty) is used, so of two
equally long layouts the one with its free space at the right end, not in holes, wins.

When all sheets are made, every sheet with spare length is nested again into narrower strips
(leaving 150 mm, the most the parts allow, and halfway between), and the variant consuming least
(tilt penalty included) is kept. The report shows the strip per sheet and the total
"Fabric consumed"; `sweep_summary.py` ranks runs by it. The report also shows the free area at the
right end per sheet.

### Final compaction

The solver only shortens the strip, so it has no reason to close a hole that does not make the
sheet shorter. When the sheets are made, every part slides left, then down, as far as the sheet
edges and the other parts (at the gap) allow, a few rounds, on the exact geometry. Free space gathers
at the right end and along the top, where it counts as reusable; the sheet never gets longer.

### Fillers

`--fillers <dir>` (may be repeated) adds the parts of another folder as fillers; their manifest
quantity is the most that may be used. After all sheets are made, fillers go into the free space of
each sheet, largest first, each as often as it fits: the sheet's parts stay where they are, the sheet
does not get longer and no sheet is added. Fillers that find no room are not an error; the report
says how many were placed.

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
fabric consumed and shows panel tilt. `examples/layout_stats.rs` measures an existing hand-made layout the same way:

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
| sparrow 0.3.0 (master `6f7b9d3`, vendored with a patch) | MIT |
| jagua-rs 1.1.0 | MPL-2.0 (used unmodified) |
| dxf 0.6.2 | MIT |
| geo 0.33 | MIT OR Apache-2.0 |
| clap, serde, toml, anyhow, log, env_logger, rand, ctrlc | MIT OR Apache-2.0 |
| csv | Unlicense OR MIT |
