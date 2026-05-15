//! Integration tests for [`krypt_core::include`].
//!
//! Each test exercises a distinct scenario documented in issue #10.  Fixtures
//! live under `tests/fixtures/include/`.

use std::path::PathBuf;

use krypt_core::include::{load_with_includes, IncludeError};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("include")
        .join(name)
}

// ── 1. Simple include ────────────────────────────────────────────────────────

/// Root declares one link, included file declares one link.
/// Merged config has both in order (root first), `include` field is empty.
#[test]
fn simple_include_merges_links() {
    let cfg = load_with_includes(fixture("simple_root.toml")).expect("should expand");
    assert_eq!(cfg.links.len(), 2, "expected root + inc1 links");
    assert_eq!(cfg.links[0].dst, "/tmp/root.conf");
    assert_eq!(cfg.links[1].dst, "/tmp/inc1.conf");
    assert!(cfg.include.is_empty(), "include list must be cleared");
}

// ── 2. Glob include ──────────────────────────────────────────────────────────

/// Root includes `parts/*.toml` which matches 3 files.
/// Result has root link + 3 included links in sorted glob order.
#[test]
fn glob_include_expands_all_matching_files() {
    let cfg = load_with_includes(fixture("glob_root.toml")).expect("should expand");
    // 1 root + 3 parts
    assert_eq!(cfg.links.len(), 4, "expected 4 links total");
    assert_eq!(cfg.links[0].dst, "/tmp/root.conf");
    // parts are sorted: part_a, part_b, part_c
    assert_eq!(cfg.links[1].dst, "/tmp/part_a.conf");
    assert_eq!(cfg.links[2].dst, "/tmp/part_b.conf");
    assert_eq!(cfg.links[3].dst, "/tmp/part_c.conf");
    assert!(cfg.include.is_empty());
}

// ── 3. Nested include ────────────────────────────────────────────────────────

/// A includes B, B includes C.  Final result has entries from A, B, C in order.
#[test]
fn nested_include_three_levels() {
    let cfg = load_with_includes(fixture("nested/a.toml")).expect("should expand");
    assert_eq!(cfg.links.len(), 3, "expected a + b + c links");
    assert_eq!(cfg.links[0].dst, "/tmp/a.conf");
    assert_eq!(cfg.links[1].dst, "/tmp/b.conf");
    assert_eq!(cfg.links[2].dst, "/tmp/c.conf");
    assert!(cfg.include.is_empty());
}

// ── 4. Cycle detected ────────────────────────────────────────────────────────

/// A includes B, B includes A → IncludeError::Cycle.
/// Error message contains the cycle chain.
#[test]
fn cycle_is_detected() {
    let err = load_with_includes(fixture("cycle/a.toml")).expect_err("should detect cycle");
    match err {
        IncludeError::Cycle { chain } => {
            assert!(
                chain.contains("a.toml") || chain.contains("b.toml"),
                "chain should mention a.toml or b.toml; got: {chain}"
            );
        }
        other => panic!("expected Cycle, got: {other:?}"),
    }
}

// ── 5. Depth exceeded ────────────────────────────────────────────────────────

/// A chain of 10 files (d0 -> d1 -> … -> d9) exceeds the default depth limit
/// of 8 and returns IncludeError::DepthExceeded.
#[test]
fn depth_exceeded() {
    let err = load_with_includes(fixture("depth/d0.toml")).expect_err("should exceed depth");
    match err {
        IncludeError::DepthExceeded { max, .. } => {
            assert_eq!(max, 8);
        }
        other => panic!("expected DepthExceeded, got: {other:?}"),
    }
}

// ── 6. Map merge — later wins ────────────────────────────────────────────────

/// Root sets `HOME = "/a"`, included file sets `HOME = "/b"`.
/// Result has `HOME = "/b"` (later wins).  Key only in root is kept.
#[test]
fn map_merge_later_wins_on_conflict() {
    let cfg = load_with_includes(fixture("map_merge_root.toml")).expect("should expand");
    assert_eq!(cfg.paths["HOME"], "/b", "later include should win");
    assert_eq!(cfg.paths["EXTRA"], "keep", "root-only key must survive");
}

// ── 7. Vec merge — appended ──────────────────────────────────────────────────

/// Root has 2 links, included file has 1 link.  Result has 3 links in order.
#[test]
fn vec_merge_appends_in_order() {
    let cfg = load_with_includes(fixture("vec_merge_root.toml")).expect("should expand");
    assert_eq!(cfg.links.len(), 3, "expected 2 root + 1 include links");
    assert_eq!(cfg.links[0].dst, "/tmp/root1.conf");
    assert_eq!(cfg.links[1].dst, "/tmp/root2.conf");
    assert_eq!(cfg.links[2].dst, "/tmp/inc1.conf");
}
