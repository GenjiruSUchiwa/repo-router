//! Surface fixture covering every Rust-producible DefKind and reference form.

/// Docs for a free function.
#[inline]
pub fn free_function(value: u32) -> u32 {
    value.saturating_add(1)
}

/// Struct docs.
pub struct SurfaceStruct {
    pub field: u32,
}

pub enum SurfaceEnum {
    Unit,
    Tuple(u32),
    Record { value: u32 },
}

pub union SurfaceUnion {
    pub a: u32,
    pub b: f32,
}

pub trait SurfaceTrait {
    type Item;
    const TRAIT_CONST: u32;
    fn required(&self);
    fn defaulted(&self) {
        self.required();
    }
}

pub type SurfaceAlias = SurfaceStruct;

pub const SURFACE_CONST: u32 = 7;
pub static SURFACE_STATIC: u32 = 9;

pub mod nested {
    pub fn nested_free() {
        crate::free_function(1);
    }

    pub struct NestedService;

    impl NestedService {
        pub fn inherent(&self) {
            nested_free();
        }
    }

    pub trait NestedTrait {
        fn method(&self);
    }

    impl NestedTrait for NestedService {
        fn method(&self) {
            self.inherent();
        }
    }

    fn outer() {
        fn inner() {
            nested_free();
        }
        inner();
    }
}

impl SurfaceStruct {
    pub fn inherent_method(&self) {
        free_function(self.field);
    }
}

impl SurfaceTrait for SurfaceStruct {
    type Item = u32;
    const TRAIT_CONST: u32 = 1;
    fn required(&self) {
        self.inherent_method();
    }
}

macro_rules! surface_macro {
    ($x:expr) => {
        $x
    };
}

/// Unicode definition.
fn méthode() {
    let valeur = 1;
    let _ = valeur;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_test_surface() {
        assert_eq!(free_function(1), 2);
    }

    #[tokio::test]
    async fn tokio_style() {}

    #[rstest]
    fn rstest_style() {}

    #[test_case]
    fn test_case_style() {}
}

fn calls_surface() {
    free_function(1);
    module::free_function(1);
    free_function::<u32>(1);
    SurfaceStruct::inherent_method();
    value.inherent_method();
    println!("hi");
    crate::surface_macro!(1);
    value.method::<u8>();
    let _ = Some(1);
}

mod module {
    pub fn free_function(_v: u32) {}
}

// Ambiguous terminal names in different local scopes.
fn scope_a() {
    fn helper() {}
    helper();
}

fn scope_b() {
    fn helper() {}
    helper();
}

extern "C" {
    fn foreign_surface(x: i32) -> i32;
}
