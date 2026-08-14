use crate::a::b;
use crate::a::{self, b, c as d, nested::{e, *}};
pub use crate::internal::Client as ApiClient;
extern crate alloc as allocator;
use super::x;
use self::y;
use a as _;
use crate::a::b;

fn block_local() {
    use crate::z;
    let _ = z;
}

mod holder {
    use crate::inside_mod::Item;
}
