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
use sparrow::util::listener::{ReportType, SolutionListener};
use sparrow::util::terminator::Terminator;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
    pub budget: Budget,
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
            budget: Budget::Time(Duration::from_secs(60)),
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
/// sparrow only solves strip packing, so sheets are filled one at a time on top of it:
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
pub fn nest_observed(demands: &[Demand], params: &NestParams, observer: &mut dyn FnMut(&Step)) -> Result<NestOutcome> {
    /// Probes per sheet; each gets an equal share of half the sheet budget.
    const MAX_PROBES: u32 = 8;
    /// A sheet is not expected to be packed denser than this, which bounds the probes worth trying.
    const MAX_DENSITY: f64 = 0.92;

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

    let mut remaining: Vec<usize> = (0..demands.len()).map(|i| if prep.items[i].is_some() { demands[i].qty } else { 0 }).collect();
    let mut sheets: Vec<Sheet> = Vec::new();
    let mut run = 0u64;
    let mut next_seed = || {
        run += 1;
        seed.wrapping_add(run.wrapping_mul(0x9E37_79B9_7F4A_7C15))
    };
    let bbox = |p: &Placement| demands[p.part].part.shape.transformed(&p.transform).bbox();
    let len_of = |pl: &[Placement]| pl.iter().map(|p| bbox(p).max.x).fold(0.0, f64::max);
    let area = |i: usize| demands[i].part.shape.area();
    let eps = 1e-6;
    let fits = |pl: &[Placement]| len_of(pl) <= params.length + eps;
    let mut report = |sheet: usize, kind: String, pl: &[Placement]| {
        observer(&Step { sheet, kind, placements: pl.to_vec(), used_length: len_of(pl), fits: fits(pl) });
    };

    while remaining.iter().sum::<usize>() > 0 {
        let sheet_no = sheets.len() + 1;
        let strip = prep.strip(&remaining, params, params.budget.share(1, 2), next_seed())?;
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
            let trial = prep.strip(&counts, params, params.budget.share(1, 2 * MAX_PROBES), next_seed())?;
            report(sheet_no, format!("probe +{}", demands[c].part.name), &trial);
            if fits(&trial) {
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
        let compact = prep.strip(&counts, params, params.budget.share(1, 2 * MAX_PROBES), next_seed())?;
        report(sheet_no, "compact".into(), &compact);
        if fits(&compact) && len_of(&compact) < len_of(&chosen) {
            chosen = compact;
        }

        for p in &chosen {
            remaining[p.part] -= 1;
        }
        info!("sheet {sheet_no}: {} parts, used length {:.1} mm, {} left", chosen.len(), len_of(&chosen), remaining.iter().sum::<usize>());
        report(sheet_no, "sheet".into(), &chosen);
        sheets.push(Sheet { width: params.width, length: params.length, used_length: len_of(&chosen), placements: chosen });
    }

    Ok(NestOutcome { result: NestResult { sheets, unplaced }, seed, elapsed: started.elapsed() })
}

/// Solver-ready items: one per demand, `None` for parts that fit the sheet at no allowed angle.
struct Prepared<'a> {
    demands: &'a [Demand],
    items: Vec<Option<BaseItem>>,
    angles: Vec<f64>,
    /// Shift that moves each part's bbox to the origin before it goes to the solver.
    offsets: Vec<Pt>,
}

fn prepare<'a>(demands: &'a [Demand], params: &NestParams) -> (Prepared<'a>, Vec<Unplaced>) {
    let angles = params.allowed_angles();
    // Parts are moved so their bbox starts at the origin: jagua works in f32
    // and large drawing coordinates would eat the precision.
    let offsets: Vec<Pt> = demands.iter().map(|d| d.part.shape.bbox().min).collect();
    let mut unplaced = Vec::new();
    let mut items = Vec::new();
    for (idx, d) in demands.iter().enumerate() {
        let fitting: Vec<f64> = angles
            .iter()
            .copied()
            .filter(|&r| {
                let b = rotated_bbox(d, r);
                b.height() <= params.width + 1e-6 && b.width() <= params.length + 1e-6
            })
            .collect();
        if fitting.is_empty() {
            unplaced.push(Unplaced {
                part: idx,
                qty: d.qty,
                reason: format!("does not fit a {} x {} mm sheet at any allowed angle", params.width, params.length),
            });
            items.push(None);
            continue;
        }
        items.push(Some(BaseItem {
            id: idx as u64,
            orientation: ExtOrientation {
                rotation: ExtRotation::Discrete { angles: fitting.iter().map(|&r| r as f32).collect() },
                reflection_axes: vec![],
            },
            shape: ExtShape::SimplePolygon(ExtSPolygon(solver_ring(&d.part.name, &d.part.shape.outer, offsets[idx]))),
            min_quality: None,
        }));
    }
    (Prepared { demands, items, angles, offsets }, unplaced)
}

impl Prepared<'_> {
    /// One sparrow strip run over `counts[i]` copies of each part; returns placements in sheet coordinates.
    fn strip(&self, counts: &[usize], params: &NestParams, budget: Budget, seed: u64) -> Result<Vec<Placement>> {
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
        let solution = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_sparrow(&ext_instance, &names, params, budget, seed)
        }))
        .map_err(|p| {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            anyhow::anyhow!("internal solver error: {msg}")
        })??;

        let mut placements = Vec::new();
        for pi in solution.layout_snapshot().placed_items().values() {
            let part = pi.item().external_id() as usize;
            let ext = jagua_rs::io::export::int_to_ext_transformation(&pi.d_transf(), &pi.item().shape_orig().pre_transform);
            if ext.reflected {
                bail!("the solver returned a mirrored part although mirroring was not allowed");
            }
            let rot = f64::from(ext.rotation()).to_degrees();
            let (tx, ty) = ext.translation();
            // Result: R·(p − offset) + t − (g, g).
            let r = Transform { rotation_deg: rot, translation: Pt::new(0.0, 0.0) }.apply(self.offsets[part]);
            placements.push(Placement {
                part,
                transform: Transform {
                    rotation_deg: snap_angle(rot, &self.angles),
                    translation: Pt::new(f64::from(tx) - r.x - g, f64::from(ty) - r.y - g),
                },
            });
        }
        placements.sort_by_key(|p| p.part);
        let wanted: usize = counts.iter().sum();
        if placements.len() != wanted {
            bail!("the solver placed {} of {wanted} parts", placements.len());
        }
        Ok(placements)
    }
}

/// Outer contour in f32 relative to `origin`, in a form jagua-rs accepts.
///
/// Rounding to f32 can merge nearly coincident points and self-touching outlines repeat a vertex;
/// such points are dropped. A self-intersecting outline is replaced by a 0.1 mm buffer around it,
/// simplified by 0.05 mm: valid and never smaller than the original. The independent validation
/// still checks the exact geometry.
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

fn run_sparrow(ext: &ExtSPInstance, names: &[&str], params: &NestParams, budget: Budget, seed: u64) -> Result<SPSolution> {
    let mut config = DEFAULT_SPARROW_CONFIG;
    config.expl_cfg.separator_config.n_workers = params.workers.max(1);
    config.cmpr_cfg.separator_config.n_workers = params.workers.max(1);
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
    let mut listener = ProgressListener;

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
}

struct ProgressListener;

impl SolutionListener for ProgressListener {
    fn report(&mut self, report: ReportType, solution: &SPSolution) {
        if matches!(report, ReportType::ExplFeas | ReportType::CmprFeas) {
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
