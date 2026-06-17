//! Time-ordered flame-chart layout.
//!
//! Turns a [`Thread`]'s samples into a flat list of [`Brick`]s in a single pass.
//! Each row (`depth`) corresponds to a stack level (depth 1 is the outermost /
//! root frame) and the x axis is time. Consecutive samples that share the same
//! ancestry are merged into one wide brick, exactly like a flame chart.
//!
//! The algorithm keeps a stack of "open" bricks. For each sample it finds the
//! longest prefix (root-first) shared with the currently open bricks, closes the
//! bricks above that point and opens new ones for the rest of the stack. This is
//! `O(sum of stack depths)` with no per-depth rescans and no per-frame
//! allocations until a brick is finally emitted.

use crate::profile::{FrameId, Sample, Thread};
use std::collections::{HashMap, HashSet};

/// A single rectangle in the flame chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Brick {
    /// 1-based row; depth 1 is the root (outermost) frame.
    pub depth: usize,
    /// Start offset from the thread's first sample, in nanoseconds.
    pub start_ns: f64,
    /// Width in nanoseconds.
    pub width_ns: f64,
    /// Interned frame; resolve against the owning thread's frame table.
    pub frame: FrameId,
    /// Profiling events (period-weighted samples) that passed through this span
    /// while it was open. This is the count-based weight `perf` reports,
    /// independent of sampling jitter in the time axis.
    pub samples: u64,
}

/// The depth-`d` (0-based, root-first) frame of a leaf-first stack.
fn frame_at(sample: &Sample, depth0: usize) -> FrameId {
    sample.stack[sample.stack.len() - 1 - depth0]
}

/// A brick that has been started but not yet closed.
struct Open {
    /// Index of the sample that opened this brick (its frame is emitted on close).
    opened_by: usize,
    start_ns: f64,
    /// Events seen so far that pass through this span (including the opener).
    samples: u64,
}

/// Build the flame-chart bricks for a single thread.
///
/// Bricks narrower than `min_width_ns` are not emitted. The renderer passes the
/// width of one pixel at its fit scale here so that, for very large profiles,
/// the transient brick list is bounded by what can actually be drawn rather than
/// by the (potentially enormous) number of sampled stack entries. Pass `0.0` to
/// emit every brick.
pub fn layout(thread: &Thread, min_width_ns: f64) -> Vec<Brick> {
    let samples = &thread.samples;
    let mut bricks = Vec::new();
    let Some(first) = samples.first() else {
        return bricks;
    };
    let first_ts = first.timestamp_ns;

    let mut open: Vec<Open> = Vec::new();

    for (i, sample) in samples.iter().enumerate() {
        let t = sample.timestamp_ns - first_ts;
        let len = sample.stack.len();

        // Longest root-first prefix shared with the currently open bricks.
        let mut common = 0;
        while common < open.len() && common < len {
            let open_frame = frame_at(&samples[open[common].opened_by], common);
            if open_frame == frame_at(sample, common) {
                common += 1;
            } else {
                break;
            }
        }

        // This sample passes through every surviving open brick above the
        // divergence point, so credit its weight to each of them.
        for o in &mut open[..common] {
            o.samples += sample.weight;
        }

        // Close every brick deeper than the shared prefix; it ends at `t`.
        close_from(&mut bricks, &mut open, samples, common, t, min_width_ns);

        // Open bricks for the remainder of this sample's stack.
        for depth0 in common..len {
            open.push(Open {
                opened_by: i,
                start_ns: t,
                samples: sample.weight,
            });
            let _ = depth0;
        }
    }

    // Close everything still open at the end of the thread's last sample.
    let end = thread_end(samples).map(|e| e - first_ts).unwrap_or(0.0);
    close_from(&mut bricks, &mut open, samples, 0, end, min_width_ns);

    bricks
}

/// Close all open bricks with depth >= `keep`, emitting them ending at `end_ns`.
fn close_from(
    bricks: &mut Vec<Brick>,
    open: &mut Vec<Open>,
    samples: &[Sample],
    keep: usize,
    end_ns: f64,
    min_width_ns: f64,
) {
    for depth0 in (keep..open.len()).rev() {
        let o = &open[depth0];
        let width = end_ns - o.start_ns;
        if width > min_width_ns {
            bricks.push(Brick {
                depth: depth0 + 1,
                start_ns: o.start_ns,
                width_ns: width,
                frame: frame_at(&samples[o.opened_by], depth0),
                samples: o.samples,
            });
        }
    }
    open.truncate(keep);
}

/// End time of the thread: the last sample's start plus its own weight (the gap
/// to the previous sample, since there is no following sample to measure).
fn thread_end(samples: &[Sample]) -> Option<f64> {
    match samples {
        [] => None,
        [only] => Some(only.timestamp_ns),
        [.., prev, last] => Some(last.timestamp_ns + (last.timestamp_ns - prev.timestamp_ns)),
    }
}

/// Inclusive (`total`) and exclusive (`self`) time for one frame identity.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    /// Time the frame was anywhere on the stack (counted once per sample).
    pub total_ns: f64,
    /// Time the frame was the leaf (top of stack).
    pub self_ns: f64,
}

/// Per-sample weight: the time until the next sample. The final sample reuses
/// the previous gap, matching [`thread_end`].
fn sample_weights(samples: &[Sample]) -> Vec<f64> {
    let n = samples.len();
    if n == 0 {
        return Vec::new();
    }
    let mut weights = Vec::with_capacity(n);
    for i in 0..n {
        let w = if i + 1 < n {
            samples[i + 1].timestamp_ns - samples[i].timestamp_ns
        } else if n >= 2 {
            samples[i].timestamp_ns - samples[i - 1].timestamp_ns
        } else {
            0.0
        };
        weights.push(w);
    }
    weights
}

/// Aggregate inclusive/exclusive time per frame identity ([`Frame::key`]) for a
/// thread. Inclusive time counts a sample once even when a frame recurses, and
/// exclusive time is credited to the leaf frame only.
pub fn symbol_stats(thread: &Thread) -> HashMap<String, Stats> {
    let samples = &thread.samples;
    let weights = sample_weights(samples);
    let mut stats: HashMap<String, Stats> = HashMap::new();
    let mut seen: HashSet<&str> = HashSet::new();

    for (sample, &weight) in samples.iter().zip(&weights) {
        if let Some(&leaf) = sample.stack.first() {
            stats.entry(thread.key(leaf).to_string()).or_default().self_ns += weight;
        }
        seen.clear();
        for &id in &sample.stack {
            let key = thread.key(id);
            if seen.insert(key) {
                stats.entry(key.to_string()).or_default().total_ns += weight;
            }
        }
    }

    stats
}

/// A node in the top-down call tree, aggregating samples by call path (root
/// first) the way `perf report -g` does: every occurrence of the same path is
/// merged regardless of when in time it was sampled.
#[derive(Debug, Clone, PartialEq)]
pub struct CallNode {
    /// Interned frame; resolve against the owning thread's frame table.
    pub frame: FrameId,
    /// Events whose stack passes through this node (its inclusive weight).
    pub total_samples: u64,
    /// Events for which this node is the leaf (its exclusive weight).
    pub self_samples: u64,
    /// Child call paths, ordered by descending event weight.
    pub children: Vec<CallNode>,
}

/// The aggregated call tree for a single thread.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallTree {
    /// Distinct root frames (a thread may have several).
    pub roots: Vec<CallNode>,
    /// Total events in the thread: the denominator for percentages.
    pub total_samples: u64,
}

/// Mutable tree node used only while building, with O(1) child lookup.
struct BuildNode {
    frame: FrameId,
    total: u64,
    self_count: u64,
    children: HashMap<FrameId, usize>,
}

/// Aggregate a thread's samples into a top-down [`CallTree`] of inclusive and
/// exclusive **event counts** (period-weighted samples, the count-based
/// weighting `perf report` uses, as opposed to the time-based weighting used by
/// [`symbol_stats`] and the flame chart).
pub fn call_tree(thread: &Thread) -> CallTree {
    let mut arena: Vec<BuildNode> = Vec::new();
    let mut roots: HashMap<FrameId, usize> = HashMap::new();
    let mut total_samples = 0u64;

    for sample in &thread.samples {
        let n = sample.stack.len();
        if n == 0 {
            continue;
        }
        let weight = sample.weight;
        total_samples += weight;

        let mut parent: Option<usize> = None;
        for k in 0..n {
            // Walk root-first: the root is the last element of a leaf-first stack.
            let id = sample.stack[n - 1 - k];
            let idx = match parent {
                None => find_or_insert(&mut arena, &mut roots, id),
                Some(p) => {
                    if let Some(&i) = arena[p].children.get(&id) {
                        i
                    } else {
                        let i = push_node(&mut arena, id);
                        arena[p].children.insert(id, i);
                        i
                    }
                }
            };
            arena[idx].total += weight;
            if k == n - 1 {
                arena[idx].self_count += weight;
            }
            parent = Some(idx);
        }
    }

    let mut tree = CallTree {
        roots: finish_children(&arena, roots.values().copied().collect()),
        total_samples,
    };
    sort_nodes(&mut tree.roots);
    tree
}

fn push_node(arena: &mut Vec<BuildNode>, frame: FrameId) -> usize {
    arena.push(BuildNode {
        frame,
        total: 0,
        self_count: 0,
        children: HashMap::new(),
    });
    arena.len() - 1
}
fn find_or_insert(
    arena: &mut Vec<BuildNode>,
    map: &mut HashMap<FrameId, usize>,
    frame: FrameId,
) -> usize {
    if let Some(&i) = map.get(&frame) {
        return i;
    }
    let i = push_node(arena, frame);
    map.insert(frame, i);
    i
}

/// Recursively convert build nodes (referenced by arena index) into the public,
/// owned [`CallNode`] form, sorted by descending sample count.
fn finish_children(arena: &[BuildNode], indices: Vec<usize>) -> Vec<CallNode> {
    let mut nodes: Vec<CallNode> = indices
        .into_iter()
        .map(|i| {
            let node = &arena[i];
            let mut children =
                finish_children(arena, node.children.values().copied().collect());
            sort_nodes(&mut children);
            CallNode {
                frame: node.frame,
                total_samples: node.total,
                self_samples: node.self_count,
                children,
            }
        })
        .collect();
    sort_nodes(&mut nodes);
    nodes
}

/// Order siblings by descending inclusive count, breaking ties by frame id so
/// the result is deterministic.
fn sort_nodes(nodes: &mut [CallNode]) {
    nodes.sort_by(|a, b| {
        b.total_samples
            .cmp(&a.total_samples)
            .then(a.frame.cmp(&b.frame))
    });
}

/// Lay out a thread as a **left-heavy flame graph** (the classic icicle view, as
/// produced by FlameGraph / inferno): identical stacks are merged regardless of
/// when they were sampled, and siblings are ordered widest-first from the left.
///
/// Unlike [`layout`], whose x axis is time, here the x axis is *events*: a
/// brick's `start_ns`/`width_ns` are event-weighted positions (same unit as
/// `samples`), so the renderer scales them just like the time axis. The widest
/// stacks collect on the left, which is what makes hot paths jump out.
pub fn left_heavy(thread: &Thread) -> Vec<Brick> {
    let tree = call_tree(thread);
    let mut bricks = Vec::new();
    place_left_heavy(&tree.roots, 1, 0.0, &mut bricks);
    bricks
}

/// Place a row of already-sorted siblings left to right, recursing into each.
fn place_left_heavy(nodes: &[CallNode], depth: usize, mut x: f64, out: &mut Vec<Brick>) {
    for node in nodes {
        let width = node.total_samples as f64;
        // A child's events are a subset of its parent's, so it is left-aligned
        // with the parent and always fits inside it.
        place_left_heavy(&node.children, depth + 1, x, out);
        out.push(Brick {
            depth,
            start_ns: x,
            width_ns: width,
            frame: node.frame,
            samples: node.total_samples,
        });
        x += width;
    }
}

/// One row of the flat, `perf report`-style function table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FunctionStat {
    /// Function identity ([`Frame::key`]).
    pub key: String,
    /// Events for which this function was the leaf (its exclusive weight).
    pub self_events: u64,
    /// Events whose stack included this function, counted once per sample even
    /// when it recurses (its inclusive weight).
    pub total_events: u64,
    /// Exclusive time in nanoseconds (time-weighted, flame-chart units).
    pub self_ns: f64,
    /// Inclusive time in nanoseconds.
    pub total_ns: f64,
}

/// Aggregate a thread into one row per function identity, the way `perf report`
/// presents its flat profile. Rows are returned sorted by descending self
/// events (the hottest leaves first), which is what points at the code worth
/// optimising. Both event-weighted counts (matching `perf`/inferno) and
/// time-weighted nanoseconds are reported.
pub fn flat_profile(thread: &Thread) -> Vec<FunctionStat> {
    let weights = sample_weights(&thread.samples);
    let mut by_key: HashMap<&str, FunctionStat> = HashMap::new();
    let mut seen: HashSet<&str> = HashSet::new();

    for (sample, &time) in thread.samples.iter().zip(&weights) {
        let events = sample.weight;
        if let Some(&leaf) = sample.stack.first() {
            let entry = by_key.entry(thread.key(leaf)).or_default();
            entry.self_events += events;
            entry.self_ns += time;
        }
        seen.clear();
        for &id in &sample.stack {
            let key = thread.key(id);
            if seen.insert(key) {
                let entry = by_key.entry(key).or_default();
                entry.total_events += events;
                entry.total_ns += time;
            }
        }
    }

    let mut rows: Vec<FunctionStat> = by_key
        .into_iter()
        .map(|(key, mut stat)| {
            stat.key = key.to_string();
            stat
        })
        .collect();
    rows.sort_by(|a, b| {
        b.self_events
            .cmp(&a.self_events)
            .then(b.total_events.cmp(&a.total_events))
            .then_with(|| a.key.cmp(&b.key))
    });
    rows
}
