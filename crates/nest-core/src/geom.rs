//! Базовая геометрия: точки, контуры, детали, размещения и листы. Все размеры в мм.

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

/// Замкнутый контур без повтора первой точки в конце.
pub type Ring = Vec<Pt>;

/// Удвоенная знаковая площадь со знаком: > 0 для обхода против часовой стрелки.
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

/// Точка внутри контура (ray casting); точки на границе дают произвольный результат.
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

/// Многоугольник с отверстиями: внешний контур против часовой стрелки, отверстия по часовой.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub outer: Ring,
    pub holes: Vec<Ring>,
}

impl Shape {
    /// Приводит ориентацию контуров к канонической.
    pub fn new(mut outer: Ring, mut holes: Vec<Ring>) -> Self {
        if signed_area(&outer) < 0.0 {
            outer.reverse();
        }
        for h in &mut holes {
            if signed_area(h) > 0.0 {
                h.reverse();
            }
        }
        Self { outer, holes }
    }

    /// Площадь материала: внешний контур минус отверстия.
    pub fn area(&self) -> f64 {
        signed_area(&self.outer) + self.holes.iter().map(|h| signed_area(h)).sum::<f64>()
    }

    pub fn bbox(&self) -> BBox {
        BBox::of(self.outer.iter().copied())
    }

    pub fn transformed(&self, t: &Transform) -> Shape {
        let map = |r: &Ring| r.iter().map(|&p| t.apply(p)).collect::<Ring>();
        Shape {
            outer: map(&self.outer),
            holes: self.holes.iter().map(map).collect(),
        }
    }
}

/// Поворот на `rotation_deg` вокруг начала координат детали, затем сдвиг.
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

/// Деталь, прочитанная из одного DXF.
#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub source: PathBuf,
    pub shape: Shape,
}

/// Деталь и сколько её нужно.
#[derive(Debug, Clone)]
pub struct Demand {
    pub part: Part,
    pub qty: usize,
}

/// Одна размещённая копия детали.
#[derive(Debug, Clone)]
pub struct Placement {
    /// Индекс в списке `Demand` задания.
    pub part: usize,
    /// Преобразование исходной геометрии DXF в координаты листа.
    pub transform: Transform,
}

/// Отрез ткани. Ширина по Y, длина по X, начало координат в левом нижнем углу.
#[derive(Debug, Clone)]
pub struct Sheet {
    pub width: f64,
    pub length: f64,
    pub placements: Vec<Placement>,
}

#[derive(Debug, Clone)]
pub struct Unplaced {
    pub part: usize,
    pub qty: usize,
    pub reason: String,
}

/// Результат раскладки. Сейчас всегда один лист, но структура рассчитана на несколько.
#[derive(Debug, Clone)]
pub struct NestResult {
    pub sheets: Vec<Sheet>,
    pub unplaced: Vec<Unplaced>,
}
