//! Обёртка над sparrow / jagua-rs: задание → `ExtSPInstance` → оптимизация → размещения в координатах DXF.
//!
//! Соответствие осей: ширина ткани = высота полосы jagua (Y), длина отреза = ширина полосы (X).

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

/// Когда останавливать поиск.
#[derive(Debug, Clone, Copy)]
pub enum Budget {
    /// По времени. С одинаковым seed результат может отличаться от прогона к прогону.
    Time(Duration),
    /// По числу проверок условия остановки: полностью воспроизводимо при том же seed и числе потоков.
    Iterations(usize),
}

#[derive(Debug, Clone)]
pub struct NestParams {
    /// Ширина ткани, мм.
    pub width: f64,
    /// Максимальная длина отреза, мм (превышение даёт предупреждение).
    pub max_length: f64,
    /// Минимальный зазор между деталями, мм. До края ткани зазор не добавляется.
    pub gap: f64,
    /// Разрешённые углы поворота, градусы.
    pub rotations: Vec<f64>,
    pub budget: Budget,
    pub seed: Option<u64>,
    /// Число потоков сепаратора sparrow.
    pub workers: usize,
    /// Внешняя отмена (Ctrl-C в CLI, кнопка в GUI): поиск переходит к следующей фазе / завершается.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl Default for NestParams {
    fn default() -> Self {
        Self {
            width: 1560.0,
            max_length: 5800.0,
            gap: 0.0,
            rotations: vec![0.0, 180.0],
            budget: Budget::Time(Duration::from_secs(60)),
            seed: None,
            workers: DEFAULT_SPARROW_CONFIG.expl_cfg.separator_config.n_workers,
            cancel: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NestOutcome {
    pub result: NestResult,
    /// Фактически использованный seed (сгенерированный, если не был задан).
    pub seed: u64,
    pub elapsed: Duration,
}

pub fn nest(demands: &[Demand], params: &NestParams) -> Result<NestOutcome> {
    let started = Instant::now();
    if !(params.width > 0.0 && params.gap >= 0.0 && params.gap < params.width) {
        bail!("неверные параметры: ширина {} мм, зазор {} мм", params.width, params.gap);
    }
    if params.rotations.is_empty() {
        bail!("список разрешённых углов пуст");
    }
    let seed = params.seed.unwrap_or_else(rand::random);

    // Детали переносятся так, чтобы левый нижний угол bbox был в нуле: jagua работает в f32,
    // и большие координаты чертежа съели бы точность.
    let offsets: Vec<Pt> = demands.iter().map(|d| d.part.shape.bbox().min).collect();

    let mut unplaced = Vec::new();
    let mut items = Vec::new();
    for (idx, d) in demands.iter().enumerate() {
        let fitting: Vec<f64> = params
            .rotations
            .iter()
            .copied()
            .filter(|&r| rotated_height(d, r) <= params.width + 1e-6)
            .collect();
        if fitting.is_empty() {
            unplaced.push(Unplaced {
                part: idx,
                qty: d.qty,
                reason: format!(
                    "не помещается в ширину {} мм ни при одном разрешённом угле",
                    params.width
                ),
            });
            continue;
        }
        let o = offsets[idx];
        let pts = solver_ring(&d.part.name, &d.part.shape.outer, o);
        items.push(ExtItem {
            base: BaseItem {
                id: idx as u64,
                orientation: ExtOrientation {
                    rotation: ExtRotation::Discrete { angles: fitting.iter().map(|&r| r as f32).collect() },
                    reflection_axes: vec![],
                },
                shape: ExtShape::SimplePolygon(ExtSPolygon(pts)),
                min_quality: None,
            },
            demand: d.qty as u64,
        });
    }

    let g = params.gap;
    if items.is_empty() {
        return Ok(NestOutcome {
            result: NestResult { sheets: vec![], unplaced },
            seed,
            elapsed: started.elapsed(),
        });
    }

    // jagua раздувает детали на g/2 и сужает полосу на g/2: до края получается g.
    // Полосу расширяем на 2g и потом сдвигаем всё на −g, чтобы детали доставали до кромки.
    let ext_instance = ExtSPInstance {
        name: "nest".into(),
        min_item_separation: g as f32,
        items,
        strip_height: (params.width + 2.0 * g) as f32,
    };

    let names: Vec<&str> = demands.iter().map(|d| d.part.name.as_str()).collect();
    // jagua-rs / sparrow panic on some degenerate geometry; turn that into an error instead of aborting.
    let solution = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_sparrow(&ext_instance, &names, params, seed)))
        .map_err(|p| {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            anyhow::anyhow!("внутренняя ошибка солвера: {msg}")
        })??;

    let mut placements = Vec::new();
    for pi in solution.layout_snapshot().placed_items().values() {
        let part = pi.item().external_id() as usize;
        let ext = jagua_rs::io::export::int_to_ext_transformation(&pi.d_transf(), &pi.item().shape_orig().pre_transform);
        if ext.reflected {
            bail!("солвер вернул отражённую деталь, хотя отражение не разрешалось");
        }
        let rot = f64::from(ext.rotation()).to_degrees();
        let (tx, ty) = ext.translation();
        // Итог: R·(p − offset) + t − (g, g).
        let r = Transform { rotation_deg: rot, translation: Pt::new(0.0, 0.0) }.apply(offsets[part]);
        placements.push(Placement {
            part,
            transform: Transform {
                rotation_deg: snap_angle(rot, &params.rotations),
                translation: Pt::new(f64::from(tx) - r.x - g, f64::from(ty) - r.y - g),
            },
        });
    }
    placements.sort_by_key(|p| p.part);

    let placed_total = placements.len();
    let wanted: usize = demands.iter().map(|d| d.qty).sum::<usize>() - unplaced.iter().map(|u| u.qty).sum::<usize>();
    if placed_total != wanted {
        bail!("солвер разместил {placed_total} деталей из {wanted}");
    }

    let length = placements
        .iter()
        .map(|p| demands[p.part].part.shape.transformed(&p.transform).bbox().max.x)
        .fold(0.0, f64::max);
    info!("длина раскладки {length:.1} мм, {placed_total} деталей");

    Ok(NestOutcome {
        result: NestResult {
            sheets: vec![Sheet { width: params.width, length, placements }],
            unplaced,
        },
        seed,
        elapsed: started.elapsed(),
    })
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
        warn!("{name}: внешний контур самопересекается, для раскладки взят его контур с запасом 0.1 мм");
        fixed
    } else {
        warn!("{name}: внешний контур самопересекается и не исправляется автоматически");
        out
    }
}

fn rotated_height(d: &Demand, rot: f64) -> f64 {
    let t = Transform { rotation_deg: rot, translation: Pt::new(0.0, 0.0) };
    BBox::of(d.part.shape.outer.iter().map(|&p| t.apply(p))).height()
}

/// Угол из f32 солвера приводится к ближайшему разрешённому (убирает шум вроде 179.99998°).
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

fn run_sparrow(ext: &ExtSPInstance, names: &[&str], params: &NestParams, seed: u64) -> Result<SPSolution> {
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
                Some((id, e)) => anyhow::anyhow!("jagua-rs отклонил деталь {}: {e:#}", names[id as usize]),
                None => e.context("jagua-rs отклонил задание"),
            });
        }
    };
    let rng = Xoshiro256PlusPlus::seed_from_u64(seed);
    let mut listener = ProgressListener;

    match params.budget {
        Budget::Time(total) => {
            config.expl_cfg.time_limit = total.mul_f32(DEFAULT_EXPLORE_TIME_RATIO);
            config.cmpr_cfg.time_limit = total.mul_f32(DEFAULT_COMPRESS_TIME_RATIO);
            let mut term = TimeTerminator { deadline: None, cancel };
            optimize(instance, rng, &mut listener, &mut term, &config.expl_cfg, &config.cmpr_cfg, None)
        }
        Budget::Iterations(total) => {
            // Сжатие по умолчанию затухает по часам; для воспроизводимости — по числу неудач.
            config.cmpr_cfg.shrink_decay = ShrinkDecayStrategy::FailureBased(DEFAULT_FAIL_DECAY_RATIO_CMPR);
            let explore = ((total as f32) * DEFAULT_EXPLORE_TIME_RATIO).round() as usize;
            let mut term = IterTerminator::new(vec![explore, total.saturating_sub(explore)], cancel);
            optimize(instance, rng, &mut listener, &mut term, &config.expl_cfg, &config.cmpr_cfg, None)
        }
    }
    .context("sparrow не смог построить раскладку")
}

struct ProgressListener;

impl SolutionListener for ProgressListener {
    fn report(&mut self, report: ReportType, solution: &SPSolution) {
        if matches!(report, ReportType::ExplFeas | ReportType::CmprFeas) {
            info!(
                "[{}] найдено решение: длина {:.1} мм, плотность {:.2}%",
                if report == ReportType::ExplFeas { "поиск" } else { "сжатие" },
                solution.strip_width(),
                solution.density() * 100.0
            );
        }
    }
}

/// Остановка по времени фазы или по внешней отмене. Отмена сбрасывается при смене фазы,
/// как у Ctrl-C в sparrow: первая отмена завершает поиск, вторая — сжатие.
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

/// Остановка после заданного числа вызовов `kill()` в каждой фазе (поиск, сжатие).
/// Часы не участвуют, поэтому при том же seed и числе потоков результат повторяется.
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
        // fetch_update устарел в новых Rust, но try_update недоступен в MSRV 1.90.
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
