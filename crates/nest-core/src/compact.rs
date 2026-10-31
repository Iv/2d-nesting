//! Final compaction of a finished sheet on the exact geometry: parts slide left, then down, as far
//! as the sheet edges and the other parts (at the gap) allow. Free space then gathers at the right
//! end and along the top, where it is reusable, instead of staying in holes the solver had no reason
//! to close (moving a part into a hole does not change the strip length).

use crate::geom::{BBox, Demand, Placement, Pt, Ring};
use geo::{Distance, Euclidean, LineString, Polygon};

/// Rounds of sliding left then down.
const ROUNDS: usize = 4;
/// Moves shorter than this, mm, are not worth making.
const MIN_MOVE: f64 = 0.5;
/// A slide brings parts no closer than the gap plus this, mm (or than they already are), so the
/// validation's allowance for f32 solver noise is never used up.
const MARGIN: f64 = 0.005;

/// Slides the parts of a sheet left and down; returns whether any part moved.
pub fn slide(demands: &[Demand], placements: &mut [Placement], gap: f64) -> bool {
    let mut rings: Vec<Ring> = placements.iter().map(|p| demands[p.part].part.shape.transformed(&p.transform).outer).collect();
    let mut moved_any = false;
    for _ in 0..ROUNDS {
        let mut moved = false;
        for axis in [Axis::X, Axis::Y] {
            // Parts nearest the edge go first and make room for the ones behind them.
            let mut order: Vec<usize> = (0..rings.len()).collect();
            order.sort_by(|&a, &b| axis.lo(&BBox::of(rings[a].iter().copied())).total_cmp(&axis.lo(&BBox::of(rings[b].iter().copied()))));
            for i in order {
                let s = free_run(&rings, i, axis, gap);
                if s >= MIN_MOVE {
                    let d = axis.step(s);
                    for p in &mut rings[i] {
                        *p = Pt::new(p.x - d.x, p.y - d.y);
                    }
                    placements[i].transform.translation.x -= d.x;
                    placements[i].transform.translation.y -= d.y;
                    moved = true;
                }
            }
        }
        moved_any |= moved;
        if !moved {
            break;
        }
    }
    moved_any
}

#[derive(Clone, Copy)]
enum Axis {
    X,
    Y,
}

impl Axis {
    /// Coordinates (along the move, across it).
    fn uv(self, p: Pt) -> (f64, f64) {
        match self {
            Axis::X => (p.x, p.y),
            Axis::Y => (p.y, p.x),
        }
    }

    fn lo(self, b: &BBox) -> f64 {
        self.uv(b.min).0
    }

    /// Displacement of a slide by `s` towards the origin.
    fn step(self, s: f64) -> Pt {
        match self {
            Axis::X => Pt::new(s, 0.0),
            Axis::Y => Pt::new(0.0, s),
        }
    }
}

/// How far part `i` can slide towards the origin along `axis`.
fn free_run(rings: &[Ring], i: usize, axis: Axis, gap: f64) -> f64 {
    let me = &rings[i];
    let bb = BBox::of(me.iter().copied());
    let (lo_v, hi_v) = {
        let (a, b) = (axis.uv(bb.min).1, axis.uv(bb.max).1);
        (a - gap, b + gap)
    };
    // Parts in the band the part sweeps (one may sit in a concavity of it, so not only those behind).
    let others: Vec<usize> = (0..rings.len())
        .filter(|&j| j != i)
        .filter(|&j| {
            let ob = BBox::of(rings[j].iter().copied());
            let (_, v0) = axis.uv(ob.min);
            let (_, v1) = axis.uv(ob.max);
            v1 >= lo_v && v0 <= hi_v
        })
        .collect();
    let mut s = axis.lo(&bb);
    for &j in &others {
        s = s.min(contact(me, &rings[j], axis) - gap);
    }
    if s < MIN_MOVE {
        return 0.0;
    }
    let poly = |r: &Ring, d: Pt| {
        let mut pts: Vec<(f64, f64)> = r.iter().map(|p| (p.x - d.x, p.y - d.y)).collect();
        pts.push(pts[0]);
        Polygon::new(LineString::from(pts), vec![])
    };
    let near: Vec<Polygon<f64>> = others.iter().map(|&j| poly(&rings[j], Pt::new(0.0, 0.0))).collect();
    // A neighbour already at the gap (the solver packs parts exactly that close) may stay as close.
    let here = poly(me, Pt::new(0.0, 0.0));
    let limit: Vec<f64> = near.iter().map(|o| Euclidean.distance(&here, o).min(gap + MARGIN) - 1e-9).collect();
    let clear = |s: f64| {
        let moved = poly(me, axis.step(s));
        near.iter().zip(&limit).all(|(o, &l)| Euclidean.distance(&moved, o) >= l)
    };
    if clear(s) {
        return s;
    }
    // Oblique contacts stop the part before the straight-line estimate: largest clear slide.
    if !clear(MIN_MOVE) {
        return 0.0;
    }
    let (mut ok, mut bad) = (MIN_MOVE, s);
    for _ in 0..24 {
        let mid = (ok + bad) / 2.0;
        if clear(mid) {
            ok = mid;
        } else {
            bad = mid;
        }
        if bad - ok < 0.05 {
            break;
        }
    }
    ok
}

/// Distance `a` can slide towards the origin along `axis` before touching `b` (∞ if never).
fn contact(a: &Ring, b: &Ring, axis: Axis) -> f64 {
    // A vertex of `p` moving by `dir` (−1: towards the origin) hits an edge of `q`.
    let hits = |p: &Ring, q: &Ring, dir: f64| {
        let mut best = f64::INFINITY;
        for &v in p {
            let (u, w) = axis.uv(v);
            for k in 0..q.len() {
                let (u1, w1) = axis.uv(q[k]);
                let (u2, w2) = axis.uv(q[(k + 1) % q.len()]);
                if w1 == w2 || w < w1.min(w2) || w > w1.max(w2) {
                    continue;
                }
                let ue = u1 + (u2 - u1) * (w - w1) / (w2 - w1);
                let t = (ue - u) * dir;
                if t >= -1e-9 {
                    best = best.min(t.max(0.0));
                }
            }
        }
        best
    };
    hits(a, b, -1.0).min(hits(b, a, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{Part, Shape, Transform};

    fn rect(w: f64, h: f64) -> Demand {
        let s = Shape::new(vec![Pt::new(0.0, 0.0), Pt::new(w, 0.0), Pt::new(w, h), Pt::new(0.0, h)], vec![]);
        Demand { part: Part { name: "r".into(), shape: s, ..Default::default() }, qty: 1, filler: false }
    }

    fn at(part: usize, x: f64, y: f64) -> Placement {
        Placement { part, transform: Transform { rotation_deg: 0.0, translation: Pt::new(x, y) } }
    }

    #[test]
    fn parts_slide_into_a_hole_and_keep_the_gap() {
        let demands = vec![rect(1000.0, 300.0), rect(600.0, 300.0)];
        // A long part on top at the left edge; a short one below it, 700 mm off the edge.
        let mut pl = vec![at(0, 0.0, 303.0), at(1, 700.0, 0.0)];
        assert!(slide(&demands, &mut pl, 3.0));
        let t = |i: usize| pl[i].transform.translation;
        assert!(t(1).x.abs() < 1e-9 && t(1).y.abs() < 1e-9, "{:?}", t(1));
        // The long one is already against the left edge and cannot go down: 3 mm above the short one.
        assert!(t(0).x.abs() < 1e-9 && (t(0).y - 303.0).abs() < 0.1, "{:?}", t(0));
    }

    #[test]
    fn a_slanted_neighbour_stops_the_part_at_the_gap() {
        // A wedge with its slanted side facing right, and a square to its right.
        let wedge = Shape::new(vec![Pt::new(0.0, 0.0), Pt::new(400.0, 0.0), Pt::new(200.0, 400.0), Pt::new(0.0, 400.0)], vec![]);
        let demands = vec![
            Demand { part: Part { name: "w".into(), shape: wedge, ..Default::default() }, qty: 1, filler: false },
            rect(100.0, 100.0),
        ];
        let mut pl = vec![at(0, 0.0, 0.0), at(1, 900.0, 250.0)];
        slide(&demands, &mut pl, 3.0);
        let rings: Vec<Ring> = pl.iter().map(|p| demands[p.part].part.shape.transformed(&p.transform).outer).collect();
        let poly = |r: &Ring| Polygon::new(LineString::from(r.iter().map(|p| (p.x, p.y)).chain(std::iter::once((r[0].x, r[0].y))).collect::<Vec<_>>()), vec![]);
        let d = Euclidean.distance(&poly(&rings[0]), &poly(&rings[1]));
        assert!(d >= 3.0 && d < 3.2, "distance {d}");
        // It ended up below the wedge's top-right corner region, not at the far right.
        assert!(pl[1].transform.translation.x < 400.0, "{:?}", pl[1].transform.translation);
    }
}
