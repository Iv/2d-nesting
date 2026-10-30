//! Reads a part from DXF: closed contours from LWPOLYLINE, POLYLINE, LINE/ARC chains, CIRCLE,
//! ELLIPSE and SPLINE (also inside INSERT blocks). Curves are approximated by polylines.

use crate::geom::{point_in_ring, signed_area, Part, Pt, Ring, Shape};
use anyhow::{bail, Context, Result};
use dxf::entities::{Entity, EntityType};
use dxf::enums::Units;
use dxf::{Drawing, Point};
use log::{debug, warn};
use std::collections::{BTreeMap, HashSet};
use std::f64::consts::{PI, TAU};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ImportOptions {
    /// Maximum chord deviation from an arc/spline, mm.
    pub tolerance: f64,
    /// How far apart the ends of adjacent chain pieces may be, mm.
    pub join_tolerance: f64,
    /// If set, only these layers are read (case-insensitive).
    pub layers: Option<Vec<String>>,
    /// Layers to skip (case-insensitive).
    pub ignore_layers: Vec<String>,
    /// Allow open lines inside the outer contour (they travel with the part into the output).
    /// If `false`, any open line is an error.
    pub allow_open_inside: bool,
    /// Gap up to which open pieces are bridged when assembling the outer contour, mm.
    pub close_gaps: f64,
    /// Inner geometry may stick out of the outer contour by this much (marks on the outline), mm.
    pub edge_tolerance: f64,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            tolerance: 0.2,
            join_tolerance: 0.05,
            layers: None,
            ignore_layers: Vec::new(),
            allow_open_inside: true,
            close_gaps: 1.0,
            edge_tolerance: 2.0,
        }
    }
}

impl ImportOptions {
    fn layer_wanted(&self, layer: &str) -> bool {
        let eq = |l: &String| l.eq_ignore_ascii_case(layer);
        if self.ignore_layers.iter().any(eq) {
            return false;
        }
        self.layers.as_ref().is_none_or(|ls| ls.iter().any(eq))
    }
}

/// Reads one part; the part name is the file stem.
pub fn read_part(path: &Path, opts: &ImportOptions) -> Result<Part> {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let drawing = Drawing::load_file(path)
        .with_context(|| format!("{}: cannot read DXF", path.display()))?;
    let mut warnings = Vec::new();
    let shape = shape_from_drawing(&drawing, opts, &mut warnings)
        .with_context(|| format!("{}", path.display()))?;
    for w in warnings {
        warn!("{}: {w}", path.display());
    }
    Ok(Part {
        name,
        source: path.to_path_buf(),
        shape,
    })
}

/// Scale factor from drawing units to mm.
pub(crate) fn unit_scale(units: Units, warnings: &mut Vec<String>) -> Result<f64> {
    Ok(match units {
        Units::Millimeters => 1.0,
        Units::Unitless => {
            warnings.push("$INSUNITS is not set, assuming millimetres".into());
            1.0
        }
        Units::Centimeters => 10.0,
        Units::Decimeters => 100.0,
        Units::Meters => 1000.0,
        Units::Inches | Units::USSurveyInch => 25.4,
        Units::Feet => 304.8,
        Units::Microns => 0.001,
        other => bail!("unsupported drawing units $INSUNITS = {other:?}"),
    })
}

/// Builds a part from a drawing; non-fatal remarks go to `warnings`.
pub fn shape_from_drawing(drawing: &Drawing, opts: &ImportOptions, warnings: &mut Vec<String>) -> Result<Shape> {
    let pieces = collect_pieces(drawing, opts, warnings)?;
    let (rings, open) = chain(pieces, opts.join_tolerance);
    if !open.is_empty() && !opts.allow_open_inside {
        let mut by_layer: BTreeMap<String, usize> = BTreeMap::new();
        for (_, layer) in &open {
            *by_layer.entry(layer.clone()).or_default() += 1;
        }
        let detail = by_layer
            .iter()
            .map(|(l, n)| format!("layer \"{l}\": {n}"))
            .collect::<Vec<_>>()
            .join(", ");
        // The distance between chain ends hints whether a larger --join-tolerance would help.
        let mut gaps: Vec<f64> = open.iter().map(|(c, _)| c[0].dist(*c.last().unwrap())).collect();
        gaps.sort_by(f64::total_cmp);
        let gaps = gaps.iter().take(3).map(|g| format!("{g:.3}")).collect::<Vec<_>>().join(", ");
        bail!(
            "{} open contours ({detail}; distance between chain ends, mm: {gaps}). \
             Close the contours in the drawing, raise --join-tolerance or skip the layer with --ignore-layers",
            open.len()
        );
    }
    assemble(rings, open, opts, warnings)
}

fn collect_pieces(drawing: &Drawing, opts: &ImportOptions, warnings: &mut Vec<String>) -> Result<Vec<(Vec<Pt>, String)>> {
    let scale = unit_scale(drawing.header.default_drawing_units, warnings)?;
    let mut collector = Collector { drawing, opts, pieces: Vec::new(), skipped: BTreeMap::new(), warnings };
    let root = Affine::scale(scale);
    for e in drawing.entities() {
        collector.entity(e, &root, None, 0)?;
    }
    if !collector.skipped.is_empty() {
        debug!("skipped entities: {:?}", collector.skipped);
    }
    Ok(collector.pieces)
}

/// Closed and open contours of a drawing (for diagnostics).
pub fn contours(drawing: &Drawing, opts: &ImportOptions) -> Result<(Vec<(Ring, String)>, Vec<(Vec<Pt>, String)>)> {
    let mut warnings = Vec::new();
    let pieces = collect_pieces(drawing, opts, &mut warnings)?;
    Ok(chain(pieces, opts.join_tolerance))
}

/// All closed contours of a drawing, including rings that only close with the `close_gaps` tolerance.
/// For analysing existing multi-part layouts.
pub fn closed_contours(drawing: &Drawing, opts: &ImportOptions) -> Result<Vec<Ring>> {
    let mut warnings = Vec::new();
    let pieces = collect_pieces(drawing, opts, &mut warnings)?;
    let (rings, open) = chain(pieces, opts.join_tolerance);
    let (bridged, _) = chain(open, opts.close_gaps);
    Ok(rings.into_iter().chain(bridged).map(|(r, _)| r).filter(|r| r.len() >= 3 && signed_area(r).abs() > 1e-6).collect())
}

/// Picks the outer contour and checks that everything else lies inside it.
///
/// The outer contour is the largest closed ring. If the open chains, joined with the coarser
/// `close_gaps` tolerance, form a larger ring, that ring becomes the outer contour instead: real
/// drawings often have an outline made of pieces that do not quite meet. Only the outer contour is
/// bridged; inner geometry is kept exactly as drawn.
fn assemble(
    rings: Vec<(Ring, String)>,
    open: Vec<(Vec<Pt>, String)>,
    opts: &ImportOptions,
    warnings: &mut Vec<String>,
) -> Result<Shape> {
    let usable = |r: &Ring| r.len() >= 3 && signed_area(r).abs() > 1e-6;
    let mut rings: Vec<(Ring, String)> = rings
        .into_iter()
        .filter(|(r, layer)| {
            let ok = usable(r);
            if !ok {
                warnings.push(format!("degenerate contour on layer \"{layer}\" skipped"));
            }
            ok
        })
        .collect();
    rings.sort_by(|a, b| signed_area(&b.0).abs().total_cmp(&signed_area(&a.0).abs()));
    let closed_best = rings.first().map_or(0.0, |(r, _)| signed_area(r).abs());

    let bridged = if opts.close_gaps > opts.join_tolerance && !open.is_empty() {
        let (b, _) = chain(open.clone(), opts.close_gaps);
        b.into_iter().map(|(r, _)| r).filter(usable).max_by(|a, b| signed_area(a).abs().total_cmp(&signed_area(b).abs()))
    } else {
        None
    };

    let mut open = open;
    let outer = match bridged {
        Some(b) if signed_area(&b).abs() > closed_best => {
            // Open chains that became part of the outer contour are no longer inner lines.
            let on_outer: HashSet<(u64, u64)> = b.iter().map(|p| (p.x.to_bits(), p.y.to_bits())).collect();
            open.retain(|(c, _)| !c.iter().all(|p| on_outer.contains(&(p.x.to_bits(), p.y.to_bits()))));
            warnings.push(format!(
                "outer contour assembled from open pieces with gaps up to {} mm (--close-gaps)",
                opts.close_gaps
            ));
            b
        }
        _ if rings.is_empty() => bail!(
            "no closed contours in the drawing; if the outline is broken, raise --close-gaps (now {} mm)",
            opts.close_gaps
        ),
        _ => rings.remove(0).0,
    };

    // Marks and notches may sit on the outline itself, so points close to it count as inside.
    let inside = |p: Pt| point_in_ring(p, &outer) || dist_to_ring(p, &outer) <= opts.edge_tolerance;
    let mut holes: Vec<Ring> = Vec::new();
    for (ring, layer) in rings {
        if !ring.iter().all(|&p| inside(p)) {
            bail!(
                "contour on layer \"{layer}\" (area {:.1} mm²) is not inside the outer contour: a file must hold one part \
                 (if the outline is broken, raise --close-gaps, now {} mm)",
                signed_area(&ring).abs(),
                opts.close_gaps
            );
        }
        holes.push(ring);
    }
    let outside = open.iter().filter(|(c, _)| !c.iter().all(|&p| inside(p))).count();
    if outside > 0 {
        bail!(
            "{outside} open lines leave the outer contour: it is probably broken by more than --close-gaps ({} mm)",
            opts.close_gaps
        );
    }
    let mut shape = Shape::new(outer, holes);
    shape.lines = open.into_iter().map(|(c, _)| c).collect();
    Ok(shape)
}

pub(crate) fn dist_to_ring(p: Pt, ring: &[Pt]) -> f64 {
    (0..ring.len()).map(|i| dist_to_segment(p, ring[i], ring[(i + 1) % ring.len()])).fold(f64::INFINITY, f64::min)
}

/// 2D affine transform: [a b c; d e f].
#[derive(Debug, Clone, Copy)]
struct Affine([f64; 6]);

impl Affine {
    fn scale(s: f64) -> Self {
        Affine([s, 0.0, 0.0, 0.0, s, 0.0])
    }

    fn apply(&self, p: Pt) -> Pt {
        let m = &self.0;
        Pt::new(m[0] * p.x + m[1] * p.y + m[2], m[3] * p.x + m[4] * p.y + m[5])
    }

    /// self ∘ other: other first, then self.
    fn then_inner(&self, o: &Affine) -> Affine {
        let (a, b) = (&self.0, &o.0);
        Affine([
            a[0] * b[0] + a[1] * b[3],
            a[0] * b[1] + a[1] * b[4],
            a[0] * b[2] + a[1] * b[5] + a[2],
            a[3] * b[0] + a[4] * b[3],
            a[3] * b[1] + a[4] * b[4],
            a[3] * b[2] + a[4] * b[5] + a[5],
        ])
    }

    /// Largest stretch factor, used to convert the tolerance into local units.
    fn max_stretch(&self) -> f64 {
        let m = &self.0;
        m[0].hypot(m[3]).max(m[1].hypot(m[4])).max(1e-12)
    }
}

/// Entities with normal (0,0,-1) live in an OCS whose X axis is flipped.
fn ocs(normal_z: f64) -> Affine {
    if normal_z < 0.0 {
        Affine([-1.0, 0.0, 0.0, 0.0, 1.0, 0.0])
    } else {
        Affine::scale(1.0)
    }
}

struct Collector<'a> {
    drawing: &'a Drawing,
    opts: &'a ImportOptions,
    /// Polylines in mm with their layer.
    pieces: Vec<(Vec<Pt>, String)>,
    skipped: BTreeMap<&'static str, usize>,
    warnings: &'a mut Vec<String>,
}

impl Collector<'_> {
    fn entity(&mut self, e: &Entity, xf: &Affine, parent_layer: Option<&str>, depth: usize) -> Result<()> {
        // Block entities on layer "0" take the layer of the insert.
        let layer = match parent_layer {
            Some(pl) if e.common.layer == "0" => pl.to_string(),
            _ => e.common.layer.clone(),
        };
        if !matches!(e.specific, EntityType::Insert(_)) && !self.opts.layer_wanted(&layer) {
            return Ok(());
        }
        let tol = self.opts.tolerance / xf.max_stretch();
        let local: Option<Vec<Pt>> = match &e.specific {
            EntityType::Line(l) => Some(vec![p2(&l.p1), p2(&l.p2)]),
            EntityType::Arc(a) => {
                let (start, mut end) = (a.start_angle.to_radians(), a.end_angle.to_radians());
                if end <= start {
                    end += TAU;
                }
                let pts = arc_points(p2(&a.center), a.radius, start, end - start, tol);
                Some(map(&ocs(a.normal.z), pts))
            }
            EntityType::Circle(c) => {
                let mut pts = arc_points(p2(&c.center), c.radius, 0.0, TAU, tol);
                pts.pop();
                pts.push(pts[0]);
                Some(map(&ocs(c.normal.z), pts))
            }
            EntityType::LwPolyline(pl) => {
                let verts: Vec<(Pt, f64)> = pl.vertices.iter().map(|v| (Pt::new(v.x, v.y), v.bulge)).collect();
                let pts = bulge_polyline(&verts, pl.flags & 1 != 0, tol);
                Some(map(&ocs(pl.extrusion_direction.z), pts))
            }
            EntityType::Polyline(pl) => {
                if pl.is_3d_polygon_mesh() || pl.is_polyface_mesh() {
                    bail!("layer \"{layer}\": POLYLINE meshes (3D mesh/polyface) are not supported");
                }
                let verts: Vec<(Pt, f64)> = pl
                    .vertices()
                    .filter(|v| !v.is_spline_frame_control_point())
                    .map(|v| (p2(&v.location), v.bulge))
                    .collect();
                let pts = bulge_polyline(&verts, pl.flags & 1 != 0, tol);
                Some(map(&ocs(pl.normal.z), pts))
            }
            EntityType::Ellipse(el) => Some(ellipse_points(el, tol)),
            EntityType::Spline(sp) => Some(spline_points(sp, tol, &layer, self.warnings)?),
            EntityType::Insert(ins) => {
                self.insert(ins, xf, &layer, depth)?;
                None
            }
            other => {
                *self.skipped.entry(entity_kind(other)).or_default() += 1;
                None
            }
        };
        if let Some(pts) = local {
            let pts: Vec<Pt> = map(xf, pts);
            if pts.len() >= 2 {
                self.pieces.push((pts, layer));
            }
        }
        Ok(())
    }

    fn insert(&mut self, ins: &dxf::entities::Insert, xf: &Affine, layer: &str, depth: usize) -> Result<()> {
        if depth > 16 {
            bail!("blocks nested too deep (INSERT \"{}\")", ins.name);
        }
        let Some(block) = self.drawing.blocks().find(|b| b.name == ins.name) else {
            bail!("block \"{}\" not found", ins.name);
        };
        if ins.row_count > 1 || ins.column_count > 1 {
            bail!("MINSERT arrays (block \"{}\") are not supported", ins.name);
        }
        let (s, c) = ins.rotation.to_radians().sin_cos();
        let (sx, sy) = (ins.x_scale_factor, ins.y_scale_factor);
        let (bx, by) = (block.base_point.x, block.base_point.y);
        // p' = location + R · S · (p − base), in the insert's OCS.
        let local = Affine([
            c * sx,
            -s * sy,
            ins.location.x - (c * sx * bx - s * sy * by),
            s * sx,
            c * sy,
            ins.location.y - (s * sx * bx + c * sy * by),
        ]);
        let full = xf.then_inner(&ocs(ins.extrusion_direction.z).then_inner(&local));
        for e in &block.entities {
            self.entity(e, &full, Some(layer), depth + 1)?;
        }
        Ok(())
    }
}

fn p2(p: &Point) -> Pt {
    Pt::new(p.x, p.y)
}

fn map(xf: &Affine, pts: Vec<Pt>) -> Vec<Pt> {
    pts.into_iter().map(|p| xf.apply(p)).collect()
}

fn entity_kind(e: &EntityType) -> &'static str {
    match e {
        EntityType::Text(_) => "TEXT",
        EntityType::MText(_) => "MTEXT",
        EntityType::ModelPoint(_) => "POINT",
        EntityType::Attribute(_) => "ATTRIB",
        EntityType::AttributeDefinition(_) => "ATTDEF",
        EntityType::Solid(_) => "SOLID",
        _ => "other",
    }
}

/// Segment count for an arc of radius r and angle sweep at chord tolerance tol.
fn arc_segments(r: f64, sweep: f64, tol: f64) -> usize {
    let step = if tol >= r { PI / 2.0 } else { 2.0 * (1.0 - tol / r).acos() };
    ((sweep.abs() / step).ceil() as usize).max(1)
}

/// Arc points from start over sweep (sign gives direction), both ends included.
pub fn arc_points(center: Pt, r: f64, start: f64, sweep: f64, tol: f64) -> Vec<Pt> {
    let n = arc_segments(r, sweep, tol).max(if (sweep.abs() - TAU).abs() < 1e-9 { 8 } else { 1 });
    (0..=n)
        .map(|i| {
            let a = start + sweep * i as f64 / n as f64;
            Pt::new(center.x + r * a.cos(), center.y + r * a.sin())
        })
        .collect()
}

/// Polyline with bulges (tan(θ/4)) -> points. A closed one ends with its first point repeated.
fn bulge_polyline(verts: &[(Pt, f64)], closed: bool, tol: f64) -> Vec<Pt> {
    let n = verts.len();
    if n == 0 {
        return Vec::new();
    }
    let segs = if closed { n } else { n - 1 };
    let mut out = vec![verts[0].0];
    for i in 0..segs {
        let (p0, b) = verts[i];
        let p1 = verts[(i + 1) % n].0;
        if b.abs() < 1e-12 || p0.dist(p1) < 1e-12 {
            out.push(p1);
            continue;
        }
        let theta = 4.0 * b.atan();
        let c = p0.dist(p1);
        let r = c / (2.0 * (theta / 2.0).sin()).abs();
        let mid = Pt::new((p0.x + p1.x) / 2.0, (p0.y + p1.y) / 2.0);
        let (dx, dy) = ((p1.x - p0.x) / c, (p1.y - p0.y) / c);
        // The centre is left of the chord midpoint (for b > 0) by (c/2)·(1 − b²)/(2b).
        let h = (c / 2.0) * (1.0 - b * b) / (2.0 * b);
        let center = Pt::new(mid.x - dy * h, mid.y + dx * h);
        let start = (p0.y - center.y).atan2(p0.x - center.x);
        let pts = arc_points(center, r, start, theta, tol);
        out.extend_from_slice(&pts[1..pts.len() - 1]);
        out.push(p1);
    }
    out
}

fn ellipse_points(el: &dxf::entities::Ellipse, tol: f64) -> Vec<Pt> {
    let c = p2(&el.center);
    let major = Pt::new(el.major_axis.x, el.major_axis.y);
    let sign = if el.normal.z < 0.0 { -1.0 } else { 1.0 };
    let minor = Pt::new(-major.y * el.minor_axis_ratio * sign, major.x * el.minor_axis_ratio * sign);
    let (start, mut end) = (el.start_parameter, el.end_parameter);
    if end <= start {
        end += TAU;
    }
    let full = (end - start - TAU).abs() < 1e-9;
    let r = major.x.hypot(major.y);
    let n = arc_segments(r, end - start, tol).max(if full { 8 } else { 1 });
    let mut pts: Vec<Pt> = (0..=n)
        .map(|i| {
            let t = start + (end - start) * i as f64 / n as f64;
            let (s, co) = t.sin_cos();
            Pt::new(c.x + major.x * co + minor.x * s, c.y + major.y * co + minor.y * s)
        })
        .collect();
    if full {
        pts.pop();
        pts.push(pts[0]);
    }
    pts
}

fn spline_points(sp: &dxf::entities::Spline, tol: f64, layer: &str, warnings: &mut Vec<String>) -> Result<Vec<Pt>> {
    let p = sp.degree_of_curve.max(1) as usize;
    let ctrl: Vec<Pt> = sp.control_points.iter().map(p2).collect();
    if ctrl.is_empty() {
        if sp.fit_points.len() >= 2 {
            warnings.push(format!("layer \"{layer}\": SPLINE without control points, using its fit points as a polyline"));
            return Ok(sp.fit_points.iter().map(p2).collect());
        }
        bail!("layer \"{layer}\": empty SPLINE");
    }
    let knots = &sp.knot_values;
    if knots.len() != ctrl.len() + p + 1 {
        bail!(
            "layer \"{layer}\": SPLINE with a wrong knot count ({} for {} points of degree {p})",
            knots.len(),
            ctrl.len()
        );
    }
    let weights: Vec<f64> = if sp.weight_values.len() == ctrl.len() {
        sp.weight_values.clone()
    } else {
        vec![1.0; ctrl.len()]
    };
    let nurbs = Nurbs { p, ctrl: &ctrl, weights: &weights, knots };
    let mut out = vec![nurbs.eval(knots[p])];
    // Each non-empty knot span is split in 4 and refined recursively by chord deviation.
    for w in knots[p..=ctrl.len()].windows(2) {
        if w[1] - w[0] <= 1e-12 {
            continue;
        }
        for k in 0..4 {
            let a = w[0] + (w[1] - w[0]) * k as f64 / 4.0;
            let b = w[0] + (w[1] - w[0]) * (k + 1) as f64 / 4.0;
            nurbs.refine(a, b, nurbs.eval(a), nurbs.eval(b), tol, 0, &mut out);
        }
    }
    Ok(out)
}

struct Nurbs<'a> {
    p: usize,
    ctrl: &'a [Pt],
    weights: &'a [f64],
    knots: &'a [f64],
}

impl Nurbs<'_> {
    /// De Boor's algorithm in homogeneous coordinates.
    fn eval(&self, t: f64) -> Pt {
        let (p, n) = (self.p, self.ctrl.len());
        let k = self.knots;
        let mut span = p;
        while span < n - 1 && t >= k[span + 1] {
            span += 1;
        }
        let mut d: Vec<[f64; 3]> = (0..=p)
            .map(|j| {
                let i = span - p + j;
                let w = self.weights[i];
                [self.ctrl[i].x * w, self.ctrl[i].y * w, w]
            })
            .collect();
        for r in 1..=p {
            for j in (r..=p).rev() {
                let i = span - p + j;
                let den = k[i + p + 1 - r] - k[i];
                let a = if den.abs() < 1e-15 { 0.0 } else { (t - k[i]) / den };
                for c in 0..3 {
                    d[j][c] = (1.0 - a) * d[j - 1][c] + a * d[j][c];
                }
            }
        }
        Pt::new(d[p][0] / d[p][2], d[p][1] / d[p][2])
    }

    fn refine(&self, a: f64, b: f64, pa: Pt, pb: Pt, tol: f64, depth: usize, out: &mut Vec<Pt>) {
        let m = (a + b) / 2.0;
        let pm = self.eval(m);
        if depth < 12 && dist_to_segment(pm, pa, pb) > tol {
            self.refine(a, m, pa, pm, tol, depth + 1, out);
            self.refine(m, b, pm, pb, tol, depth + 1, out);
        } else {
            out.push(pb);
        }
    }
}

fn dist_to_segment(p: Pt, a: Pt, b: Pt) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-24 {
        return p.dist(a);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0);
    p.dist(Pt::new(a.x + t * dx, a.y + t * dy))
}

/// Joins pieces into closed contours. Returns (closed, open).
/// Pieces shorter than the tolerance are dropped: their ends count as coincident anyway.
#[allow(clippy::type_complexity)]
fn chain(pieces: Vec<(Vec<Pt>, String)>, tol: f64) -> (Vec<(Ring, String)>, Vec<(Vec<Pt>, String)>) {
    let mut closed = Vec::new();
    let mut open: Vec<(Vec<Pt>, String)> = Vec::new();
    for (pts, layer) in pieces {
        let len: f64 = pts.windows(2).map(|w| w[0].dist(w[1])).sum();
        if len <= tol {
            continue;
        }
        if pts.len() >= 4 && pts[0].dist(*pts.last().unwrap()) <= tol {
            closed.push((finish_ring(pts), layer));
        } else {
            open.push((pts, layer));
        }
    }
    // Nearest piece end to p among the remaining pieces: (index, matched its end rather than start).
    let nearest = |open: &[(Vec<Pt>, String)], p: Pt| -> Option<(usize, bool)> {
        open.iter()
            .enumerate()
            .flat_map(|(i, (q, _))| [(i, false, q[0].dist(p)), (i, true, q.last().unwrap().dist(p))])
            .filter(|&(_, _, d)| d <= tol)
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(i, rev, _)| (i, rev))
    };
    let mut leftover = Vec::new();
    while let Some((mut cur, layer)) = open.pop() {
        let mut reversed_once = false;
        loop {
            let (start, end) = (cur[0], *cur.last().unwrap());
            let closing = start.dist(end);
            let next = nearest(&open, end);
            // Close the ring if its start is nearer than any continuation.
            let next_dist = next.map(|(i, rev)| {
                let q = &open[i].0;
                if rev { q.last().unwrap().dist(end) } else { q[0].dist(end) }
            });
            if cur.len() >= 4 && closing <= tol && next_dist.is_none_or(|d| closing <= d) {
                closed.push((finish_ring(cur), layer));
                break;
            }
            match next {
                Some((i, rev)) => {
                    let (mut q, _) = open.swap_remove(i);
                    if rev {
                        q.reverse();
                    }
                    cur.extend_from_slice(&q[1..]);
                }
                None if !reversed_once => {
                    // The continuation may be at the other end.
                    reversed_once = true;
                    cur.reverse();
                }
                None => {
                    leftover.push((cur, layer));
                    break;
                }
            }
        }
    }
    (closed, leftover)
}

/// Drops the closing point and consecutive duplicates.
fn finish_ring(mut pts: Vec<Pt>) -> Ring {
    pts.pop();
    let mut out: Ring = Vec::with_capacity(pts.len());
    for p in pts {
        if out.last().is_none_or(|q: &Pt| q.dist(p) > 1e-6) {
            out.push(p);
        }
    }
    while out.len() > 1 && out[0].dist(*out.last().unwrap()) <= 1e-6 {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulge_half_circle() {
        // Half circle r=10 from (0,0) to (20,0) with bulge 1 (counter-clockwise) passes through (10,-10).
        let pts = bulge_polyline(&[(Pt::new(0.0, 0.0), 1.0), (Pt::new(20.0, 0.0), 0.0)], false, 0.01);
        assert!(pts.iter().all(|p| (p.dist(Pt::new(10.0, 0.0)) - 10.0).abs() < 1e-9));
        assert!(pts.iter().any(|p| p.dist(Pt::new(10.0, -10.0)) < 1e-6));
    }

    #[test]
    fn arc_chord_error_within_tolerance() {
        let r = 100.0;
        let pts = arc_points(Pt::new(0.0, 0.0), r, 0.0, TAU, 0.2);
        for w in pts.windows(2) {
            let mid = Pt::new((w[0].x + w[1].x) / 2.0, (w[0].y + w[1].y) / 2.0);
            assert!(r - mid.dist(Pt::new(0.0, 0.0)) <= 0.2 + 1e-9);
        }
    }
}
