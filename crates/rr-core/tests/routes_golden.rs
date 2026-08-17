//! The bytes of `.rr/ROUTES.md`, pinned.
//!
//! `.rr/ROUTES.md` is read by `grep` and `awk` far more often than by this
//! crate, so its layout is a contract with tools that will never be recompiled
//! against a change. This is also the test that fails the moment anyone adds a
//! clock, a counter, a last-seen timestamp, or any other field whose value is
//! not a pure function of the routes: two records in, the same bytes out,
//! forever.

use rr_core::result::Confidence;
use rr_core::text::{
    encode_map_destination, parse_routes, render_routes, Digest, RouteKey, RouteRecord, RouteTable,
    MAX_ROUTES,
};

/// The two questions of the issue's §1, learned against one corpus.
///
/// Both records share an `api_identity` because a route is an answer about the
/// whole index rather than about one directory: corpus-wide staleness is not an
/// implementation detail here, it is visible in the artifact.
fn table() -> RouteTable {
    let api_identity = Digest::of_bytes(b"repository api");
    let mut table = RouteTable::default();
    for (key, anchor, confidence) in [
        ("token verify", "src/auth/token.rs#verify_token", 1.0_f32),
        (
            "rotate signing",
            "src/auth/keys.rs#rotate_signing_key",
            0.813_263_4,
        ),
    ] {
        assert!(table.insert(RouteRecord {
            key: RouteKey::new(key).expect("a two-word key"),
            anchor: anchor.to_owned(),
            map: encode_map_destination("src/auth/MAP.md"),
            api_identity,
            confidence: Confidence::new(confidence).expect("in range"),
        }));
    }
    table
}

#[test]
fn the_routes_file_renders_exactly_this() {
    let bytes = render_routes(&table()).expect("render");
    let text = String::from_utf8(bytes).expect("the artifact is UTF-8");
    insta::assert_snapshot!("routes_file", text);
}

/// Rendering is a function of the routes and nothing else, so rendering the
/// same table twice in one process — and reading it back and rendering again —
/// cannot differ. Without this, the snapshot above could pass while the file
/// still carried something that changes between runs.
#[test]
fn the_routes_file_is_a_function_of_its_routes_alone() {
    let first = render_routes(&table()).expect("render");
    let second = render_routes(&table()).expect("render again");
    assert_eq!(first, second);

    let reparsed = parse_routes(&first).expect("the golden bytes parse");
    assert_eq!(render_routes(&reparsed).expect("re-render"), first);
}

/// The bound is part of the format's promise — a file a human can grep is a
/// file that stays a readable size — so it is pinned where the bytes are.
#[test]
fn the_bound_is_the_one_the_format_promises() {
    assert_eq!(MAX_ROUTES, 1024);
}
