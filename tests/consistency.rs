//! Cross-validation tests: every span's sample count, percentage and
//! parent/child hierarchy must agree with an *independent* reading of the same
//! profile (the numbers `perf` itself reports).
//!
//! The oracle in this file re-parses `perf script` text and re-aggregates it
//! from scratch, sharing no code with `src/`. If our parser, frame interning,
//! call-tree aggregation or flame layout miscounts or mis-nests anything, the
//! oracle disagrees and the test fails.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use flamegraph_viewer::flame::{self, CallNode};
use flamegraph_viewer::parsers;
use flamegraph_viewer::profile::{Profile, Thread};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn thread(profile: &Profile, pid: u64, tid: u64) -> &Thread {
    profile
        .threads
        .iter()
        .find(|t| t.pid == pid && t.tid == tid)
        .expect("thread present")
}

// --------------------------------------------------------------------------
// Independent `perf script` oracle (shares no code with the crate under test).
// --------------------------------------------------------------------------

/// Root-first call paths, grouped by `(pid, tid)`.
type Oracle = BTreeMap<(u64, u64), Vec<Vec<String>>>;

fn oracle(text: &str) -> Oracle {
    let mut threads: Oracle = BTreeMap::new();
    // `(pid, tid)` plus a leaf-first stack of frame keys.
    let mut current: Option<((u64, u64), Vec<String>)> = None;

    let flush = |threads: &mut Oracle, current: &mut Option<((u64, u64), Vec<String>)>| {
        if let Some((tt, mut leaf_first)) = current.take()
            && !leaf_first.is_empty()
        {
            leaf_first.reverse(); // -> root first
            threads.entry(tt).or_default().push(leaf_first);
        }
    };

    for line in text.lines() {
        if line.trim().is_empty() {
            flush(&mut threads, &mut current);
        } else if line.starts_with(char::is_whitespace) {
            if let (Some((_, stack)), Some(key)) = (current.as_mut(), frame_key(line)) {
                stack.push(key);
            }
        } else {
            flush(&mut threads, &mut current);
            current = parse_pid_tid(line).map(|tt| (tt, Vec::new()));
        }
    }
    flush(&mut threads, &mut current);
    threads
}

/// Frame identity key, mirroring `Frame::key` (symbol when present, else ip).
fn frame_key(line: &str) -> Option<String> {
    let line = line.trim();
    let (addr, rest) = line.split_once(char::is_whitespace)?;
    let symbol = match rest.rsplit_once(" (") {
        Some((sym, _dso)) => sym.trim(),
        None => rest.trim(),
    };
    Some(match clean(symbol) {
        Some(symbol) => symbol,
        None => format!("0x{}", addr.trim_start_matches("0x")),
    })
}

fn clean(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value != "[unknown]").then(|| value.to_string())
}

fn parse_pid_tid(line: &str) -> Option<(u64, u64)> {
    line.split_whitespace().find_map(|token| {
        let (pid, tid) = token.split_once('/')?;
        Some((pid.parse().ok()?, tid.parse().ok()?))
    })
}

/// Aggregate the oracle's stacks into `root-first path -> (total, self)` counts,
/// exactly the quantity each call-tree node represents.
fn oracle_paths(stacks: &[Vec<String>]) -> BTreeMap<Vec<String>, (u64, u64)> {
    let mut paths: BTreeMap<Vec<String>, (u64, u64)> = BTreeMap::new();
    for stack in stacks {
        for depth in 0..stack.len() {
            let entry = paths.entry(stack[0..=depth].to_vec()).or_default();
            entry.0 += 1;
            if depth == stack.len() - 1 {
                entry.1 += 1;
            }
        }
    }
    paths
}

/// Flatten our [`flame::CallTree`] into the same `path -> (total, self)` map.
fn tree_paths(thread: &Thread) -> BTreeMap<Vec<String>, (u64, u64)> {
    fn walk(
        thread: &Thread,
        nodes: &[CallNode],
        prefix: &mut Vec<String>,
        out: &mut BTreeMap<Vec<String>, (u64, u64)>,
    ) {
        for node in nodes {
            prefix.push(thread.key(node.frame).to_string());
            out.insert(prefix.clone(), (node.total_samples, node.self_samples));
            walk(thread, &node.children, prefix, out);
            prefix.pop();
        }
    }
    let tree = flame::call_tree(thread);
    let mut out = BTreeMap::new();
    walk(thread, &tree.roots, &mut Vec::new(), &mut out);
    out
}

// --------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------

#[test]
fn call_tree_matches_independent_oracle() {
    let text = std::fs::read_to_string(fixture("consistency.perf")).unwrap();
    let oracle = oracle(&text);
    let profile = parsers::load(&fixture("consistency.perf")).unwrap();

    // Same set of threads.
    let our_threads: Vec<(u64, u64)> = profile.threads.iter().map(|t| (t.pid, t.tid)).collect();
    let oracle_threads: Vec<(u64, u64)> = oracle.keys().copied().collect();
    assert_eq!(
        our_threads.iter().copied().collect::<std::collections::BTreeSet<_>>(),
        oracle_threads.iter().copied().collect::<std::collections::BTreeSet<_>>(),
    );

    for (&(pid, tid), stacks) in &oracle {
        let thread = thread(&profile, pid, tid);

        // Total sample count per thread.
        assert_eq!(
            thread.samples.len(),
            stacks.len(),
            "sample count mismatch for {pid}/{tid}"
        );
        // The fixture uses a period of 1, so events and samples coincide.
        assert_eq!(flame::call_tree(thread).total_samples, thread.event_count());
        assert_eq!(thread.event_count() as usize, stacks.len());

        // Every call path's inclusive/exclusive sample count matches perf's.
        assert_eq!(
            tree_paths(thread),
            oracle_paths(stacks),
            "call-tree counts/hierarchy mismatch for {pid}/{tid}"
        );
    }
}

#[test]
fn known_counts_in_consistency_fixture() {
    let profile = parsers::load(&fixture("consistency.perf")).unwrap();
    let app = thread(&profile, 100, 100);
    let paths = tree_paths(app);

    let p = |keys: &[&str]| keys.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    // main is the root of all 7 app samples and is never itself the leaf.
    assert_eq!(paths[&p(&["main"])], (7, 0));
    // alpha appears in 3 samples; it recurses once (depth-3 alpha) and that
    // inner frame is the leaf of 2 samples.
    assert_eq!(paths[&p(&["main", "alpha"])], (3, 0));
    assert_eq!(paths[&p(&["main", "alpha", "alpha"])], (2, 2));
    assert_eq!(paths[&p(&["main", "alpha", "beta"])], (1, 1));
    // gamma is the leaf of 3 samples.
    assert_eq!(paths[&p(&["main", "gamma"])], (3, 3));
    // The unknown leaf is keyed by its instruction pointer.
    assert_eq!(paths[&p(&["main", "0x00000000000000a0"])], (1, 1));
}

#[test]
fn percentages_account_for_all_samples() {
    for name in [
        "consistency.perf",
        "mini.perf",
        "mini.perf.json",
        "mini.gecko.json",
    ] {
        let profile = parsers::load(&fixture(name)).unwrap();
        for thread in &profile.threads {
            let tree = flame::call_tree(thread);
            assert_call_tree_conserves(&tree.roots, tree.total_samples);

            // Root percentages and self (leaf) percentages each cover 100%.
            let total = tree.total_samples as f64;
            let root_pct: f64 = tree.roots.iter().map(|n| n.total_samples as f64).sum::<f64>()
                / total
                * 100.0;
            assert!((root_pct - 100.0).abs() < 1e-9, "{name}: roots cover {root_pct}%");

            let self_pct = total_self(&tree.roots) as f64 / total * 100.0;
            assert!((self_pct - 100.0).abs() < 1e-9, "{name}: self covers {self_pct}%");
        }
    }
}

/// For every node: own leaf samples plus all child inclusive samples equals the
/// node's own inclusive samples, and the roots cover every sample exactly once.
fn assert_call_tree_conserves(roots: &[CallNode], total: u64) {
    fn check(node: &CallNode) {
        let children: u64 = node.children.iter().map(|c| c.total_samples).sum();
        assert_eq!(
            node.self_samples + children,
            node.total_samples,
            "node conservation violated"
        );
        node.children.iter().for_each(check);
    }
    roots.iter().for_each(check);
    let root_total: u64 = roots.iter().map(|n| n.total_samples).sum();
    assert_eq!(root_total, total, "roots must cover every sample");
}

fn total_self(nodes: &[CallNode]) -> u64 {
    nodes
        .iter()
        .map(|n| n.self_samples + total_self(&n.children))
        .sum()
}

#[test]
fn brick_sample_counts_and_nesting_match_stacks() {
    let profile = parsers::load(&fixture("consistency.perf")).unwrap();

    for thread in &profile.threads {
        let bricks = flame::layout(thread, 0.0);

        // Each sample is credited to exactly one brick per stack level, so the
        // bricks at depth `d` together account for every sample at least `d`
        // deep.
        let max_depth = thread.max_depth();
        for d in 1..=max_depth {
            let span_samples: u64 = bricks
                .iter()
                .filter(|b| b.depth == d)
                .map(|b| b.samples)
                .sum();
            let deep_enough = thread.samples.iter().filter(|s| s.stack.len() >= d).count() as u64;
            assert_eq!(span_samples, deep_enough, "depth {d} sample accounting");
        }

        // Every non-root brick sits inside exactly one parent brick (one row up)
        // whose time range contains it: the flame pyramid is well formed.
        for child in bricks.iter().filter(|b| b.depth >= 2) {
            let parents = bricks
                .iter()
                .filter(|p| p.depth == child.depth - 1)
                .filter(|p| {
                    p.start_ns <= child.start_ns + 1e-6
                        && p.start_ns + p.width_ns + 1e-6 >= child.start_ns + child.width_ns
                })
                .count();
            assert_eq!(parents, 1, "each span has exactly one parent span");
        }
    }
}

/// When the real `perf script` and `perf data --json` captures are present (they
/// are large and git-ignored), the two formats describe the same run, so our
/// parser must agree with itself on every thread's sample count, and every
/// call tree must conserve samples.
#[test]
fn real_perf_formats_agree_when_present() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = root.join("out.perf");
    let json = root.join("perf.json");
    if !script.exists() || !json.exists() {
        eprintln!("skipping: out.perf / perf.json not present");
        return;
    }

    let from_script = parsers::load(&script).unwrap();
    let from_json = parsers::load(&json).unwrap();

    let counts = |p: &Profile| {
        p.threads
            .iter()
            .map(|t| ((t.pid, t.tid), t.samples.len()))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(
        counts(&from_script),
        counts(&from_json),
        "the two perf formats of the same capture must agree on per-thread sample counts"
    );

    for profile in [&from_script, &from_json] {
        for thread in &profile.threads {
            let tree = flame::call_tree(thread);
            assert_call_tree_conserves(&tree.roots, tree.total_samples);
            assert_eq!(tree.total_samples, thread.event_count());
        }
    }
}
