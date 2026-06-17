//! Common intermediate representation shared by every profile parser.
//!
//! Each supported on-disk format (perf JSON, `perf script` text and the Gecko /
//! Firefox profiler format) is converted into a [`Profile`]. The renderer only
//! ever sees this representation, so adding a new format means adding a new
//! parser and nothing else.

use std::collections::HashMap;
use std::sync::Arc;

/// A single stack frame.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Frame {
    /// Stable identifier for the frame: an instruction pointer when available,
    /// otherwise a symbolic identity. Two frames with the same `ip` are treated
    /// as the same frame when merging adjacent samples.
    pub ip: String,
    /// Human-readable symbol name, when known.
    pub symbol: Option<String>,
    /// Originating shared object or source file, when known.
    pub dso: Option<String>,
}

impl Frame {
    /// The frame's display/identity key: its symbol when present, otherwise its
    /// instruction pointer. Used for labelling, colouring and grouping bricks
    /// that represent the same function.
    pub fn key(&self) -> &str {
        match &self.symbol {
            Some(symbol) if !symbol.is_empty() => symbol,
            _ => &self.ip,
        }
    }
}

/// An interned reference to a [`Frame`] in a [`FrameTable`].
///
/// Samples store these 4-byte ids instead of owning their frames, which keeps
/// large profiles (millions of samples, tens of frames each) compact: the
/// expensive string fields of each distinct frame are stored exactly once.
pub type FrameId = u32;

/// Deduplicating store of [`Frame`]s, shared by every thread in a [`Profile`].
///
/// Real profiles contain a small number of distinct frames relative to the
/// number of sampled stack entries, so interning collapses that redundancy and
/// lets samples reference frames by a cheap [`FrameId`].
///
/// Frames are deduplicated by their identity [key](Frame::key) (function name
/// when known, otherwise instruction pointer). This groups every occurrence of a
/// function into one frame regardless of its instruction pointer or call offset,
/// which is exactly how `perf report` and the FlameGraph tools aggregate, so the
/// counts the viewer reports line up with those tools.
#[derive(Debug, Default)]
pub struct FrameTable {
    frames: Vec<Frame>,
    index: HashMap<String, FrameId>,
}

impl FrameTable {
    /// Return the id for `frame`, inserting it if its identity key has not been
    /// seen before.
    pub fn intern(&mut self, frame: Frame) -> FrameId {
        if let Some(&id) = self.index.get(frame.key()) {
            return id;
        }
        let id = self.frames.len() as FrameId;
        self.index.insert(frame.key().to_string(), id);
        self.frames.push(frame);
        id
    }

    /// Resolve an id back to its [`Frame`].
    pub fn get(&self, id: FrameId) -> &Frame {
        &self.frames[id as usize]
    }

    /// Number of distinct interned frames.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

/// A single profiler sample: a stack captured at a point in time.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Sample time in nanoseconds. The absolute value is arbitrary; only
    /// differences between samples are meaningful.
    pub timestamp_ns: f64,
    /// Stack frames ordered leaf-first: the innermost (currently executing)
    /// frame is at index 0 and the outermost (entry point) frame is last. Each
    /// entry is a [`FrameId`] into the owning [`Thread`]'s frame table.
    pub stack: Vec<FrameId>,
    /// How many profiling events this sample stands for: the `perf` *period*
    /// (e.g. the number of CPU cycles since the previous sample). `perf report`
    /// and the FlameGraph tools weight a stack by this value rather than
    /// counting it once, so the viewer does too. Formats that do not record a
    /// period use `1`, making the weighting collapse back to a plain count.
    pub weight: u64,
}

/// A single thread (or process) of execution and its time-ordered samples.
///
/// Threads are kept distinct even when their `tid` is unreliable: the Gecko
/// format, for example, reports `tid = 0` for every non-main thread and only
/// distinguishes them by `name`.
#[derive(Debug, Clone)]
pub struct Thread {
    pub pid: u64,
    pub tid: u64,
    pub name: Option<String>,
    /// Samples in time order.
    pub samples: Vec<Sample>,
    /// Frame table shared by every thread in the owning profile; resolves the
    /// [`FrameId`]s stored in each sample's stack.
    pub frames: Arc<FrameTable>,
}

impl Thread {
    /// Resolve one of this thread's [`FrameId`]s to its [`Frame`].
    pub fn frame(&self, id: FrameId) -> &Frame {
        self.frames.get(id)
    }

    /// The identity key of one of this thread's frames (see [`Frame::key`]).
    pub fn key(&self, id: FrameId) -> &str {
        self.frames.get(id).key()
    }

    /// A human-readable label for thread pickers, e.g. `"rg [64477/64477]"` or
    /// `"listen-run_thread [42886/0]"`.
    pub fn label(&self) -> String {
        match &self.name {
            Some(name) if !name.is_empty() => format!("{name} [{}/{}]", self.pid, self.tid),
            _ => format!("{}/{}", self.pid, self.tid),
        }
    }

    /// Deepest stack in this thread.
    pub fn max_depth(&self) -> usize {
        self.samples.iter().map(|s| s.stack.len()).max().unwrap_or(0)
    }

    /// Total profiling events this thread represents: the sum of its samples'
    /// [weights](Sample::weight). This is the denominator `perf report` uses for
    /// its overhead percentages.
    pub fn event_count(&self) -> u64 {
        self.samples.iter().map(|s| s.weight).sum()
    }

    /// Time from the first to the last sample, in nanoseconds.
    pub fn span_ns(&self) -> f64 {
        match (self.samples.first(), self.samples.last()) {
            (Some(first), Some(last)) => last.timestamp_ns - first.timestamp_ns,
            _ => 0.0,
        }
    }

    /// Absolute timestamp of this thread's first sample, or `None` when the
    /// thread has no samples. Useful for placing the thread on the profile-wide
    /// timeline (a thread may begin well after the profile starts).
    pub fn first_timestamp_ns(&self) -> Option<f64> {
        self.samples.first().map(|s| s.timestamp_ns)
    }

    /// Absolute timestamp just past this thread's last sample.
    pub fn last_timestamp_ns(&self) -> Option<f64> {
        self.samples.last().map(|s| s.timestamp_ns)
    }
}

/// A parsed profile: a set of threads, each with its own samples.
#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub threads: Vec<Thread>,
}

impl Profile {
    pub fn total_samples(&self) -> usize {
        self.threads.iter().map(|t| t.samples.len()).sum()
    }

    /// Total profiling events across every thread (sum of sample weights).
    pub fn event_count(&self) -> u64 {
        self.threads.iter().map(|t| t.event_count()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.total_samples() == 0
    }

    /// Absolute timestamp of the earliest sample across every thread: the start
    /// of the profile's timeline. `None` when the profile has no samples.
    pub fn start_ns(&self) -> Option<f64> {
        self.threads
            .iter()
            .filter_map(|t| t.first_timestamp_ns())
            .reduce(f64::min)
    }

    /// Absolute timestamp of the latest sample across every thread. `None` when
    /// the profile has no samples.
    pub fn end_ns(&self) -> Option<f64> {
        self.threads
            .iter()
            .filter_map(|t| t.last_timestamp_ns())
            .reduce(f64::max)
    }

    /// Wall-clock span covered by the whole profile, in nanoseconds.
    pub fn span_ns(&self) -> f64 {
        match (self.start_ns(), self.end_ns()) {
            (Some(start), Some(end)) => end - start,
            _ => 0.0,
        }
    }

    /// Thread indices ordered by descending sample count (ties broken by the
    /// thread's original order), so index 0 is the busiest thread.
    pub fn threads_by_samples(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.threads.len()).collect();
        order.sort_by(|&a, &b| {
            self.threads[b]
                .samples
                .len()
                .cmp(&self.threads[a].samples.len())
                .then(a.cmp(&b))
        });
        order
    }

    /// Index of the thread with the most samples, if any.
    pub fn busiest_thread(&self) -> Option<usize> {
        self.threads_by_samples().into_iter().next()
    }
}

/// Helper for parsers whose samples are a flat, interleaved stream tagged with
/// `(pid, tid)` (perf JSON and `perf script`). Groups them into [`Thread`]s
/// while preserving per-thread sample order and interning their frames into a
/// single shared [`FrameTable`].
#[derive(Default)]
pub struct ThreadBuilder {
    threads: Vec<Thread>,
    index: HashMap<(u64, u64), usize>,
    frames: FrameTable,
}

impl ThreadBuilder {
    /// Intern a frame, returning the id to store in a sample's stack.
    pub fn intern(&mut self, frame: Frame) -> FrameId {
        self.frames.intern(frame)
    }

    pub fn push(&mut self, pid: u64, tid: u64, name: Option<String>, sample: Sample) {
        let idx = match self.index.get(&(pid, tid)) {
            Some(&idx) => idx,
            None => {
                let idx = self.threads.len();
                self.threads.push(Thread {
                    pid,
                    tid,
                    name: name.clone(),
                    samples: Vec::new(),
                    frames: Arc::default(),
                });
                self.index.insert((pid, tid), idx);
                idx
            }
        };
        if self.threads[idx].name.is_none() {
            self.threads[idx].name = name;
        }
        self.threads[idx].samples.push(sample);
    }

    pub fn finish(self) -> Profile {
        let frames = Arc::new(self.frames);
        let threads = self
            .threads
            .into_iter()
            .map(|mut thread| {
                thread.frames = frames.clone();
                thread
            })
            .collect();
        Profile { threads }
    }
}
