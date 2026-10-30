use crate::eval::lbf_evaluator::X_MULTIPLIER;
use crate::eval::sample_eval::{SampleEval, SampleEvaluator};
use jagua_rs::entities::Item;
use jagua_rs::geometry::{normalize_rotation, DTransformation};

/// Penalty for placing an item at a given rotation, e.g. for deviating from a preferred direction.
/// The unit is the strip length: a penalty of 10 weighs as much as 10 units of extra length.
#[derive(Debug, Clone, Default)]
pub struct TiltPenalty {
    /// By the item's external id: `(rotation in radians, penalty)` for every allowed rotation.
    /// `None` (or a missing entry) means the item is not penalised.
    pub items: Vec<Option<Vec<(f32, f32)>>>,
}

impl TiltPenalty {
    /// The rotations and penalties of an item, if it is penalised.
    pub fn table(&self, item: &Item) -> Option<&[(f32, f32)]> {
        self.items.get(item.external_id() as usize)?.as_deref()
    }

    /// Penalty of an item at `rotation` (the entry of the closest listed rotation).
    pub fn of(&self, item: &Item, rotation: f32) -> f32 {
        self.table(item)
            .and_then(|t| {
                t.iter()
                    .min_by(|a, b| {
                        let da = normalize_rotation(rotation - a.0).abs();
                        let db = normalize_rotation(rotation - b.0).abs();
                        da.total_cmp(&db)
                    })
                    .map(|&(_, p)| p)
            })
            .unwrap_or(0.0)
    }
}

/// Adds the tilt penalty to collision-free evaluations, so that among collision-free placements the
/// least tilted one wins. Collisions are left alone: removing overlap always comes first.
/// The penalty is scaled like the x coordinate of the left-bottom-fill evaluator, so there it acts as
/// extra length; for separation any positive scale only orders the collision-free samples.
pub struct TiltEvaluator<'a, E> {
    pub inner: E,
    pub tilt: &'a TiltPenalty,
    pub item: &'a Item,
}

impl<E: SampleEvaluator> SampleEvaluator for TiltEvaluator<'_, E> {
    fn evaluate_sample(&mut self, dt: DTransformation, upper_bound: Option<SampleEval>) -> SampleEval {
        match self.inner.evaluate_sample(dt, upper_bound) {
            SampleEval::Clear { loss } => SampleEval::Clear { loss: loss + X_MULTIPLIER * self.tilt.of(self.item, dt.rotation()) },
            other => other,
        }
    }

    fn n_evals(&self) -> usize {
        self.inner.n_evals()
    }
}
