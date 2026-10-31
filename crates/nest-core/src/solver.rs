//! Wrapper over sparrow / jagua-rs: job -> `ExtSPInstance` -> optimisation -> placements in DXF coordinates.
//!
//! Axes: fabric width = jagua strip height (Y), cut length = strip width (X).

use crate::geom::{BBox, Demand, NestResult, Placement, Pt, Sheet, Transform, Unplaced};
use anyhow::{bail, Context, Result};
use jagua_rs::io::ext_repr::{ExtItem as BaseItem, ExtOrientation, ExtRotation, ExtSPolygon, ExtShape};
use jagua_rs::io::import::Importer;
use jagua_rs::probs::spp::entities::SPSolution;
use jagua_rs::probs::spp::io::ext_repr::{ExtItem, ExtSPInstance};
use jagua_rs::Instant;
use log::{info, warn};
use rand::rngs::Xoshiro256PlusPlus;
use rand::SeedableRng;
use sparrow::config::{ShrinkDecayStrategy, DEFAULT_SPARROW_CONFIG};
use sparrow::consts::{DEFAULT_COMPRESS_TIME_RATIO, DEFAULT_EXPLORE_TIME_RATIO, DEFAULT_FAIL_DECAY_RATIO_CMPR};
use sparrow::optimizer::optimize;
use jagua_rs::entities::PlacedItem;
use jagua_rs::geometry::DTransformation;
use jagua_rs::probs::spp::entities::{SPPlacement, SPProblem};
use sparrow::consts::LBF_SAMPLE_CONFIG;
use sparrow::eval::lbf_evaluator::LBFEvaluator;
use sparrow::eval::sample_eval::SampleEval;
use sparrow::sample::search::{search_placement, SampleConfig};
use sparrow::sample::tilt::TiltPenalty;
use sparrow::util::listener::{ReportType, SolutionListener};
use sparrow::util::terminator::Terminator;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How parts are distributed over sheets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Deal parts over the estimated number of sheets up front (long and short parts mixed on
    /// every sheet), then repair overfull sheets and fill gaps from the overflow.
    Balanced,
    /// Fill one sheet at a time from a strip of all remaining parts.
    Greedy,
}

/// When to stop the search.
#[derive(Debug, Clone, Copy)]
pub enum Budget {
    /// Wall-clock limit. Results may differ between runs even with the same seed.
    Time(Duration),
    /// Number of termination checks: fully reproducible with the same seed and worker count.
    Iterations(usize),
}

#[derive(Debug, Clone)]
pub struct NestParams {
    /// Sheet width, mm.
    pub width: f64,
    /// Sheet length, mm.
    pub length: f64,
    /// Minimum gap between parts, mm. No gap is added at the fabric edge.
    pub gap: f64,
    /// Base rotation angles, degrees.
    pub rotations: Vec<f64>,
    /// Each base angle may deviate by up to this much in either direction, degrees.
    pub rotation_tolerance: f64,
    /// Step of the deviation around each base angle, degrees.
    pub rotation_step: f64,
    /// Parts whose name contains one of these (case-insensitive) are panels: their deviation from a
    /// base angle is penalised. Other parts (ribs) may use the whole tolerance freely.
    pub panels: Vec<String>,
    /// Penalty for a panel deviating from a base angle by d degrees: `tilt_penalty · d²`, in mm of
    /// used length. Among the layouts found, the one with the least length + penalty is kept.
    /// 0 turns the penalty off.
    pub tilt_penalty: f64,
    /// Panels deviate from a base angle by at most this much, degrees (the general
    /// `rotation_tolerance` still applies). Layouts with steeper panels would lose to the penalty
    /// anyway, so the search does not spend time on them.
    pub panel_tolerance: f64,
    /// Turn elongated panels so their long axis lies along a base angle before nesting (see
    /// [`align_panels`]). Off: the drawing's X axis is the panel's straight direction.
    pub align_panels: bool,
    /// A free band along the top of a sheet at least this wide (mm) is reusable fabric, like the
    /// fabric beyond the cut: it is not counted as used. 0 turns this off.
    pub min_strip: f64,
    /// How many times fabric saved at the end of a sheet is worth more than the same area left as a
    /// free strip on top: the strip counts as saved fabric divided by this. 1 = equally valuable.
    pub strip_ratio: f64,
    /// A part whose outer area is below this fraction of the largest part's is small: sheets are
    /// made of the other parts, and small parts then go into their holes (see [`nest_observed`]).
    /// 0 turns this off.
    pub small_parts: f64,
    pub budget: Budget,
    pub strategy: Strategy,
    /// 0: sheets are made once with the full budget. Otherwise this many quick drafts (each with
    /// 1/10 of the budget, different seeds) decide which parts go on which sheet, and every sheet of
    /// the best draft is then nested again with the full budget.
    pub drafts: usize,
    pub seed: Option<u64>,
    /// Number of sparrow separator workers.
    pub workers: usize,
    /// External cancel (Ctrl-C in the CLI, a button in a GUI): moves to the next phase / stops.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for NestParams {
    fn default() -> Self {
        Self {
            width: 1560.0,
            length: 5800.0,
            gap: 0.0,
            rotations: vec![0.0, 90.0, 180.0, 270.0],
            rotation_tolerance: 0.0,
            rotation_step: 1.0,
            panels: vec!["top".into(), "bot".into()],
            tilt_penalty: 10.0,
            panel_tolerance: 3.0,
            align_panels: true,
            min_strip: 150.0,
            strip_ratio: 2.0,
            small_parts: 0.0,
            budget: Budget::Time(Duration::from_secs(60)),
            strategy: Strategy::Balanced,
            drafts: 0,
            seed: None,
            workers: DEFAULT_SPARROW_CONFIG.expl_cfg.separator_config.n_workers,
            cancel: None,
        }
    }
}

impl Budget {
    /// A `num/den` share of the budget, at least one iteration or one millisecond.
    pub fn share(self, num: u32, den: u32) -> Budget {
        match self {
            Budget::Time(d) => Budget::Time((d * num / den).max(Duration::from_millis(1))),
            Budget::Iterations(n) => Budget::Iterations((n * num as usize / den as usize).max(1)),
        }
    }
}

impl NestParams {
    /// All allowed angles: every base angle ± tolerance in steps, normalised to [0, 360).
    pub fn allowed_angles(&self) -> Vec<f64> {
        let n = if self.rotation_tolerance > 0.0 { (self.rotation_tolerance / self.rotation_step + 1e-9).floor() as i64 } else { 0 };
        let mut out: Vec<f64> = self
            .rotations
            .iter()
            .flat_map(|&b| (-n..=n).map(move |k| b + k as f64 * self.rotation_step))
            .map(|a| {
                let a = a.rem_euclid(360.0);
                // Keep angles tidy: 359.9999999 -> 0.
                if (a - 360.0).abs() < 1e-9 { 0.0 } else { (a * 1e6).round() / 1e6 }
            })
            .collect();
        out.sort_by(f64::total_cmp);
        out.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        out
    }

    /// Whether a part (by name) is a panel, see [`NestParams::panels`].
    pub fn is_panel(&self, name: &str) -> bool {
        let name = name.to_lowercase();
        self.panels.iter().any(|p| !p.is_empty() && name.contains(&p.to_lowercase()))
    }

    /// Deviation of an angle from the closest base angle, degrees (≥ 0).
    pub fn tilt(&self, rotation_deg: f64) -> f64 {
        self.rotations
            .iter()
            .map(|b| ((rotation_deg - b).rem_euclid(360.0) + 180.0).rem_euclid(360.0) - 180.0)
            .map(f64::abs)
            .fold(f64::INFINITY, f64::min)
    }

    /// Width of the free band along the top of a sheet if it counts as reusable (≥ `min_strip`), else 0.
    pub fn top_strip(&self, demands: &[Demand], placements: &[Placement]) -> f64 {
        if self.min_strip <= 0.0 || placements.is_empty() {
            return 0.0;
        }
        let top = placements.iter().map(|p| demands[p.part].part.shape.transformed(&p.transform).bbox().max.y).fold(0.0, f64::max);
        let h = (self.width - top).max(0.0);
        if h >= self.min_strip - 1e-6 { h } else { 0.0 }
    }

    /// Fabric a sheet consumes, as length of the full width: the used length, less the reusable
    /// band along the top, valued at 1/`strip_ratio` of fabric saved at the end of the sheet.
    pub fn consumed(&self, demands: &[Demand], placements: &[Placement], used_length: f64) -> f64 {
        let free = self.top_strip(demands, placements) * used_length + self.right_pocket(demands, placements, used_length);
        used_length - free / self.width / self.strip_ratio.max(1.0)
    }

    /// Free area (mm²) open to the right end of a sheet, below the top strip: wherever bands across
    /// the sheet end at least `min_strip` short of the used length, over a stretch at least
    /// `min_strip` tall. A laser cut can follow the parts there, and the fabric takes parts of other
    /// jobs; a hole between parts cannot. Like the top strip it is worth 1/`strip_ratio` of length.
    pub fn right_pocket(&self, demands: &[Demand], placements: &[Placement], used_length: f64) -> f64 {
        self.right_pocket_bands(demands, placements, used_length).iter().map(|&(y0, y1, x)| (y1 - y0) * (used_length - x)).sum()
    }

    /// The free space of [`NestParams::right_pocket`] as horizontal bands `(y0, y1, x)`: free from
    /// `x` to the used length.
    fn right_pocket_bands(&self, demands: &[Demand], placements: &[Placement], used_length: f64) -> Vec<(f64, f64, f64)> {
        /// Height of the bands the sheet is scanned in, mm.
        const BAND: f64 = 5.0;
        if self.min_strip <= 0.0 || placements.is_empty() {
            return Vec::new();
        }
        let top = self.width - self.top_strip(demands, placements);
        let n = (top / BAND).ceil() as usize;
        // Rightmost part boundary in each band.
        let mut right = vec![0.0f64; n];
        for p in placements {
            let ring = demands[p.part].part.shape.transformed(&p.transform).outer;
            for i in 0..ring.len() {
                let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                let (lo, hi) = (a.y.min(b.y).max(0.0), a.y.max(b.y).min(top));
                if lo > hi {
                    continue;
                }
                let x_at = |y: f64| if (b.y - a.y).abs() < 1e-12 { a.x.max(b.x) } else { a.x + (b.x - a.x) * (y - a.y) / (b.y - a.y) };
                // Bands the edge passes through; touching a band's boundary does not count.
                let end = ((hi / BAND).ceil() as usize).min(n);
                for (k, r) in right.iter_mut().enumerate().take(end).skip((lo / BAND) as usize) {
                    let (y0, y1) = ((k as f64 * BAND).max(lo), ((k + 1) as f64 * BAND).min(hi));
                    *r = r.max(x_at(y0)).max(x_at(y1));
                }
            }
        }
        let (mut bands, mut run) = (Vec::new(), Vec::new());
        for k in 0..=n {
            let y0 = k as f64 * BAND;
            let gap = if k < n { used_length - right[k] } else { 0.0 };
            if gap >= self.min_strip {
                run.push((y0, (y0 + BAND).min(top), right[k]));
            } else {
                if run.last().map_or(0.0, |r: &(f64, f64, f64)| r.1) - run.first().map_or(0.0, |r| r.0) >= self.min_strip - 1e-6 {
                    bands.append(&mut run);
                }
                run.clear();
            }
        }
        bands
    }

    /// Reusable free space of a sheet as rectangles: the band on top and the space open to the right
    /// end. Small parts going into holes are kept out of them.
    fn reusable_regions(&self, demands: &[Demand], placements: &[Placement], used_length: f64) -> Vec<BBox> {
        /// Neighbouring bands whose free space starts within this of each other make one rectangle, mm.
        const MERGE: f64 = 20.0;
        let mut out: Vec<BBox> = Vec::new();
        for (y0, y1, x) in self.right_pocket_bands(demands, placements, used_length) {
            match out.last_mut() {
                Some(r) if (r.max.y - y0).abs() < 1e-6 && (r.min.x - x).abs() < MERGE => {
                    r.max.y = y1;
                    r.min.x = r.min.x.min(x);
                }
                _ => out.push(BBox { min: Pt::new(x, y0), max: Pt::new(used_length, y1) }),
            }
        }
        let strip = self.top_strip(demands, placements);
        if strip > 0.0 {
            out.push(BBox { min: Pt::new(0.0, self.width - strip), max: Pt::new(used_length, self.width) });
        }
        out
    }

    /// Penalty of a part placed at `rotation_deg`, mm of length (0 for non-panels).
    pub fn tilt_cost(&self, name: &str, rotation_deg: f64) -> f64 {
        if self.tilt_penalty > 0.0 && self.is_panel(name) {
            let d = self.tilt(rotation_deg);
            // Below the angle snapping noise a part counts as straight.
            if d < 1e-3 { 0.0 } else { self.tilt_penalty * d * d }
        } else {
            0.0
        }
    }
}

/// A sheet is not expected to be packed denser than this, which bounds the probes worth trying.
const MAX_DENSITY: f64 = 0.92;
/// Density used to estimate the sheet count, how much to move out of an overfull sheet and the
/// length a part takes up on some sheet.
const TARGET_DENSITY: f64 = 0.85;
/// A draft gets this fraction (1/n) of the per-sheet budget, see [`NestParams::drafts`].
const DRAFT_SHARE: u32 = 10;

/// Panels this much longer than wide have a long axis (the grain direction) to align.
pub const MIN_PANEL_ELONGATION: f64 = 2.0;

/// Turns every elongated panel so its long axis lies along X: the fabric grain should run along the
/// panel, while drawings often show it at an angle. The shape is rotated in place (by at most 45°,
/// to the closest of 0/90/180/270), so the layout, validation and output all use the turned part.
/// Returns `(part index, degrees turned)` for each turned part.
pub fn align_panels(demands: &mut [Demand], params: &NestParams) -> Vec<(usize, f64)> {
    let mut turned = Vec::new();
    for (i, d) in demands.iter_mut().enumerate() {
        if !params.is_panel(&d.part.name) {
            continue;
        }
        let (axis, elongation) = d.part.shape.long_axis();
        let off = axis - 90.0 * (axis / 90.0).round();
        if elongation < MIN_PANEL_ELONGATION || off.abs() < 0.01 {
            continue;
        }
        d.part.shape = d.part.shape.transformed(&Transform { rotation_deg: -off, translation: Pt::new(0.0, 0.0) });
        d.part.drawn.rotation_deg -= off;
        turned.push((i, -off));
    }
    turned
}

#[derive(Debug, Clone)]
pub struct NestOutcome {
    pub result: NestResult,
    /// The seed actually used (generated if none was given).
    pub seed: u64,
    pub elapsed: Duration,
}

/// An intermediate layout produced while filling sheets, for inspection.
#[derive(Debug, Clone)]
pub struct Step {
    /// Sheet being filled (1-based).
    pub sheet: usize,
    /// What the run was: "strip", "probe +<part>", "sheet".
    pub kind: String,
    pub placements: Vec<Placement>,
    pub used_length: f64,
    /// Whether the layout fits the sheet length.
    pub fits: bool,
}

/// Nests the demands onto as many fixed-size sheets as needed. See [`nest_observed`].
pub fn nest(demands: &[Demand], params: &NestParams) -> Result<NestOutcome> {
    nest_observed(demands, params, &mut |_| {})
}

/// Nests the demands onto fixed-size sheets, reporting every intermediate layout to `observer`.
///
/// With [`Strategy::Balanced`] parts are first dealt over the estimated sheet count (see the code);
/// whatever does not fit then goes through the greedy loop below.
///
/// sparrow only solves strip packing, so the greedy loop fills sheets one at a time on top of it:
/// 1. all remaining parts are nested into a strip of the sheet width; if the strip is no longer
///    than the sheet, it is the last sheet;
/// 2. otherwise the parts lying entirely within the sheet length form a valid starting sheet;
/// 3. the sheet is then grown: remaining parts, largest first and only while the free area allows,
///    are added one at a time, each kept if a short strip run still fits the sheet length;
/// 4. the rest goes on to the next sheet.
///
/// 5. finally the sheet is compacted once more, since the fabric beyond a straight cut after the
///    last part is not waste.
///
/// Half of the per-sheet budget goes to step 1, the other half is shared by the probes of step 3
/// and the compaction.
///
/// With [`NestParams::drafts`] > 0 all of the above runs that many times at 1/10 of the budget;
/// the draft with the fewest sheets and least fabric + tilt penalty wins, and each of its sheets is
/// nested again with the full budget (its parts stay on it).
pub fn nest_observed(demands: &[Demand], params: &NestParams, observer: &mut dyn FnMut(&Step)) -> Result<NestOutcome> {
    let started = Instant::now();
    if !(params.width > 0.0 && params.length > 0.0 && params.gap >= 0.0 && params.gap < params.width) {
        bail!("invalid parameters: sheet {} x {} mm, gap {} mm", params.width, params.length, params.gap);
    }
    if params.rotations.is_empty() {
        bail!("the list of allowed angles is empty");
    }
    if params.rotation_tolerance < 0.0 || (params.rotation_tolerance > 0.0 && params.rotation_step <= 0.0) {
        bail!("invalid rotation tolerance or step");
    }
    let seed = params.seed.unwrap_or_else(rand::random);
    let (prep, unplaced) = prepare(demands, params);

    let mut run = 0u64;
    let mut next_seed = || {
        run += 1;
        seed.wrapping_add(run.wrapping_mul(0x9E37_79B9_7F4A_7C15))
    };
    let bbox = |p: &Placement| demands[p.part].part.shape.transformed(&p.transform).bbox();
    let len_of = |pl: &[Placement]| pl.iter().map(|p| bbox(p).max.x).fold(0.0, f64::max);
    let area = |i: usize| demands[i].part.shape.area();
    let tilt_of = |pl: &[Placement]| pl.iter().map(|p| params.tilt_cost(&demands[p.part].part.name, p.transform.rotation_deg)).sum::<f64>();
    let eps = 1e-6;
    let fits = |pl: &[Placement]| len_of(pl) <= params.length + eps;
    // Fabric a sheet consumes plus the tilt penalty: what the polishing passes minimise.
    let sheet_cost = |pl: &[Placement]| params.consumed(demands, pl, len_of(pl)) + tilt_of(pl);
    let mut report = |sheet: usize, kind: String, pl: &[Placement]| {
        observer(&Step { sheet, kind, placements: pl.to_vec(), used_length: len_of(pl), fits: fits(pl) });
    };

    // Small parts and fillers are kept out until the sheets are made.
    let largest = (0..demands.len()).filter(|&i| prep.items[i].is_some()).map(area).fold(0.0, f64::max);
    let small: Vec<bool> = (0..demands.len()).map(|i| !demands[i].filler && area(i) < params.small_parts * largest).collect();
    let placeable = |i: usize| prep.items[i].is_some() && !demands[i].filler;
    let main: Vec<usize> = (0..demands.len()).map(|i| if placeable(i) && !small[i] { demands[i].qty } else { 0 }).collect();
    let mut late: Vec<usize> = (0..demands.len()).map(|i| if placeable(i) && small[i] { demands[i].qty } else { 0 }).collect();
    if late.iter().sum::<usize>() > 0 {
        info!("{} small parts ({} copies) go into holes after the sheets are made", late.iter().filter(|&&n| n > 0).count(), late.iter().sum::<usize>());
    }

    let mut sheets = if params.drafts == 0 {
        make_sheets(demands, params, &prep, main.clone(), &mut next_seed, &mut report)?
    } else {
        // Quick drafts decide which parts go on which sheet; the best one is then polished, each
        // sheet nested again with the full budget and kept if that consumes less fabric.
        let draft_params = NestParams { budget: params.budget.share(1, DRAFT_SHARE), ..params.clone() };
        let mut best: Option<((usize, f64), Vec<Sheet>)> = None;
        for k in 1..=params.drafts {
            let draft = make_sheets(demands, &draft_params, &prep, main.clone(), &mut next_seed, &mut |n, kind, pl| report(n, format!("draft{k} {kind}"), pl))?;
            let score = (draft.len(), draft.iter().map(|s| sheet_cost(&s.placements)).sum::<f64>());
            info!("draft {k}: {} sheets, {:.1} mm", score.0, score.1);
            if best.as_ref().is_none_or(|b| score.0 < b.0.0 || (score.0 == b.0.0 && score.1 < b.0.1)) {
                best = Some((score, draft));
            }
        }
        let mut sheets = best.expect("at least one draft").1;
        for (n, sheet) in sheets.iter_mut().enumerate() {
            let mut counts = vec![0usize; demands.len()];
            for p in &sheet.placements {
                counts[p.part] += 1;
            }
            let trial = prep.strip(&counts, params, params.length, params.budget, next_seed())?;
            report(n + 1, "polish".into(), &trial);
            if fits(&trial) && sheet_cost(&trial) < sheet_cost(&sheet.placements) - eps {
                sheet.used_length = len_of(&trial);
                sheet.placements = trial;
            }
        }
        sheets
    };

    // A free band along the top is reusable fabric: every sheet with spare length is nested again
    // into narrower strips, and kept that way if it then consumes less fabric (tilt included).
    if params.min_strip > 0.0 {
        let cost = sheet_cost;
        for (n, sheet) in sheets.iter_mut().enumerate() {
            let parts: f64 = sheet.placements.iter().map(|p| area(p.part)).sum();
            // Widest band that can still hold the parts on the sheet at a plausible density.
            let h_max = params.width - parts / (MAX_DENSITY * params.length);
            if h_max < params.min_strip {
                continue;
            }
            let mut counts = vec![0usize; demands.len()];
            for p in &sheet.placements {
                counts[p.part] += 1;
            }
            let mut best = cost(&sheet.placements);
            for h in [params.min_strip, (params.min_strip + h_max) / 2.0, h_max] {
                let narrow = NestParams { width: params.width - h, ..params.clone() };
                // Parts that fit the narrower strip at no angle make sparrow fail: just skip that try.
                let Ok(trial) = prep.strip(&counts, &narrow, params.length, params.budget.share(1, 4), next_seed()) else {
                    continue;
                };
                report(n + 1, format!("strip {h:.0}"), &trial);
                if fits(&trial) && cost(&trial) < best - eps {
                    best = cost(&trial);
                    sheet.used_length = len_of(&trial);
                    sheet.placements = trial;
                }
            }
        }
    }

    // Parts slide left and down on the exact geometry, closing holes the solver had no reason to
    // close: the free space moves to the right end and the top, where it counts as reusable.
    for (n, sheet) in sheets.iter_mut().enumerate() {
        let mut pl = sheet.placements.clone();
        if crate::compact::slide(demands, &mut pl, params.gap) && sheet_cost(&pl) <= sheet_cost(&sheet.placements) + eps {
            sheet.used_length = len_of(&pl);
            sheet.placements = pl;
            report(n + 1, "slide".into(), &sheet.placements);
        }
    }

    if late.iter().sum::<usize>() > 0 {
        place_small(demands, params, &prep, &mut sheets, &mut late, &mut next_seed, &mut report)?;
    }
    if small.iter().any(|&s| s) {
        relocate_small(demands, params, &prep, &mut sheets, &small, &mut next_seed, &mut report)?;
    }

    // Fillers go into the free space of the finished sheets, which stay as they are.
    let mut fillers: Vec<usize> = (0..demands.len()).map(|i| if prep.items[i].is_some() && demands[i].filler { demands[i].qty } else { 0 }).collect();
    for (n, sheet) in sheets.iter_mut().enumerate() {
        if fillers.iter().all(|&f| f == 0) {
            break;
        }
        let added = prep.insert_fillers(&sheet.placements, &mut fillers, params, sheet.used_length, params.width, &[], next_seed())?;
        if !added.is_empty() {
            info!("sheet {}: {} fillers added", n + 1, added.len());
            sheet.placements.extend(added);
            sheet.placements.sort_by_key(|p| p.part);
            report(n + 1, "filled".into(), &sheet.placements);
        }
    }

    Ok(NestOutcome { result: NestResult { sheets, unplaced }, seed, elapsed: started.elapsed() })
}

/// Fills sheets with `remaining[i]` copies of each part by the chosen strategy (see [`nest_observed`]).
fn make_sheets(
    demands: &[Demand],
    params: &NestParams,
    prep: &Prepared,
    mut remaining: Vec<usize>,
    next_seed: &mut dyn FnMut() -> u64,
    report: &mut dyn FnMut(usize, String, &[Placement]),
) -> Result<Vec<Sheet>> {
    /// Probes per sheet; each gets an equal share of half the sheet budget.
    const MAX_PROBES: u32 = 8;

    let bbox = |p: &Placement| demands[p.part].part.shape.transformed(&p.transform).bbox();
    let len_of = |pl: &[Placement]| pl.iter().map(|p| bbox(p).max.x).fold(0.0, f64::max);
    let area = |i: usize| demands[i].part.shape.area();
    let tilt_of = |pl: &[Placement]| pl.iter().map(|p| params.tilt_cost(&demands[p.part].part.name, p.transform.rotation_deg)).sum::<f64>();
    let eps = 1e-6;
    let fits = |pl: &[Placement]| len_of(pl) <= params.length + eps;
    // A probe that adds part `c` is kept only if the sheet grows by no more than the part would take
    // up elsewhere: squeezing a part onto a sheet at the cost of a much longer sheet wastes fabric.
    let worth = |c: usize, before: &[Placement], after: &[Placement]| {
        fits(after) && len_of(after) - len_of(before) <= area(c) / (params.width * TARGET_DENSITY) + eps
    };
    let mut sheets: Vec<Sheet> = Vec::new();

    if params.strategy == Strategy::Balanced && remaining.iter().sum::<usize>() > 0 {
        const MAX_REPAIRS: usize = 4;
        let (w, l) = (params.width, params.length);
        let total: f64 = (0..demands.len()).map(|i| remaining[i] as f64 * area(i)).sum();
        let n_sheets = ((total / (TARGET_DENSITY * w * l)).ceil() as usize).max(1);

        // Long parts (ribs) and short ones (fillers) are dealt separately, each in serpentine order
        // 1..N, N..1, ..., largest first: every sheet gets long and short ribs that pair up along
        // the sheet, plus its share of small parts.
        let long = |i: usize| {
            let b = demands[i].part.shape.bbox();
            b.width().max(b.height()) > 0.25 * l
        };
        let mut copies: Vec<usize> = (0..demands.len()).flat_map(|i| std::iter::repeat_n(i, remaining[i])).collect();
        copies.sort_by(|&a, &b| area(b).total_cmp(&area(a)));
        let mut batches = vec![vec![0usize; demands.len()]; n_sheets];
        for class in [true, false] {
            for (k, &c) in copies.iter().filter(|&&c| long(c) == class).enumerate() {
                let round = k / n_sheets;
                let pos = k % n_sheets;
                let b = if round % 2 == 0 { pos } else { n_sheets - 1 - pos };
                batches[b][c] += 1;
            }
        }
        info!("balanced plan: {n_sheets} sheets for {} parts", copies.len());

        let mut overflow = vec![0usize; demands.len()];
        for mut counts in batches {
            let sheet_no = sheets.len() + 1;
            let mut pl = prep.strip(&counts, params, params.length, params.budget.share(1, 4), next_seed())?;
            report(sheet_no, "batch".into(), &pl);
            let mut repairs = 0;
            while !fits(&pl) && repairs < MAX_REPAIRS {
                repairs += 1;
                // Move out parts reaching past the sheet end, smallest first, about as much area as sticks out.
                let excess = (len_of(&pl) - l) * w * TARGET_DENSITY;
                let mut crossing: Vec<usize> = pl.iter().filter(|p| bbox(p).max.x > l + eps).map(|p| p.part).collect();
                crossing.sort_by(|&a, &b| area(a).total_cmp(&area(b)));
                let mut moved = 0.0;
                for c in crossing {
                    if moved > 0.0 && moved >= excess {
                        break;
                    }
                    counts[c] -= 1;
                    overflow[c] += 1;
                    moved += area(c);
                }
                pl = prep.strip(&counts, params, params.length, params.budget.share(1, 4), next_seed())?;
                report(sheet_no, format!("repair{repairs}"), &pl);
            }
            if !fits(&pl) {
                // Still too long: keep what lies inside the sheet, the rest joins the overflow.
                let (inside, out): (Vec<Placement>, Vec<Placement>) = pl.into_iter().partition(|p| bbox(p).max.x <= l + eps);
                for p in out {
                    counts[p.part] -= 1;
                    overflow[p.part] += 1;
                }
                pl = inside;
                report(sheet_no, "cut".into(), &pl);
            }

            // Fill the free area with parts that earlier sheets could not take, largest first.
            let mut free = MAX_DENSITY * w * l - pl.iter().map(|p| area(p.part)).sum::<f64>();
            let mut candidates: Vec<usize> = (0..demands.len()).filter(|&i| overflow[i] > 0).collect();
            candidates.sort_by(|&a, &b| area(b).total_cmp(&area(a)));
            let (mut probes, mut i) = (0, 0);
            while probes < MAX_PROBES && i < candidates.len() {
                let c = candidates[i];
                if overflow[c] == 0 || area(c) > free {
                    i += 1;
                    continue;
                }
                counts[c] += 1;
                probes += 1;
                let trial = prep.strip(&counts, params, params.length, params.budget.share(1, 2 * MAX_PROBES), next_seed())?;
                report(sheet_no, format!("probe +{}", demands[c].part.name), &trial);
                if worth(c, &pl, &trial) {
                    overflow[c] -= 1;
                    free -= area(c);
                    pl = trial;
                } else {
                    counts[c] -= 1;
                    i += 1;
                }
            }

            for p in &pl {
                remaining[p.part] -= 1;
            }
            info!("sheet {sheet_no}: {} parts, used length {:.1} mm, {} in overflow", pl.len(), len_of(&pl), overflow.iter().sum::<usize>());
            report(sheet_no, "sheet".into(), &pl);
            sheets.push(Sheet { width: w, length: l, used_length: len_of(&pl), placements: pl });
        }
        // `remaining` is now exactly the overflow; the loop below puts it on further sheets.
    }

    while remaining.iter().sum::<usize>() > 0 {
        let sheet_no = sheets.len() + 1;
        let strip = prep.strip(&remaining, params, params.length, params.budget.share(1, 2), next_seed())?;
        report(sheet_no, "strip".into(), &strip);
        if fits(&strip) {
            info!("sheet {sheet_no}: last sheet, {} parts, used length {:.1} mm", strip.len(), len_of(&strip));
            sheets.push(Sheet { width: params.width, length: params.length, used_length: len_of(&strip), placements: strip });
            break;
        }

        let mut chosen: Vec<Placement> = strip.into_iter().filter(|p| bbox(p).max.x <= params.length + eps).collect();
        if chosen.is_empty() {
            bail!("sheet {sheet_no}: no part fits within the sheet length {} mm", params.length);
        }
        report(sheet_no, "cut".into(), &chosen);

        let mut counts = vec![0usize; demands.len()];
        for p in &chosen {
            counts[p.part] += 1;
        }
        let mut free = MAX_DENSITY * params.width * params.length - chosen.iter().map(|p| area(p.part)).sum::<f64>();
        // Candidates: one entry per part type still available, largest first.
        let mut candidates: Vec<usize> = (0..demands.len()).filter(|&i| remaining[i] > counts[i]).collect();
        candidates.sort_by(|&a, &b| area(b).total_cmp(&area(a)));
        let mut probes = 0;
        let mut i = 0;
        while probes < MAX_PROBES && i < candidates.len() {
            let c = candidates[i];
            if remaining[c] <= counts[c] || area(c) > free {
                i += 1;
                continue;
            }
            counts[c] += 1;
            probes += 1;
            let trial = prep.strip(&counts, params, params.length, params.budget.share(1, 2 * MAX_PROBES), next_seed())?;
            report(sheet_no, format!("probe +{}", demands[c].part.name), &trial);
            if worth(c, &chosen, &trial) {
                free -= area(c);
                chosen = trial;
                // Try the same part again: identical copies often pair up well.
            } else {
                counts[c] -= 1;
                i += 1;
            }
        }

        // Whatever lies beyond a straight cut after the last part is reusable fabric, so the sheet is
        // compacted once more: a ragged right edge (typical right after the cut) would waste it.
        let compact = prep.strip(&counts, params, params.length, params.budget.share(1, 2 * MAX_PROBES), next_seed())?;
        report(sheet_no, "compact".into(), &compact);
        if fits(&compact) && len_of(&compact) + tilt_of(&compact) < len_of(&chosen) + tilt_of(&chosen) {
            chosen = compact;
        }

        for p in &chosen {
            remaining[p.part] -= 1;
        }
        info!("sheet {sheet_no}: {} parts, used length {:.1} mm, {} left", chosen.len(), len_of(&chosen), remaining.iter().sum::<usize>());
        report(sheet_no, "sheet".into(), &chosen);
        sheets.push(Sheet { width: params.width, length: params.length, used_length: len_of(&chosen), placements: chosen });
    }
    Ok(sheets)
}

/// Puts the small parts (`left[i]` copies of part `i`) on the finished sheets, largest first:
/// 1. into a hole of some sheet where it costs no fabric (the free band on top stays free and the
///    consumed fabric does not grow), the last sheet tried first;
/// 2. else where it costs least, if that is less than the length it would add at the end;
/// 3. whatever is left is nested together with the last sheet, or onto further sheets.
fn place_small(
    demands: &[Demand],
    params: &NestParams,
    prep: &Prepared,
    sheets: &mut Vec<Sheet>,
    left: &mut [usize],
    next_seed: &mut dyn FnMut() -> u64,
    report: &mut dyn FnMut(usize, String, &[Placement]),
) -> Result<()> {
    let eps = 1e-6;
    let area = |i: usize| demands[i].part.shape.area();
    let cost = |pl: &[Placement]| sheet_cost(demands, params, pl);
    let mut order: Vec<usize> = (0..left.len()).filter(|&i| left[i] > 0).collect();
    order.sort_by(|&a, &b| area(b).total_cmp(&area(a)));
    for c in order {
        while left[c] > 0 {
            // Length the part would add at the end of a sheet.
            let at_end = area(c) / (params.width * TARGET_DENSITY);
            let mut best: Option<(f64, usize, Placement)> = None;
            for pass in [1, 2] {
                for n in (0..sheets.len()).rev() {
                    let sheet = &sheets[n];
                    let blocked = if pass == 1 { params.reusable_regions(demands, &sheet.placements, sheet.used_length) } else { Vec::new() };
                    let mut one = vec![0usize; demands.len()];
                    one[c] = 1;
                    let Some(p) = prep.insert_fillers(&sheet.placements, &mut one, params, sheet.used_length, params.width, &blocked, next_seed())?.pop() else {
                        continue;
                    };
                    let mut after = sheet.placements.clone();
                    after.push(p.clone());
                    let delta = cost(&after) - cost(&sheet.placements);
                    let ok = if pass == 1 { delta <= eps } else { delta < at_end };
                    if ok && best.as_ref().is_none_or(|b| delta < b.0 - eps) {
                        best = Some((delta, n, p));
                        if pass == 1 {
                            break;
                        }
                    }
                }
                if best.is_some() {
                    break;
                }
            }
            let Some((delta, n, p)) = best else { break };
            info!("{}: into a hole on sheet {} (+{delta:.1} mm)", demands[c].part.name, n + 1);
            sheets[n].placements.push(p);
            report(n + 1, format!("small +{}", demands[c].part.name), &sheets[n].placements);
            left[c] -= 1;
        }
    }
    if left.iter().sum::<usize>() == 0 {
        return Ok(());
    }

    // The rest: nested together with the last sheet if that still fits, else onto new sheets.
    let last = sheets.len() - 1;
    let mut counts = left.to_vec();
    for p in &sheets[last].placements {
        counts[p.part] += 1;
    }
    let trial = prep.strip(&counts, params, params.length, params.budget, next_seed())?;
    report(last + 1, "small at the end".into(), &trial);
    let len = trial.iter().map(|p| demands[p.part].part.shape.transformed(&p.transform).bbox().max.x).fold(0.0, f64::max);
    if len <= params.length + eps && trial.len() == counts.iter().sum::<usize>() {
        info!("{} small parts nested with the last sheet", left.iter().sum::<usize>());
        let mut pl = trial;
        crate::compact::slide(demands, &mut pl, params.gap);
        sheets[last].used_length = len_of(demands, &pl);
        sheets[last].placements = pl;
    } else {
        info!("{} small parts go onto further sheets", left.iter().sum::<usize>());
        sheets.extend(make_sheets(demands, params, prep, left.to_vec(), next_seed, report)?);
    }
    left.iter_mut().for_each(|n| *n = 0);
    Ok(())
}

/// Moves small parts between finished sheets: a part is taken off its sheet (whose parts then slide
/// left and down) and put into a hole of another sheet, the last one tried first, if the fabric
/// consumed by both sheets together goes down: a wider free band on top, a shorter sheet.
fn relocate_small(
    demands: &[Demand],
    params: &NestParams,
    prep: &Prepared,
    sheets: &mut [Sheet],
    small: &[bool],
    next_seed: &mut dyn FnMut() -> u64,
    report: &mut dyn FnMut(usize, String, &[Placement]),
) -> Result<()> {
    let eps = 1e-6;
    let cost = |pl: &[Placement]| sheet_cost(demands, params, pl);
    let area = |i: usize| demands[i].part.shape.area();
    for i in 0..sheets.len() {
        // Smallest first: they are the likeliest to find a hole elsewhere.
        let mut k = 0;
        loop {
            let mut cands: Vec<usize> = (0..sheets[i].placements.len()).filter(|&k| small[sheets[i].placements[k].part]).collect();
            cands.sort_by(|&a, &b| area(sheets[i].placements[a].part).total_cmp(&area(sheets[i].placements[b].part)));
            let Some(&pk) = cands.get(k) else { break };
            let c = sheets[i].placements[pk].part;
            let mut without = sheets[i].placements.clone();
            without.remove(pk);
            crate::compact::slide(demands, &mut without, params.gap);
            let gain = cost(&sheets[i].placements) - cost(&without);
            let mut moved = false;
            if gain > eps {
                for j in (0..sheets.len()).rev().filter(|&j| j != i) {
                    let blocked = params.reusable_regions(demands, &sheets[j].placements, sheets[j].used_length);
                    let mut one = vec![0usize; demands.len()];
                    one[c] = 1;
                    let Some(p) = prep.insert_fillers(&sheets[j].placements, &mut one, params, sheets[j].used_length, params.width, &blocked, next_seed())?.pop() else {
                        continue;
                    };
                    let mut with = sheets[j].placements.clone();
                    with.push(p);
                    if cost(&with) - cost(&sheets[j].placements) < gain - eps {
                        info!("{}: moved from sheet {} to sheet {} ({:.1} mm less)", demands[c].part.name, i + 1, j + 1, gain - (cost(&with) - cost(&sheets[j].placements)));
                        sheets[j].placements = with;
                        report(j + 1, format!("moved in {}", demands[c].part.name), &sheets[j].placements);
                        sheets[i].used_length = len_of(demands, &without);
                        sheets[i].placements = without;
                        report(i + 1, format!("moved out {}", demands[c].part.name), &sheets[i].placements);
                        moved = true;
                        break;
                    }
                }
            }
            if !moved {
                k += 1;
            }
        }
    }
    Ok(())
}

/// Used length of a layout: the rightmost point of any part.
fn len_of(demands: &[Demand], pl: &[Placement]) -> f64 {
    pl.iter().map(|p| demands[p.part].part.shape.transformed(&p.transform).bbox().max.x).fold(0.0, f64::max)
}

/// Fabric a sheet consumes plus the tilt penalty, mm: what the polishing passes minimise.
fn sheet_cost(demands: &[Demand], params: &NestParams, pl: &[Placement]) -> f64 {
    let tilt: f64 = pl.iter().map(|p| params.tilt_cost(&demands[p.part].part.name, p.transform.rotation_deg)).sum();
    params.consumed(demands, pl, len_of(demands, pl)) + tilt
}

/// Solver-ready items: one per demand, `None` for parts that fit the sheet at no allowed angle.
struct Prepared<'a> {
    demands: &'a [Demand],
    items: Vec<Option<BaseItem>>,
    angles: Vec<f64>,
    /// Shift that moves each part's bbox to the origin before it goes to the solver.
    offsets: Vec<Pt>,
    /// Rotation penalties for sparrow, `None` if no part is penalised.
    tilt: Option<&'static TiltPenalty>,
}

fn prepare<'a>(demands: &'a [Demand], params: &NestParams) -> (Prepared<'a>, Vec<Unplaced>) {
    let angles = params.allowed_angles();
    // Parts are moved so their bbox starts at the origin: jagua works in f32
    // and large drawing coordinates would eat the precision.
    let offsets: Vec<Pt> = demands.iter().map(|d| d.part.shape.bbox().min).collect();
    let mut unplaced = Vec::new();
    let mut items = Vec::new();
    let mut tilt = TiltPenalty::default();
    for (idx, d) in demands.iter().enumerate() {
        let fitting: Vec<f64> = angles
            .iter()
            .copied()
            .filter(|&r| !params.is_panel(&d.part.name) || params.tilt(r) <= params.panel_tolerance + 1e-9)
            .filter(|&r| {
                let b = rotated_bbox(d, r);
                b.height() <= params.width + 1e-6 && b.width() <= params.length + 1e-6
            })
            .collect();
        if fitting.is_empty() && d.filler {
            items.push(None);
            tilt.items.push(None);
            continue;
        }
        if fitting.is_empty() {
            unplaced.push(Unplaced {
                part: idx,
                qty: d.qty,
                reason: format!("does not fit a {} x {} mm sheet at any allowed angle", params.width, params.length),
            });
            items.push(None);
            tilt.items.push(None);
            continue;
        }
        let make = |ring: Vec<(f32, f32)>| BaseItem {
            id: idx as u64,
            orientation: ExtOrientation {
                rotation: ExtRotation::Discrete { angles: fitting.iter().map(|&r| r as f32).collect() },
                reflection_axes: vec![],
            },
            shape: ExtShape::SimplePolygon(ExtSPolygon(ring)),
            min_quality: None,
        };
        // jagua-rs offsets every item by half the gap, which can make tiny notches self-intersect.
        // Check each item the way the solver will import it; a rejected outline is replaced by a
        // smoothed envelope 0.5 mm larger, and only if that fails too is the part left out.
        let importer = Importer::new(DEFAULT_SPARROW_CONFIG.cde_config, DEFAULT_SPARROW_CONFIG.poly_simpl_tolerance, DEFAULT_SPARROW_CONFIG.narrow_concavity_cutoff_ratio);
        let importer = importer.with_min_item_separation(params.gap as f32).unwrap_or(importer);
        let accepted = |it: &BaseItem| importer.import_item(it, 0).map(|_| ()).map_err(|e| format!("{e:#}"));
        let item = make(solver_ring(&d.part.name, &d.part.shape.outer, offsets[idx]));
        let item = match accepted(&item) {
            Ok(()) => item,
            Err(first) => {
                let robust = make(envelope_ring(&d.part.shape.outer, offsets[idx], 0.5));
                match accepted(&robust) {
                    Ok(()) => {
                        warn!("{}: outline rejected by jagua-rs ({first}); nesting uses an envelope 0.5 mm larger", d.part.name);
                        robust
                    }
                    Err(e) => {
                        unplaced.push(Unplaced { part: idx, qty: d.qty, reason: format!("outline rejected by the solver: {e}") });
                        items.push(None);
                        tilt.items.push(None);
                        continue;
                    }
                }
            }
        };
        items.push(Some(item));
        let costs: Vec<(f32, f32)> = fitting.iter().map(|&r| ((r.to_radians()) as f32, params.tilt_cost(&d.part.name, r) as f32)).collect();
        tilt.items.push(costs.iter().any(|c| c.1 > 0.0).then_some(costs));
    }
    // One small table per job, shared by all solver threads for the rest of the process: sparrow's
    // configs are `Copy`, so it takes a `&'static`.
    let tilt = tilt.items.iter().any(Option::is_some).then(|| &*Box::leak(Box::new(tilt)));
    (Prepared { demands, items, angles, offsets, tilt }, unplaced)
}

impl Prepared<'_> {
    /// One sparrow strip run over `counts[i]` copies of each part; returns placements in sheet coordinates.
    /// Of the layouts sparrow found, one no longer than `max_len` is preferred (least fabric
    /// consumed + tilt penalty).
    fn strip(&self, counts: &[usize], params: &NestParams, max_len: f64, budget: Budget, seed: u64) -> Result<Vec<Placement>> {
        let g = params.gap;
        let items: Vec<ExtItem> = counts
            .iter()
            .enumerate()
            .filter(|&(_, &c)| c > 0)
            .map(|(i, &c)| ExtItem { base: self.items[i].clone().expect("unplaceable part requested"), demand: c as u64 })
            .collect();
        // jagua inflates parts by g/2 and deflates the strip by g/2, giving g at the edge too.
        // So the strip is made 2g taller and everything is shifted by −g, letting parts reach the edge.
        let ext_instance = ExtSPInstance {
            name: "nest".into(),
            min_item_separation: g as f32,
            items,
            strip_height: (params.width + 2.0 * g) as f32,
        };
        let names: Vec<&str> = self.demands.iter().map(|d| d.part.name.as_str()).collect();
        // jagua-rs / sparrow panic on some degenerate geometry; turn that into an error instead of aborting.
        let solutions = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_sparrow(&ext_instance, &names, params, budget, seed, self.tilt)
        }))
        .map_err(|p| {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            anyhow::anyhow!("internal solver error: {msg}")
        })??;

        let mut best: Option<(bool, f64, Vec<Placement>)> = None;
        for sol in &solutions {
            let pl = self.placements(sol, g)?;
            let len = pl.iter().map(|p| self.demands[p.part].part.shape.transformed(&p.transform).bbox().max.x).fold(0.0, f64::max);
            // Fabric consumed (free space open to the top or the right end counts less) + tilt penalty.
            let cost = params.consumed(self.demands, &pl, len)
                + pl.iter().map(|p| params.tilt_cost(&self.demands[p.part].part.name, p.transform.rotation_deg)).sum::<f64>();
            // A layout that fits the sheet beats one that does not, whatever the penalty.
            let fits = len <= max_len + 1e-6;
            if best.as_ref().is_none_or(|b| (fits, -cost) > (b.0, -b.1)) {
                best = Some((fits, cost, pl));
            }
        }
        let placements = best.context("sparrow returned no layout")?.2;
        let wanted: usize = counts.iter().sum();
        if placements.len() != wanted {
            bail!("the solver placed {} of {wanted} parts", placements.len());
        }
        Ok(placements)
    }

    /// Placements of a sparrow solution in sheet coordinates.
    fn placements(&self, solution: &SPSolution, g: f64) -> Result<Vec<Placement>> {
        let mut placements =
            solution.layout_snapshot().placed_items().values().map(|pi| self.placement(pi, g)).collect::<Result<Vec<_>>>()?;
        placements.sort_by_key(|p| p.part);
        Ok(placements)
    }

    /// A placed solver item in sheet coordinates.
    fn placement(&self, pi: &PlacedItem, g: f64) -> Result<Placement> {
        let part = pi.item().external_id() as usize;
        let ext = jagua_rs::io::export::int_to_ext_transformation(&pi.d_transf(), &pi.item().shape_orig().pre_transform);
        if ext.reflected {
            bail!("the solver returned a mirrored part although mirroring was not allowed");
        }
        let rot = f64::from(ext.rotation()).to_degrees();
        let (tx, ty) = ext.translation();
        // Result: R·(p − offset) + t − (g, g).
        let r = Transform { rotation_deg: rot, translation: Pt::new(0.0, 0.0) }.apply(self.offsets[part]);
        Ok(Placement {
            part,
            transform: Transform {
                rotation_deg: snap_angle(rot, &self.angles),
                translation: Pt::new(f64::from(tx) - r.x - g, f64::from(ty) - r.y - g),
            },
        })
    }

    /// Puts fillers into the free space of a finished sheet, leaving its parts where they are,
    /// staying within `max_len` and below `height` and out of the `blocked` rectangles.
    /// `available[i]` is how many copies of part `i` may be added; it is
    /// reduced by what was placed. Largest fillers first, each as often as it fits. Returns the
    /// added placements.
    fn insert_fillers(
        &self,
        placed: &[Placement],
        available: &mut [usize],
        params: &NestParams,
        max_len: f64,
        height: f64,
        blocked: &[BBox],
        seed: u64,
    ) -> Result<Vec<Placement>> {
        let g = params.gap;
        let mut counts = available.to_vec();
        for p in placed {
            counts[p.part] += 1;
        }
        let mut items: Vec<ExtItem> = counts
            .iter()
            .enumerate()
            .filter(|&(i, &c)| c > 0 && self.items[i].is_some())
            .map(|(i, &c)| ExtItem { base: self.items[i].clone().unwrap(), demand: c as u64 })
            .collect();
        // Blocked rectangles are items of their own (ids after the parts), placed where they are.
        let blocker_id = |k: usize| (self.demands.len() + k) as u64;
        for (k, b) in blocked.iter().enumerate() {
            let (w, h) = (b.width() as f32, b.height() as f32);
            items.push(ExtItem {
                base: BaseItem {
                    id: blocker_id(k),
                    orientation: ExtOrientation { rotation: ExtRotation::Discrete { angles: vec![0.0] }, reflection_axes: vec![] },
                    shape: ExtShape::SimplePolygon(ExtSPolygon(vec![(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)])),
                    min_quality: None,
                },
                demand: 1,
            });
        }
        let ext = ExtSPInstance { name: "fill".into(), min_item_separation: g as f32, items, strip_height: (height + 2.0 * g) as f32 };
        let config = DEFAULT_SPARROW_CONFIG;
        let importer = Importer::new(config.cde_config, config.poly_simpl_tolerance, config.narrow_concavity_cutoff_ratio);
        let instance = jagua_rs::probs::spp::io::import_instance(&importer, &ext).context("jagua-rs rejected the filler job")?;
        let mut prob = SPProblem::new(instance)?;
        // Same frame as `strip`: solver coordinates are sheet coordinates + (g, g).
        prob.change_strip_width((max_len + 2.0 * g) as f32)?;

        // The sheet as it is. Its parts may touch within f32 noise; only the fillers are checked.
        for p in placed {
            let idx = prob.instance().item_idx(p.part as u64).context("sheet part missing from the filler job")?;
            let item = prob.instance().item(idx).clone();
            let r = Transform { rotation_deg: p.transform.rotation_deg, translation: Pt::new(0.0, 0.0) }.apply(self.offsets[p.part]);
            let t = (p.transform.translation.x + r.x + g, p.transform.translation.y + r.y + g);
            let ext_t = DTransformation::new(p.transform.rotation_deg.to_radians() as f32, (t.0 as f32, t.1 as f32));
            let d_transf = jagua_rs::io::import::ext_to_int_transformation(&ext_t, &item.shape_orig().pre_transform);
            prob.place_item(SPPlacement { item_idx: idx, d_transf });
        }
        for (k, b) in blocked.iter().enumerate() {
            let idx = prob.instance().item_idx(blocker_id(k)).context("blocker missing from the filler job")?;
            let item = prob.instance().item(idx).clone();
            let ext_t = DTransformation::new(0.0, ((b.min.x + g) as f32, (b.min.y + g) as f32));
            let d_transf = jagua_rs::io::import::ext_to_int_transformation(&ext_t, &item.shape_orig().pre_transform);
            prob.place_item(SPPlacement { item_idx: idx, d_transf });
        }

        let mut order: Vec<usize> = (0..available.len()).filter(|&i| available[i] > 0 && self.items[i].is_some()).collect();
        order.sort_by(|&a, &b| self.demands[b].part.shape.area().total_cmp(&self.demands[a].part.shape.area()));
        let sample_config = SampleConfig { tilt: self.tilt, ..LBF_SAMPLE_CONFIG };
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(seed);
        let mut added = Vec::new();
        for part in order {
            let Some(idx) = prob.instance().item_idx(part as u64) else { continue };
            while available[part] > 0 {
                let item = prob.instance().item(idx).clone();
                let (best, _) = search_placement(prob.layout(), &item, None, LBFEvaluator::new(prob.layout(), &item), sample_config, &mut rng);
                let Some((d_transf, SampleEval::Clear { .. })) = best else { break };
                let pk = prob.place_item(SPPlacement { item_idx: idx, d_transf });
                added.push(self.placement(&prob.layout().placed_items()[pk], g)?);
                available[part] -= 1;
            }
        }
        Ok(added)
    }
}

/// Outer contour in f32 relative to `origin`, in a form jagua-rs accepts.
///
/// Rounding to f32 can merge nearly coincident points and self-touching outlines repeat a vertex;
/// such points are dropped. A self-intersecting outline is replaced by a 0.1 mm buffer around it,
/// simplified by 0.05 mm: valid and never smaller than the original. The independent validation
/// still checks the exact geometry.
/// Outline grown by `grow` mm and simplified by a fifth of that: valid, slightly larger, without
/// notches narrower than about `2 * grow`.
fn envelope_ring(ring: &[Pt], origin: Pt, grow: f64) -> Vec<(f32, f32)> {
    use geo::{Area, Buffer, Simplify};
    let poly = geo::Polygon::new(ring.iter().map(|p| (p.x - origin.x, p.y - origin.y)).collect::<Vec<_>>().into(), vec![]);
    let grown = poly.buffer(grow);
    let Some(best) = grown.0.iter().max_by(|a, b| a.unsigned_area().total_cmp(&b.unsigned_area())) else {
        return Vec::new();
    };
    let simple = geo::Polygon::new(best.exterior().clone(), vec![]).simplify(grow / 5.0);
    let mut seen = std::collections::HashSet::new();
    let mut pts = simple.exterior().0.clone();
    pts.pop();
    pts.iter()
        .map(|c| (c.x as f32, c.y as f32))
        .filter(|q| seen.insert(((q.0 * 1000.0).round() as i64, (q.1 * 1000.0).round() as i64)))
        .collect()
}

fn solver_ring(name: &str, ring: &[Pt], origin: Pt) -> Vec<(f32, f32)> {
    use geo::{Area, Buffer, Simplify, Validation};
    // Drops points closer than 1 µm to an already kept one (f32 noise, repeated vertices).
    let clean = |pts: &mut dyn Iterator<Item = (f64, f64)>| {
        let mut seen = std::collections::HashSet::new();
        pts.map(|(x, y)| (x as f32, y as f32))
            .filter(|q| seen.insert(((q.0 * 1000.0).round() as i64, (q.1 * 1000.0).round() as i64)))
            .collect::<Vec<_>>()
    };
    let out = clean(&mut ring.iter().map(|p| (p.x - origin.x, p.y - origin.y)));
    let as_poly = |pts: &[(f32, f32)]| {
        geo::Polygon::new(pts.iter().map(|&(x, y)| (f64::from(x), f64::from(y))).collect::<Vec<_>>().into(), vec![])
    };
    let poly = as_poly(&out);
    if poly.is_valid() {
        return out;
    }
    let grown = poly.buffer(0.1);
    let Some(best) = grown.0.iter().max_by(|a, b| a.unsigned_area().total_cmp(&b.unsigned_area())) else {
        return out;
    };
    let simple = geo::Polygon::new(best.exterior().clone(), vec![]).simplify(0.05);
    let mut pts = simple.exterior().0.clone();
    pts.pop();
    let fixed = clean(&mut pts.iter().map(|c| (c.x, c.y)));
    if as_poly(&fixed).is_valid() {
        warn!("{name}: the outer contour self-intersects; nesting uses its outline grown by 0.1 mm");
        fixed
    } else {
        warn!("{name}: the outer contour self-intersects and cannot be repaired automatically");
        out
    }
}

fn rotated_bbox(d: &Demand, rot: f64) -> BBox {
    let t = Transform { rotation_deg: rot, translation: Pt::new(0.0, 0.0) };
    BBox::of(d.part.shape.outer.iter().map(|&p| t.apply(p)))
}

/// Snaps the solver's f32 angle to the nearest allowed one (removes noise like 179.99998°).
fn snap_angle(rot: f64, allowed: &[f64]) -> f64 {
    let norm = |a: f64| a.rem_euclid(360.0);
    allowed
        .iter()
        .copied()
        .min_by(|a, b| {
            let da = (norm(rot - a) + 180.0).rem_euclid(360.0) - 180.0;
            let db = (norm(rot - b) + 180.0).rem_euclid(360.0) - 180.0;
            da.abs().total_cmp(&db.abs())
        })
        .filter(|a| ((norm(rot - a) + 180.0).rem_euclid(360.0) - 180.0).abs() < 0.01)
        .unwrap_or(rot)
}

/// Runs sparrow. Returns its final layout; with a tilt table, also every feasible layout found on the
/// way (longer ones may be less tilted), for the caller to weigh length against tilt.
fn run_sparrow(
    ext: &ExtSPInstance,
    names: &[&str],
    params: &NestParams,
    budget: Budget,
    seed: u64,
    tilt: Option<&'static TiltPenalty>,
) -> Result<Vec<SPSolution>> {
    let mut config = DEFAULT_SPARROW_CONFIG;
    config.expl_cfg.separator_config.n_workers = params.workers.max(1);
    config.cmpr_cfg.separator_config.n_workers = params.workers.max(1);
    config.expl_cfg.separator_config.sample_config.tilt = tilt;
    config.cmpr_cfg.separator_config.sample_config.tilt = tilt;
    let cancel = params.cancel.clone().unwrap_or_default();

    let importer = Importer::new(config.cde_config, config.poly_simpl_tolerance, config.narrow_concavity_cutoff_ratio);
    let instance = match jagua_rs::probs::spp::io::import_instance(&importer, ext) {
        Ok(i) => i,
        Err(e) => {
            // Re-import item by item to name the offending part.
            let bad = ext.items.iter().enumerate().find_map(|(i, it)| importer.import_item(&it.base, i).err().map(|e| (it.base.id, e)));
            return Err(match bad {
                Some((id, e)) => anyhow::anyhow!("jagua-rs rejected part {}: {e:#}", names[id as usize]),
                None => e.context("jagua-rs rejected the job"),
            });
        }
    };
    let rng = Xoshiro256PlusPlus::seed_from_u64(seed);
    let mut listener = ProgressListener { keep: tilt.is_some(), feasible: Vec::new() };

    match budget {
        Budget::Time(total) => {
            config.expl_cfg.time_limit = total.mul_f32(DEFAULT_EXPLORE_TIME_RATIO);
            config.cmpr_cfg.time_limit = total.mul_f32(DEFAULT_COMPRESS_TIME_RATIO);
            let mut term = TimeTerminator { deadline: None, cancel };
            optimize(instance, rng, &mut listener, &mut term, &config.expl_cfg, &config.cmpr_cfg, None)
        }
        Budget::Iterations(total) => {
            // Compression decays by the clock by default; failure-based decay keeps it reproducible.
            config.cmpr_cfg.shrink_decay = ShrinkDecayStrategy::FailureBased(DEFAULT_FAIL_DECAY_RATIO_CMPR);
            let explore = ((total as f32) * DEFAULT_EXPLORE_TIME_RATIO).round() as usize;
            let mut term = IterTerminator::new(vec![explore, total.saturating_sub(explore)], cancel);
            optimize(instance, rng, &mut listener, &mut term, &config.expl_cfg, &config.cmpr_cfg, None)
        }
    }
    .context("sparrow could not build a layout")
    .map(|last| {
        let mut all = listener.feasible;
        all.push(last);
        all
    })
}

struct ProgressListener {
    /// Whether to keep the feasible layouts.
    keep: bool,
    feasible: Vec<SPSolution>,
}

impl SolutionListener for ProgressListener {
    fn report(&mut self, report: ReportType, solution: &SPSolution) {
        if matches!(report, ReportType::ExplFeas | ReportType::CmprFeas) {
            if self.keep {
                self.feasible.push(solution.clone());
            }
            info!(
                "[{}] feasible layout: length {:.1} mm, density {:.2}%",
                if report == ReportType::ExplFeas { "explore" } else { "compress" },
                solution.strip_width(),
                solution.density() * 100.0
            );
        }
    }
}

/// Stops on the phase deadline or on external cancel. Cancel resets on phase change,
/// like sparrow's Ctrl-C: the first cancel ends exploration, the second compression.
struct TimeTerminator {
    deadline: Option<Instant>,
    cancel: Arc<AtomicBool>,
}

impl Terminator for TimeTerminator {
    fn kill(&self) -> bool {
        self.cancel.load(Ordering::SeqCst) || self.deadline.is_some_and(|d| Instant::now() > d)
    }

    fn new_timeout(&mut self, timeout: Duration) {
        self.cancel.store(false, Ordering::SeqCst);
        self.deadline = Some(Instant::now() + timeout);
    }

    fn timeout_at(&self) -> Option<Instant> {
        self.deadline
    }
}

/// Stops after a fixed number of `kill()` calls per phase (exploration, compression).
/// The clock is not involved, so the same seed and worker count give the same result.
struct IterTerminator {
    phase_budgets: Vec<usize>,
    next_phase: usize,
    remaining: AtomicUsize,
    cancel: Arc<AtomicBool>,
}

impl IterTerminator {
    fn new(phase_budgets: Vec<usize>, cancel: Arc<AtomicBool>) -> Self {
        Self { phase_budgets, next_phase: 0, remaining: AtomicUsize::new(0), cancel }
    }
}

impl Terminator for IterTerminator {
    fn kill(&self) -> bool {
        if self.cancel.load(Ordering::SeqCst) {
            return true;
        }
        // fetch_update is deprecated in newer Rust, but try_update is not in MSRV 1.90.
        #[allow(deprecated)]
        let prev = self
            .remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |r| Some(r.saturating_sub(1)));
        matches!(prev, Ok(0))
    }

    fn new_timeout(&mut self, _timeout: Duration) {
        self.cancel.store(false, Ordering::SeqCst);
        let budget = self.phase_budgets.get(self.next_phase).copied().unwrap_or(0);
        self.next_phase += 1;
        self.remaining.store(budget, Ordering::SeqCst);
    }

    fn timeout_at(&self) -> Option<Instant> {
        None
    }
}
