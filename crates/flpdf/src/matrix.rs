//! Mirrors qpdf 11.9.0 libqpdf/QPDFMatrix.cc.
//! Public API: qpdf 11.9.0 include/qpdf/QPDFMatrix.hh.

/// An axis-aligned rectangle represented by its lower-left and upper-right corners.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rectangle {
    /// Lower-left x coordinate.
    pub llx: f64,
    /// Lower-left y coordinate.
    pub lly: f64,
    /// Upper-right x coordinate.
    pub urx: f64,
    /// Upper-right y coordinate.
    pub ury: f64,
}

impl Rectangle {
    /// Creates a rectangle from its lower-left and upper-right corners.
    pub const fn new(llx: f64, lly: f64, urx: f64, ury: f64) -> Self {
        Self { llx, lly, urx, ury }
    }
}

impl From<[f64; 4]> for Rectangle {
    fn from([llx, lly, urx, ury]: [f64; 4]) -> Self {
        Self::new(llx, lly, urx, ury)
    }
}

impl From<Rectangle> for [f64; 4] {
    fn from(rectangle: Rectangle) -> Self {
        [rectangle.llx, rectangle.lly, rectangle.urx, rectangle.ury]
    }
}

/// A PDF affine transformation matrix.
///
/// The six values represent the matrix `[a b c d e f]`. Points are transformed
/// as `(a*x + c*y + e, b*x + d*y + f)`.
///
/// # Examples
///
/// ```
/// use flpdf::Matrix;
///
/// let mut matrix = Matrix::default();
/// matrix.translate(10.0, 20.0);
/// assert_eq!(matrix.transform(1.0, 2.0), (11.0, 22.0));
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    /// Horizontal scale and rotation component.
    pub a: f64,
    /// Vertical rotation component.
    pub b: f64,
    /// Horizontal rotation component.
    pub c: f64,
    /// Vertical scale and rotation component.
    pub d: f64,
    /// Horizontal translation.
    pub e: f64,
    /// Vertical translation.
    pub f: f64,
}

impl Default for Matrix {
    fn default() -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
    }
}

impl From<[f64; 6]> for Matrix {
    fn from([a, b, c, d, e, f]: [f64; 6]) -> Self {
        Self::new(a, b, c, d, e, f)
    }
}

impl From<Matrix> for [f64; 6] {
    fn from(matrix: Matrix) -> Self {
        matrix.get_as_matrix()
    }
}

impl Matrix {
    /// Creates a matrix from its six PDF matrix components.
    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// Returns the six PDF matrix components.
    pub const fn get_as_matrix(self) -> [f64; 6] {
        [self.a, self.b, self.c, self.d, self.e, self.f]
    }

    /// Concatenates `other` using qpdf's matrix multiplication order.
    pub fn concat(&mut self, other: Self) {
        let ap = (self.a * other.a) + (self.c * other.b);
        let bp = (self.b * other.a) + (self.d * other.b);
        let cp = (self.a * other.c) + (self.c * other.d);
        let dp = (self.b * other.c) + (self.d * other.d);
        let ep = (self.a * other.e) + (self.c * other.f) + self.e;
        let fp = (self.b * other.e) + (self.d * other.f) + self.f;
        self.a = ap;
        self.b = bp;
        self.c = cp;
        self.d = dp;
        self.e = ep;
        self.f = fp;
    }

    /// Concatenates a scale transformation.
    pub fn scale(&mut self, sx: f64, sy: f64) {
        self.concat(Self::new(sx, 0.0, 0.0, sy, 0.0, 0.0));
    }

    /// Concatenates a translation transformation.
    pub fn translate(&mut self, tx: f64, ty: f64) {
        self.concat(Self::new(1.0, 0.0, 0.0, 1.0, tx, ty));
    }

    /// Concatenates a quarter-turn for 90, 180, or 270 degrees.
    ///
    /// Other angles leave the matrix unchanged.
    pub fn rotatex90(&mut self, angle: i32) {
        match angle {
            90 => self.concat(Self::new(0.0, 1.0, -1.0, 0.0, 0.0, 0.0)),
            180 => self.concat(Self::new(-1.0, 0.0, 0.0, -1.0, 0.0, 0.0)),
            270 => self.concat(Self::new(0.0, -1.0, 1.0, 0.0, 0.0, 0.0)),
            _ => {}
        }
    }

    /// Transforms a point.
    pub fn transform(self, x: f64, y: f64) -> (f64, f64) {
        (
            (self.a * x) + (self.c * y) + self.e,
            (self.b * x) + (self.d * y) + self.f,
        )
    }

    /// Returns the tight axis-aligned bounds of a transformed rectangle.
    pub fn transform_rectangle(self, rectangle: Rectangle) -> Rectangle {
        let points = [
            self.transform(rectangle.llx, rectangle.lly),
            self.transform(rectangle.llx, rectangle.ury),
            self.transform(rectangle.urx, rectangle.lly),
            self.transform(rectangle.urx, rectangle.ury),
        ];
        Rectangle::new(
            points
                .iter()
                .map(|point| point.0)
                .fold(f64::INFINITY, f64::min),
            points
                .iter()
                .map(|point| point.1)
                .fold(f64::INFINITY, f64::min),
            points
                .iter()
                .map(|point| point.0)
                .fold(f64::NEG_INFINITY, f64::max),
            points
                .iter()
                .map(|point| point.1)
                .fold(f64::NEG_INFINITY, f64::max),
        )
    }

    /// Serializes the six components as qpdf-formatted real numbers.
    pub fn unparse(self) -> String {
        self.get_as_matrix()
            .into_iter()
            .map(format_component)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn format_component(value: f64) -> String {
    let value = if value > -0.00001 && value < 0.00001 {
        0.0
    } else {
        value
    };
    format!("{value:.5}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}
