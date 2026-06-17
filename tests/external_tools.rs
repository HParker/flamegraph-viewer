//! Cross-validation against an external, independent flame-graph tool.
//!
//! [inferno](https://github.com/jonhoo/inferno) is the Rust port of Brendan
//! Gregg's FlameGraph and folds `perf script` output exactly the way
//! `perf report` aggregates: per function, with instruction offsets stripped and
//! each sample weighted by its period. If our parser and aggregation are
//! correct, the per-function event counts we compute must match inferno's
//! folded output to the event.
//!
//! The test is skipped (not failed) when `inferno-collapse-perf` is not
//! installed, so it never breaks a machine without the tool.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use flamegraph_viewer::flame;
use flamegraph_viewer::parsers;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Locate `inferno-collapse-perf` on `PATH` or in the default cargo bin dir.
fn inferno() -> Option<PathBuf> {
    if Command::new("inferno-collapse-perf").arg("--help").output().is_ok() {
        return Some(PathBuf::from("inferno-collapse-perf"));
    }
    let cargo_bin = PathBuf::from(std::env::var("HOME").ok()?)
        .join(".cargo/bin/inferno-collapse-perf");
    cargo_bin.exists().then_some(cargo_bin)
}

/// Run inferno and parse its folded output into `(self-by-leaf, total events)`
/// plus the inclusive total per function (dropping the leading comm frame that
/// inferno prepends and that our IR does not model).
fn inferno_fold(bin: &Path, profile: &Path) -> (BTreeMap<String, u64>, BTreeMap<String, u64>, u64) {
    let out = Command::new(bin).arg(profile).output().expect("run inferno");
    assert!(out.status.success(), "inferno failed: {}", String::from_utf8_lossy(&out.stderr));
    let folded = String::from_utf8(out.stdout).unwrap();

    let mut self_by_leaf: BTreeMap<String, u64> = BTreeMap::new();
    let mut total_by_fn: BTreeMap<String, u64> = BTreeMap::new();
    let mut total_events = 0u64;

    for line in folded.lines() {
        let (path, count) = line.rsplit_once(' ').expect("folded line has a count");
        let count: u64 = count.parse().expect("count is a number");
        total_events += count;

        let frames: Vec<&str> = path.split(';').collect();
        if let Some(leaf) = frames.last() {
            *self_by_leaf.entry((*leaf).to_string()).or_default() += count;
        }
        // Skip frames[0]: inferno prepends the command name as a synthetic root.
        let mut seen = std::collections::HashSet::new();
        for &f in frames.iter().skip(1) {
            if seen.insert(f) {
                *total_by_fn.entry(f.to_string()).or_default() += count;
            }
        }
    }
    (self_by_leaf, total_by_fn, total_events)
}

#[test]
fn per_function_counts_match_inferno() {
    let Some(bin) = inferno() else {
        eprintln!("skipping: inferno-collapse-perf not installed");
        return;
    };

    let (inferno_self, inferno_total, inferno_events) = inferno_fold(&bin, &fixture("weighted.perf"));

    // Aggregate our per-thread flat profiles across the whole capture, the way
    // inferno folds every thread into one set of stacks.
    let profile = parsers::load(&fixture("weighted.perf")).unwrap();
    let mut our_self: BTreeMap<String, u64> = BTreeMap::new();
    let mut our_total: BTreeMap<String, u64> = BTreeMap::new();
    for thread in &profile.threads {
        for row in flame::flat_profile(thread) {
            if row.self_events > 0 {
                *our_self.entry(row.key.clone()).or_default() += row.self_events;
            }
            *our_total.entry(row.key).or_default() += row.total_events;
        }
    }

    assert_eq!(profile.event_count(), inferno_events, "total events must match inferno");
    assert_eq!(our_self, inferno_self, "per-function self events must match inferno");
    assert_eq!(our_total, inferno_total, "per-function inclusive events must match inferno");
}

/// The numbers `perf report` would print for `weighted.perf`, computed by hand
/// from the fixture's periods, pinned so a regression in weighting, offset
/// stripping or function grouping is caught even without inferno installed.
#[test]
fn matches_hand_computed_perf_report() {
    let profile = parsers::load(&fixture("weighted.perf")).unwrap();

    // Event totals: app = 5+3+2+10 = 20, worker = 4+6 = 10.
    assert_eq!(profile.event_count(), 30);

    let app = profile.threads.iter().find(|t| t.tid == 1).unwrap();
    let rows: BTreeMap<String, (u64, u64)> = flame::flat_profile(app)
        .into_iter()
        .map(|r| (r.key, (r.self_events, r.total_events)))
        .collect();
    // (self events, total events), offsets stripped so alpha+0x4 / alpha+0x8 fuse.
    assert_eq!(rows["main"], (0, 20));
    assert_eq!(rows["alpha"], (8, 8));
    assert_eq!(rows["gamma"], (12, 12));

    let worker = profile.threads.iter().find(|t| t.tid == 2).unwrap();
    let rows: BTreeMap<String, (u64, u64)> = flame::flat_profile(worker)
        .into_iter()
        .map(|r| (r.key, (r.self_events, r.total_events)))
        .collect();
    assert_eq!(rows["worker"], (0, 10));
    assert_eq!(rows["spin"], (10, 10));
}

/// With `include_offset`, `alpha+0x4` and `alpha+0x8` stay distinct, giving
/// finer-grained rows than `perf report` (which always groups by function).
#[test]
fn offset_option_keeps_instruction_detail() {
    let opts = parsers::Options { include_offset: true };
    let profile = parsers::load_with(&fixture("weighted.perf"), opts).unwrap();
    let app = profile.threads.iter().find(|t| t.tid == 1).unwrap();
    let keys: Vec<String> = flame::flat_profile(app).into_iter().map(|r| r.key).collect();

    assert!(keys.iter().any(|k| k == "alpha+0x4"));
    assert!(keys.iter().any(|k| k == "alpha+0x8"));
    assert!(keys.iter().any(|k| k == "main+0x10"));
    // The fused name must not appear when offsets are retained.
    assert!(!keys.iter().any(|k| k == "alpha"));
}

/// The left-heavy layout must conserve events: every row's bricks sum to no more
/// than the total, depth-1 bricks sum to exactly the total, and a brick's
/// children never overflow it.
#[test]
fn left_heavy_layout_conserves_events() {
    for name in ["weighted.perf", "consistency.perf", "mini.perf"] {
        let profile = parsers::load(&fixture(name)).unwrap();
        for thread in &profile.threads {
            let bricks = flame::left_heavy(thread);
            let events = thread.event_count();

            let depth1: u64 = bricks.iter().filter(|b| b.depth == 1).map(|b| b.samples).sum();
            assert_eq!(depth1, events, "{name}: roots cover all events");

            // No row exceeds the total width.
            let max_depth = bricks.iter().map(|b| b.depth).max().unwrap_or(0);
            for d in 1..=max_depth {
                let row: u64 = bricks.iter().filter(|b| b.depth == d).map(|b| b.samples).sum();
                assert!(row <= events, "{name}: depth {d} width {row} exceeds {events}");
            }

            // Each non-root brick is contained in exactly one parent brick.
            for child in bricks.iter().filter(|b| b.depth >= 2) {
                let parents = bricks
                    .iter()
                    .filter(|p| p.depth == child.depth - 1)
                    .filter(|p| {
                        p.start_ns <= child.start_ns + 1e-9
                            && p.start_ns + p.width_ns + 1e-9 >= child.start_ns + child.width_ns
                    })
                    .count();
                assert_eq!(parents, 1, "{name}: each brick has one parent");
            }
        }
    }
}
