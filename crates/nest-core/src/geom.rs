//! Basic geometry: points, contours, parts, placements and sheets. All sizes in mm.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pt {
    pub x: f64,
    pub y: f64,
}

impl Pt {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn dist(self, o: Pt) -> f64 {
        (self.x - o.x).hypot(self.y - o.y)
    }
}

/// Closed contour without repeating the first point at the end.
pub type Ring = Vec<Pt>;

/// Signed area: positive for counter-clockwise rings.
pub fn signed_area(ring: &[Pt]) -> f64 {
    let n = ring.len();
    let mut s = 0.0;
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        s += a.x * b.y - b.x * a.y;
    }
    s / 2.0
}

/// Point-in-ring test (ray casting); points on the boundary give either answer.
pub fn point_in_ring(p: Pt, ring: &[Pt]) -> bool {
    let n = ring.len();
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (ring[i], ring[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    pub min: Pt,
    pub max: Pt,
}

impl BBox {
    pub fn of(points: impl IntoIterator<Item = Pt>) -> Self {
        let mut b = BBox {
            min: Pt::new(f64::INFINITY, f64::INFINITY),
            max: Pt::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
        };
        for p in points {
            b.min.x = b.min.x.min(p.x);
            b.min.y = b.min.y.min(p.y);
            b.max.x = b.max.x.max(p.x);
            b.max.y = b.max.y.max(p.y);
        }
        b
    }

    pub fn width(&self) -> f64 {
        self.max.x - self.min.x
    }

    pub fn height(&self) -> f64 {
        self.max.y - self.min.y
    }

    pub fn center(&self) -> Pt {
        Pt::new((self.min.x + self.max.x) / 2.0, (self.min.y + self.max.y) / 2.0)
    }
}

/// A part: the outer contour (used for nesting) and everything inside it.
/// The outer contour is counter-clockwise, inner closed contours clockwise.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Shape {
    pub outer: Ring,
    /// Closed contours inside the part at any depth (holes, cut-outs, contours inside holes).
    pub holes: Vec<Ring>,
    /// Open lines inside the part.
    pub lines: Vec<Vec<Pt>>,
}

impl Shape {
    /// Normalises ring orientation.
    pub fn new(mut outer: Ring, mut holes: Vec<Ring>) -> Self {
        if signed_area(&outer) < 0.0 {
            outer.reverse();
        }
        for h in &mut holes {
            if signed_area(h) > 0.0 {
                h.reverse();
            }
        }
        Self { outer, holes, lines: Vec::new() }
    }

    /// Area of the outer contour: the fabric the part takes up in a layout.
    pub fn area(&self) -> f64 {
        signed_area(&self.outer)
    }

    /// Net material area by the even-odd rule: odd-depth contours subtract, even-depth ones add.
    pub fn net_area(&self) -> f64 {
        let depth = |h: &Ring| self.holes.iter().filter(|o| !std::ptr::eq(*o, h) && point_in_ring(h[0], o)).count();
        self.area()
            - self
                .holes
                .iter()
                .map(|h| if depth(h) % 2 == 0 { signed_area(h).abs() } else { -signed_area(h).abs() })
                .sum::<f64>()
    }

    pub fn bbox(&self) -> BBox {
        BBox::of(self.outer.iter().copied())
    }

    /// Direction of the long axis, degrees in (-90, 90], and the elongation (length along the axis /
    /// width across it). The axis is the direction across which the outer contour is narrowest.
    pub fn long_axis(&self) -> (f64, f64) {
        let extent = |deg: f64| {
            let (s, c) = deg.to_radians().sin_cos();
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for p in &self.outer {
                let v = c * p.x + s * p.y;
                lo = lo.min(v);
                hi = hi.max(v);
            }
            hi - lo
        };
        // Coarse 0.5° scan, then 0.01° around the best.
        let best = |from: f64, to: f64, step: f64| {
            let n = ((to - from) / step).round() as i64;
            (0..=n).map(|k| from + k as f64 * step).min_by(|a, b| extent(a + 90.0).total_cmp(&extent(b + 90.0))).unwrap()
        };
        let coarse = best(-89.5, 90.0, 0.5);
        let mut axis = best(coarse - 0.5, coarse + 0.5, 0.01);
        if axis <= -90.0 {
            axis += 180.0;
        } else if axis > 90.0 {
            axis -= 180.0;
        }
        let width = extent(axis + 90.0);
        (axis, if width > 0.0 { extent(axis) / width } else { f64::INFINITY })
    }

    /// Mirror image across the Y axis (x -> -x): a left-hand part from a right-hand one.
    pub fn mirrored(&self) -> Shape {
        let map = |r: &[Pt]| r.iter().map(|p| Pt::new(-p.x, p.y)).collect::<Vec<Pt>>();
        let mut s = Shape::new(map(&self.outer), self.holes.iter().map(|h| map(h)).collect());
        s.lines = self.lines.iter().map(|l| map(l)).collect();
        s
    }

    pub fn transformed(&self, t: &Transform) -> Shape {
        let map = |r: &Ring| r.iter().map(|&p| t.apply(p)).collect::<Ring>();
        Shape {
            outer: map(&self.outer),
            holes: self.holes.iter().map(map).collect(),
            lines: self.lines.iter().map(map).collect(),
        }
    }
}

/// Rotation by `rotation_deg` about the part origin, then translation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub rotation_deg: f64,
    pub translation: Pt,
}

impl Transform {
    pub fn apply(&self, p: Pt) -> Pt {
        let (s, c) = self.rotation_deg.to_radians().sin_cos();
        Pt::new(
            c * p.x - s * p.y + self.translation.x,
            s * p.x + c * p.y + self.translation.y,
        )
    }
}

/// A part read from one DXF.
#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub source: PathBuf,
    pub shape: Shape,
}

/// A part and how many are needed.
#[derive(Debug, Clone)]
pub struct Demand {
    pub part: Part,
    pub qty: usize,
    /// A filler (e.g. a small part of another job): up to `qty` copies go into free space of the
    /// sheets, never making a sheet longer or adding one. Leftover fillers are not an error.
    pub filler: bool,
}

/// One placed copy of a part.
#[derive(Debug, Clone)]
pub struct Placement {
    /// Index into the job's `Demand` list.
    pub part: usize,
    /// Maps the original DXF geometry to sheet coordinates.
    pub transform: Transform,
}

/// A fixed-size sheet of fabric. Width along Y, length along X, origin at the bottom-left corner.
#[derive(Debug, Clone)]
pub struct Sheet {
    pub width: f64,
    pub length: f64,
    /// Distance from the sheet start to the far end of the last part, mm.
    pub used_length: f64,
    pub placements: Vec<Placement>,
}

#[derive(Debug, Clone)]
pub struct Unplaced {
    pub part: usize,
    pub qty: usize,
    pub reason: String,
}

/// Nesting result: fixed-size sheets filled in order, and the parts that fit no sheet.
#[derive(Debug, Clone)]
pub struct NestResult {
    pub sheets: Vec<Sheet>,
    pub unplaced: Vec<Unplaced>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirrored_keeps_orientation_and_inner_geometry() {
        let mut s = Shape::new(
            vec![Pt::new(0.0, 0.0), Pt::new(4.0, 0.0), Pt::new(3.0, 2.0), Pt::new(0.0, 2.0)],
            vec![vec![Pt::new(1.0, 0.5), Pt::new(1.0, 1.5), Pt::new(2.0, 1.5), Pt::new(2.0, 0.5)]],
        );
        s.lines = vec![vec![Pt::new(0.5, 0.5), Pt::new(3.0, 1.0)]];
        let m = s.mirrored();
        assert_eq!(m.area(), s.area());
        assert!(signed_area(&m.holes[0]) < 0.0);
        assert_eq!(m.lines[0], vec![Pt::new(-0.5, 0.5), Pt::new(-3.0, 1.0)]);
        assert!(m.outer.contains(&Pt::new(-3.0, 2.0)));
        assert_eq!(m.mirrored().bbox(), s.bbox());
    }
}
