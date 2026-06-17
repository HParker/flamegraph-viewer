//! Integration tests covering all three profile parsers, format detection and
//! the flame-chart layout, using the small fixtures in `tests/fixtures/`.

use std::path::{Path, PathBuf};

use flamegraph_viewer::flame;
use flamegraph_viewer::parsers::{self, Format};
use flamegraph_viewer::profile::{Profile, Thread};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn load(name: &str) -> Profile {
    parsers::load(&fixture(name)).expect("fixture should parse")
}

/// The busiest thread of a profile.
fn busiest(profile: &Profile) -> &Thread {
    &profile.threads[profile.busiest_thread().expect("a thread")]
}

/// Every fixture encodes the same scenario, so the busiest thread always looks
/// the same regardless of the source format.
fn assert_busiest_scenario(thread: &Thread) {
    assert_eq!(thread.samples.len(), 3, "busiest thread has three samples");

    // Stacks are leaf-first: the leaf alternates, the root is shared.
    let leaves: Vec<&str> = thread
        .samples
        .iter()
        .map(|s| thread.key(*s.stack.first().unwrap()))
        .collect();
    assert_eq!(leaves, ["leaf_a", "leaf_b", "leaf_a"]);

    for sample in &thread.samples {
        assert!(!sample.stack.is_empty());
        assert_eq!(thread.key(*sample.stack.last().unwrap()), "root");
    }
}

#[test]
fn perf_json_parses() {
    let profile = load("mini.perf.json");
    assert_eq!(profile.threads.len(), 2, "two distinct threads");
    assert_eq!(profile.total_samples(), 4);
    assert_busiest_scenario(busiest(&profile));
}

#[test]
fn perf_script_parses() {
    let profile = load("mini.perf");
    assert_eq!(profile.threads.len(), 2, "two distinct threads");
    assert_eq!(profile.total_samples(), 4);
    assert_busiest_scenario(busiest(&profile));
}

#[test]
fn gecko_parses() {
    let profile = load("mini.gecko.json");
    assert_eq!(profile.threads.len(), 1);
    assert_eq!(profile.total_samples(), 3);
    let thread = busiest(&profile);
    assert_eq!(thread.name.as_deref(), Some("app"));
    assert_busiest_scenario(thread);
}

#[test]
fn all_formats_agree_on_timing() {
    // The three fixtures describe the same samples at 1000/2000/3000 ns, so the
    // busiest thread's span is identical across formats.
    for name in ["mini.perf.json", "mini.perf", "mini.gecko.json"] {
        let profile = load(name);
        let span = busiest(&profile).span_ns();
        assert!((span - 2000.0).abs() < 1.0, "span for {name} was {span}");
    }
}

#[test]
fn format_detection() {
    let json_head = std::fs::read_to_string(fixture("mini.perf.json")).unwrap();
    let gecko_head = std::fs::read_to_string(fixture("mini.gecko.json")).unwrap();
    let script_head = std::fs::read_to_string(fixture("mini.perf")).unwrap();

    assert_eq!(
        parsers::detect_format(Path::new("a.json"), &json_head),
        Some(Format::PerfJson)
    );
    assert_eq!(
        parsers::detect_format(Path::new("a.json"), &gecko_head),
        Some(Format::Gecko)
    );
    // The `.perf` extension forces perf-script regardless of contents.
    assert_eq!(
        parsers::detect_format(Path::new("a.perf"), &script_head),
        Some(Format::PerfScript)
    );
    // Non-JSON text without the extension still detects as perf-script.
    assert_eq!(
        parsers::detect_format(Path::new("noext"), &script_head),
        Some(Format::PerfScript)
    );
}

#[test]
fn layout_bottom_row_is_a_single_root_brick() {
    let profile = load("mini.perf.json");
    let thread = busiest(&profile);
    let bricks = flame::layout(thread, 0.0);

    let root_row: Vec<_> = bricks.iter().filter(|b| b.depth == 1).collect();
    assert_eq!(root_row.len(), 1, "root is one continuous brick");

    let root = root_row[0];
    assert_eq!(thread.key(root.frame), "root");
    assert_eq!(root.start_ns, 0.0);
    // Span is 2000 ns plus the final sample's own weight (1000 ns).
    assert_eq!(root.width_ns, 3000.0);

    // The second row should contain the alternating leaves.
    let leaves: Vec<&str> = bricks
        .iter()
        .filter(|b| b.depth == 2)
        .map(|b| thread.key(b.frame))
        .collect();
    assert_eq!(leaves, ["leaf_a", "leaf_b", "leaf_a"]);
}

#[test]
fn symbol_stats_split_self_and_total() {
    let profile = load("mini.perf.json");
    let thread = busiest(&profile);
    let stats = flame::symbol_stats(thread);

    // root is on every stack but never the leaf: all total, no self.
    let root = stats["root"];
    assert_eq!(root.total_ns, 3000.0);
    assert_eq!(root.self_ns, 0.0);

    // leaf_a is the leaf of two samples; leaf_b of one.
    assert_eq!(stats["leaf_a"].self_ns, 2000.0);
    assert_eq!(stats["leaf_a"].total_ns, 2000.0);
    assert_eq!(stats["leaf_b"].self_ns, 1000.0);
}
