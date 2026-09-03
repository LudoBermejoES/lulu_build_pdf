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
        Length::guarded(inches * POINTS_PER_INCH)
    }

    pub fn from_mm(mm: f64) -> Self {
        Length::guarded(mm / MM_PER_INCH * POINTS_PER_INCH)
    }

    /// Raw, unchecked construction from a points value already known to be
    /// finite — e.g. a literal, or a number already validated while reading
    /// a PDF object (see `pdf::as_f64`). Kept as a `const fn` (so it can
    /// build compile-time constants) and therefore deliberately does not run
    /// the [`NaN` debug guard](#nan-and-infinity) the other constructors do;
    /// prefer [`Length::checked_from_points`] at a trust boundary where the
    /// input has not already been validated.
    pub const fn from_points(points: f64) -> Self {
        Length(points)
    }

    /// Fallible construction for a trust boundary — a value read out of a
    /// PDF object, a user-supplied config value, or the result of a division
    /// that could be `0.0 / 0.0` — where the input may legitimately be `NaN`
    /// or infinite and the caller needs to turn that into an error or a
    /// finding rather than let it silently propagate into geometry math and
    /// eventually into a `cm` operator written into generated PDF content.
    pub fn checked_from_points(points: f64) -> Option<Self> {
        points.is_finite().then_some(Length(points))
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

    /// `false` for `NaN` and for either infinity. A [`Length`] built through
    /// the unchecked, `const` [`Length::from_points`] or produced by
    /// arithmetic on an already-non-finite `Length` can end up non-finite;
    /// this is how a caller downstream of that (e.g. before writing a `cm`
    /// operand) checks for it.
    pub fn is_finite(self) -> bool {
        self.0.is_finite()
    }

    /// Debug-only guard against a `NaN` result: panics in a debug build,
    /// is a no-op in release. `NaN` is the class of bug this exists to catch
    /// (e.g. a degenerate `0.0 / 0.0` scale factor from a zero-size
    /// dimension elsewhere in the crate) — it should never occur so a debug
    /// build should fail loudly and immediately at the point of construction
    /// rather than letting the value travel silently into a report or, worse,
    /// into written PDF content. It is deliberately not a release-mode panic:
    /// `Length` arithmetic is used pervasively throughout this crate as
    /// infallible, and turning every one of those call sites into a fallible
    /// one — or panicking a release build on a class of input a caller may
    /// not have had the chance to validate yet — would be a worse failure
    /// mode than the bug itself. A caller that already knows a value may
    /// legitimately be non-finite (a boundary reading external input) should
    /// use [`Length::checked_from_points`] instead of a plain constructor.
    fn guarded(points: f64) -> Length {
        debug_assert!(
            !points.is_nan(),
            "Length constructed from NaN (points = {points}); this indicates \
             a degenerate calculation upstream (e.g. dividing by a zero-size \
             dimension) that should be refused before it reaches this point"
        );
        Length(points)
    }
}

impl std::ops::Add for Length {
    type Output = Length;
    fn add(self, rhs: Length) -> Length {
        Length::guarded(self.0 + rhs.0)
    }
}

impl std::ops::Sub for Length {
    type Output = Length;
    fn sub(self, rhs: Length) -> Length {
        Length::guarded(self.0 - rhs.0)
    }
}

impl std::ops::Mul<f64> for Length {
    type Output = Length;
    fn mul(self, rhs: f64) -> Length {
        Length::guarded(self.0 * rhs)
    }
}

impl std::ops::Div<f64> for Length {
    type Output = Length;
    fn div(self, rhs: f64) -> Length {
        Length::guarded(self.0 / rhs)
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

    /// Move every edge inward (positive `amount`) or outward (negative
    /// `amount`).
    ///
    /// An inset large enough to cross an axis (`amount` more than half that
    /// axis's extent) is clamped to a single point at that axis's midpoint
    /// rather than producing an inverted rectangle (`x0 > x1` or `y0 > y1`).
    /// The one real caller this matters for is
    /// `cover::generate_template`'s spine-safety guide: a narrow spine panel
    /// inset by a safety margin wider than half the spine would otherwise
    /// draw a box whose edges had crossed and mirrored outward past the
    /// panel's own boundary into its neighbours — a flattened, zero-size box
    /// at the panel's centre is a far less misleading result than that.
    pub fn inset(self, amount: Length) -> Rect {
        let (x0, x1) = clamp_inset(self.x0, self.x1, amount);
        let (y0, y1) = clamp_inset(self.y0, self.y1, amount);
        Rect { x0, y0, x1, y1 }
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

/// One axis of [`Rect::inset`]: insets `(lo, hi)` by `amount` on each end,
/// clamping to their shared midpoint instead of crossing over.
fn clamp_inset(lo: Length, hi: Length, amount: Length) -> (Length, Length) {
    let new_lo = lo + amount;
    let new_hi = hi - amount;
    if new_lo <= new_hi {
        (new_lo, new_hi)
    } else {
        let mid = (lo + hi) / 2.0;
        (mid, mid)
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

    #[test]
    fn rect_inset_larger_than_half_the_width_clamps_instead_of_inverting() {
        // A 20pt-wide spine panel inset by a 30pt safety margin: unclamped
        // math would give x0=30, x1=-10 (x0 > x1, an inverted rectangle that
        // would draw a mirrored box straddling the neighbouring panels).
        let panel = Rect {
            x0: Length::from_points(0.0),
            y0: Length::from_points(0.0),
            x1: Length::from_points(20.0),
            y1: Length::from_points(100.0),
        };
        let inset = panel.inset(Length::from_points(30.0));
        assert!(inset.x0 <= inset.x1, "must not invert: {inset:?}");
        // Clamped to a single point at the midpoint of the crossed axis.
        assert_eq!(inset.x0.as_points(), 10.0);
        assert_eq!(inset.x1.as_points(), 10.0);
        // The untouched axis (y) is unaffected.
        assert_eq!(inset.y0.as_points(), 30.0);
        assert_eq!(inset.y1.as_points(), 70.0);
    }

    #[test]
    fn rect_inset_exactly_at_the_midpoint_is_not_inverted() {
        let r = Rect::from_origin_size(Size::new(
            Length::from_points(20.0),
            Length::from_points(20.0),
        ));
        let inset = r.inset(Length::from_points(10.0));
        assert_eq!(inset.as_pdf_array_points(), [10.0, 10.0, 10.0, 10.0]);
    }

    #[test]
    fn checked_from_points_rejects_nan_and_infinity() {
        assert!(Length::checked_from_points(f64::NAN).is_none());
        assert!(Length::checked_from_points(f64::INFINITY).is_none());
        assert!(Length::checked_from_points(f64::NEG_INFINITY).is_none());
        assert_eq!(
            Length::checked_from_points(450.0).map(Length::as_points),
            Some(450.0)
        );
    }

    #[test]
    fn is_finite_reflects_the_underlying_value() {
        assert!(Length::from_points(450.0).is_finite());
        assert!(!Length::from_points(f64::NAN).is_finite());
        assert!(!Length::from_points(f64::INFINITY).is_finite());
    }

    #[test]
    #[should_panic(expected = "Length constructed from NaN")]
    fn from_inches_of_nan_panics_in_debug() {
        let _ = Length::from_inches(f64::NAN);
    }

    #[test]
    #[should_panic(expected = "Length constructed from NaN")]
    fn arithmetic_producing_nan_panics_in_debug() {
        // infinity - infinity = NaN
        let inf = Length::from_points(f64::INFINITY);
        let _ = inf - inf;
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
