

#[derive(Copy, Clone)]
#[repr(C)]
pub struct Transform2F {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl Transform2F {
    pub fn row_major(m11: f32, m12: f32, m13: f32, m21: f32, m22: f32, m23: f32) -> Transform2F {
        Transform2F {
            a: m11,
            b: m21,
            c: m12,
            d: m22,
            e: m13,
            f: m23
        }
    }
    pub fn from_scale(scale: Vector2F) -> Self {
        Transform2F { a: scale.x, b: 0.0, c: 0.0, d: scale.y, e: 0.0, f: 0.0 }
    }
    pub fn from_translation(tr: Vector2F) -> Self {
        Transform2F { a: 0.0, b: 0.0, c: 0.0, d: 0.0, e: tr.x, f: tr.y }
    }
    fn equals(&self, rhs: &Self) -> bool {
        const E: f32 = 1e-6;
        (self.a - rhs.a).abs() < E &&
        (self.b - rhs.b).abs() < E &&
        (self.c - rhs.c).abs() < E &&
        (self.d - rhs.d).abs() < E &&
        (self.e - rhs.e).abs() < E &&
        (self.f - rhs.f).abs() < E
    }
    fn m11(&self) -> f32 {
        self.a
    }
    fn m22(&self) -> f32 {
        self.d
    }
}
impl Default for Transform2F {
    fn default() -> Self {
        Transform2F { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 }
    }
}
impl std::ops::Mul for Transform2F {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self::Output {
        Transform2F {
            a: self.a * rhs.a + self.c * rhs.b,
            b: self.b * rhs.a + self.d * rhs.b,
            c: self.a * rhs.c + self.c * rhs.d,
            d: self.b * rhs.c + self.d * rhs.d,
            e: self.a * rhs.e + self.c * rhs.f + self.e,
            f: self.b * rhs.e + self.d * rhs.f + self.f,
        }
    }
}

impl From<pathfinder_geometry::transform2d::Transform2F> for Transform2F {
    fn from(t: pathfinder_geometry::transform2d::Transform2F) -> Self {
        Self::row_major(t.m11(), t.m12(), t.m13(), t.m21(), t.m22(), t.m23())

    }
}

#[test]
fn test_matmul() {
    for i in 0 .. 10 {
        let mut m: [f32; 6] = rand::random();
        let rand_mat = || pathfinder_geometry::transform2d::Transform2F::row_major(
            m[0], m[1], m[2], m[3], m[4], m[5]
        );

        let a_pa = rand_mat();
        let b_pa = rand_mat();
        let a: Transform2F = a_pa.into();
        let b: Transform2F = b_pa.into();

        let c_pa = a_pa * b_pa;
        let c = a * b;
        assert!(Transform2F::from(c_pa).equals(&c));
    }
}

#[repr(C)]
pub struct Vector2F {
    x: f32,
    y: f32
}
impl Vector2F {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}
