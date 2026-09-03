//! Measurement primitives. All public signatures use [`Length`] — never a bare `f64` —
//! so a caller can never accidentally mix inches and points.

/// A length stored internally in PDF points (1 in = 72 pt).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Length(f64);

const POINTS_PER_INCH: f64 = 72.0;
const MM_PER_INCH: f64 = 25.4;

impl Length {
    pub const ZERO: Length = Length(0.0);

    pub fn from_inches(inches: f64) -> Self {
        Length(inches * POINTS_PER_INCH)
    }

    pub fn from_mm(mm: f64) -> Self {
        Length(mm / MM_PER_INCH * POINTS_PER_INCH)
    }

    pub const fn from_points(points: f64) -> Self {
        Length(points)
    }

    pub fn as_inches(self) -> f64 {
        self.0 / POINTS_PER_INCH
    }

    pub fn as_mm(self) -> f64 {
        self.0 / POINTS_PER_INCH * MM_PER_INCH
    }

    pub fn as_points(self) -> f64 {
        self.0
    }

    pub fn abs(self) -> Length {
        Length(self.0.abs())
    }
}

impl std::ops::Add for Length {
    type Output = Length;
    fn add(self, rhs: Length) -> Length {
        Length(self.0 + rhs.0)
    }
}

impl std::ops::Sub for Length {
    type Output = Length;
    fn sub(self, rhs: Length) -> Length {
        Length(self.0 - rhs.0)
    }
}

impl std::ops::Mul<f64> for Length {
    type Output = Length;
    fn mul(self, rhs: f64) -> Length {
        Length(self.0 * rhs)
    }
}

impl std::ops::Div<f64> for Length {
    type Output = Length;
    fn div(self, rhs: f64) -> Length {
        Length(self.0 / rhs)
    }
}

/// A width x height pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub width: Length,
    pub height: Length,
}

impl Size {
    pub fn new(width: Length, height: Length) -> Self {
        Size { width, height }
    }

    /// Grow (or shrink, for a negative amount) the size by `amount` on every side —
    /// i.e. `amount` is added to each dimension twice (once per side).
    pub fn outset(self, amount: Length) -> Size {
        Size {
            width: self.width + amount * 2.0,
            height: self.height + amount * 2.0,
        }
    }

    pub fn approx_eq(self, other: Size, tolerance: Length) -> bool {
        (self.width - other.width).abs() <= tolerance
            && (self.height - other.height).abs() <= tolerance
    }
}

/// An axis-aligned rectangle, `[x0 y0 x1 y1]` in PDF convention (origin bottom-left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: Length,
    pub y0: Length,
    pub x1: Length,
    pub y1: Length,
}

impl Rect {
    pub fn from_origin_size(size: Size) -> Self {
        Rect {
            x0: Length::ZERO,
            y0: Length::ZERO,
            x1: size.width,
            y1: size.height,
        }
    }

    pub fn width(self) -> Length {
        self.x1 - self.x0
    }

    pub fn height(self) -> Length {
        self.y1 - self.y0
    }

    /// Move every edge inward (positive `amount`) or outward (negative `amount`).
    pub fn inset(self, amount: Length) -> Rect {
        Rect {
            x0: self.x0 + amount,
            y0: self.y0 + amount,
            x1: self.x1 - amount,
            y1: self.y1 - amount,
        }
    }

    /// As a PDF array `[x0 y0 x1 y1]` in points, for embedding in a page's box entries.
    pub fn as_pdf_array_points(self) -> [f64; 4] {
        [
            self.x0.as_points(),
            self.y0.as_points(),
            self.x1.as_points(),
            self.y1.as_points(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_by_nine_inches_is_432_by_648_points() {
        let w = Length::from_inches(6.0);
        let h = Length::from_inches(9.0);
        assert!((w.as_points() - 432.0).abs() < 1e-9);
        assert!((h.as_points() - 648.0).abs() < 1e-9);
    }

    #[test]
    fn inch_round_trip_stays_within_tolerance() {
        let original = 6.25_f64;
        let l = Length::from_inches(original);
        assert!((l.as_inches() - original).abs() < 0.001 / 72.0);
    }

    #[test]
    fn mm_round_trip_stays_within_tolerance() {
        let original = 210.0_f64; // A4 width in mm
        let l = Length::from_mm(original);
        assert!((l.as_mm() - original).abs() < 0.001);
    }

    #[test]
    fn points_round_trip_is_exact() {
        let l = Length::from_points(450.0);
        assert_eq!(l.as_points(), 450.0);
    }

    #[test]
    fn outset_adds_amount_to_each_side() {
        let trim = Size::new(Length::from_inches(6.0), Length::from_inches(9.0));
        let with_bleed = trim.outset(Length::from_inches(0.125));
        // 0.125 in bleed per side => +0.25 in per dimension
        let expected = Size::new(Length::from_inches(6.25), Length::from_inches(9.25));
        assert!(with_bleed.approx_eq(expected, Length::from_points(0.01)));
    }

    #[test]
    fn rect_inset_moves_all_four_edges() {
        let r = Rect::from_origin_size(Size::new(
            Length::from_points(450.0),
            Length::from_points(666.0),
        ));
        let inset = r.inset(Length::from_points(9.0));
        assert_eq!(inset.as_pdf_array_points(), [9.0, 9.0, 441.0, 657.0]);
    }
}

/// A 2D affine transform, stored as the PDF `cm` operand order `[a b c d e f]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn translate(tx: Length, ty: Length) -> Self {
        Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx.as_points(),
            f: ty.as_points(),
        }
    }

    pub fn scale(sx: f64, sy: f64) -> Self {
        Matrix {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            e: 0.0,
            f: 0.0,
        }
    }

    pub fn scale_uniform(s: f64) -> Self {
        Matrix::scale(s, s)
    }

    /// Rotation by `degrees`, counter-clockwise, about the origin.
    pub fn rotate_degrees(degrees: f64) -> Self {
        let r = degrees.to_radians();
        Matrix {
            a: r.cos(),
            b: r.sin(),
            c: -r.sin(),
            d: r.cos(),
            e: 0.0,
            f: 0.0,
        }
    }

    /// `self` applied first, then `other` — i.e. `other * self` in matrix-multiplication order,
    /// matching the PDF convention that later `cm` operators post-multiply the CTM.
    pub fn then(self, other: Matrix) -> Matrix {
        Matrix {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    /// The `[a b c d e f]` operand list for a PDF `cm` operator.
    pub fn as_cm_operands(self) -> [f64; 6] {
        [self.a, self.b, self.c, self.d, self.e, self.f]
    }

    pub fn apply_to_point(self, x: Length, y: Length) -> (Length, Length) {
        let x = x.as_points();
        let y = y.as_points();
        (
            Length::from_points(self.a * x + self.c * y + self.e),
            Length::from_points(self.b * x + self.d * y + self.f),
        )
    }
}

#[cfg(test)]
mod matrix_tests {
    use super::*;

    #[test]
    fn identity_is_a_no_op() {
        let p = Matrix::IDENTITY.apply_to_point(Length::from_points(9.0), Length::from_points(9.0));
        assert_eq!(p.0.as_points(), 9.0);
        assert_eq!(p.1.as_points(), 9.0);
    }

    #[test]
    fn translate_offsets_a_point() {
        let m = Matrix::translate(Length::from_points(9.0), Length::from_points(9.0));
        let p = m.apply_to_point(Length::ZERO, Length::ZERO);
        assert_eq!(p.0.as_points(), 9.0);
        assert_eq!(p.1.as_points(), 9.0);
    }

    #[test]
    fn scale_uniform_scales_both_axes() {
        let m = Matrix::scale_uniform(6.25 / 6.0);
        let p = m.apply_to_point(Length::from_points(432.0), Length::from_points(648.0));
        assert!((p.0.as_points() - 450.0).abs() < 1e-9);
        assert!((p.1.as_points() - 675.0).abs() < 1e-9);
    }

    #[test]
    fn rotate_90_degrees_maps_x_axis_onto_y_axis() {
        let m = Matrix::rotate_degrees(90.0);
        let p = m.apply_to_point(Length::from_points(1.0), Length::ZERO);
        assert!(p.0.as_points().abs() < 1e-9);
        assert!((p.1.as_points() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn then_composes_scale_before_translate() {
        // scale by 2, then translate by (10, 0): point (1,0) -> (2,0) -> (12, 0)
        let m = Matrix::scale_uniform(2.0)
            .then(Matrix::translate(Length::from_points(10.0), Length::ZERO));
        let p = m.apply_to_point(Length::from_points(1.0), Length::ZERO);
        assert!((p.0.as_points() - 12.0).abs() < 1e-9);
    }

    #[test]
    fn cm_operands_match_pdf_operator_order() {
        let m = Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 9.0,
            f: 9.0,
        };
        assert_eq!(m.as_cm_operands(), [1.0, 0.0, 0.0, 1.0, 9.0, 9.0]);
    }
}
