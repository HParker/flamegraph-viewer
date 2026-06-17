//! Parser for the Gecko / Firefox profiler format (the "processed profile"
//! shape emitted by tools such as [Vernier](https://github.com/jhawthorn/vernier)).
//!
//! The format stores each thread's data in parallel "tables" (struct-of-arrays).
//! A sample points at a node in `stackTable`; each stack node references a frame
//! and its parent (`prefix`). Walking `prefix` links from a sample to the root
//! yields the call stack leaf-first:
//!
//! ```text
//! samples.stack[i] -> stackTable -> frameTable -> funcTable -> stringArray
//! ```

use serde::Deserialize;
use serde::de::{Deserializer, Error as _};
use std::error::Error;
use std::io::Read;
use std::sync::Arc;

use crate::profile::{Frame, FrameId, FrameTable, Profile, Sample, Thread};

pub fn parse_str(input: &str) -> Result<Profile, Box<dyn Error>> {
    let gecko: GeckoProfile = serde_json::from_str(input)?;
    Ok(gecko.into_profile())
}

pub fn parse_reader<R: Read>(reader: R) -> Result<Profile, Box<dyn Error>> {
    let gecko: GeckoProfile = serde_json::from_reader(reader)?;
    Ok(gecko.into_profile())
}

#[derive(Deserialize)]
struct GeckoProfile {
    #[serde(default)]
    meta: GeckoMeta,
    threads: Vec<GeckoThread>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GeckoMeta {
    #[serde(default)]
    sample_units: Option<SampleUnits>,
}

#[derive(Deserialize)]
struct SampleUnits {
    #[serde(default)]
    time: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeckoThread {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, deserialize_with = "de_u64_flexible")]
    pid: u64,
    #[serde(default, deserialize_with = "de_u64_flexible")]
    tid: u64,
    frame_table: GeckoFrameTable,
    func_table: FuncTable,
    stack_table: StackTable,
    samples: Samples,
    string_array: Vec<String>,
}

#[derive(Deserialize)]
struct GeckoFrameTable {
    address: Vec<i64>,
    func: Vec<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FuncTable {
    name: Vec<usize>,
    file_name: Vec<i64>,
}

#[derive(Deserialize)]
struct StackTable {
    frame: Vec<usize>,
    prefix: Vec<Option<usize>>,
}

#[derive(Deserialize)]
struct Samples {
    stack: Vec<Option<usize>>,
    time: Vec<f64>,
}

impl GeckoProfile {
    fn into_profile(self) -> Profile {
        let to_ns = self.meta.time_multiplier();
        let mut frames = FrameTable::default();
        let mut threads: Vec<Thread> = self
            .threads
            .iter()
            .map(|thread| thread.to_thread(to_ns, &mut frames))
            .collect();
        let frames = Arc::new(frames);
        for thread in &mut threads {
            thread.frames = frames.clone();
        }
        Profile { threads }
    }
}

impl GeckoMeta {
    /// Multiplier converting this profile's sample-time unit into nanoseconds.
    fn time_multiplier(&self) -> f64 {
        let unit = self
            .sample_units
            .as_ref()
            .and_then(|u| u.time.as_deref())
            .unwrap_or("ms");
        match unit {
            "ns" => 1.0,
            "us" | "µs" => 1_000.0,
            "s" => 1_000_000_000.0,
            // "ms" and anything unexpected default to milliseconds.
            _ => 1_000_000.0,
        }
    }
}

impl GeckoThread {
    fn to_thread(&self, to_ns: f64, frames: &mut FrameTable) -> Thread {
        // Memo from this thread's frame index to its interned id, so a frame
        // shared by many stack nodes is built (and its strings cloned) once.
        let mut frame_ids: Vec<Option<FrameId>> = vec![None; self.frame_table.func.len()];
        let samples = self
            .samples
            .time
            .iter()
            .enumerate()
            .map(|(i, &time)| {
                let stack_index = self.samples.stack.get(i).copied().flatten();
                Sample {
                    timestamp_ns: time * to_ns,
                    stack: self.build_stack(stack_index, frames, &mut frame_ids),
                    weight: 1,
                }
            })
            .collect();

        Thread {
            pid: self.pid,
            tid: self.tid,
            name: self.name.clone().filter(|n| !n.is_empty()),
            samples,
            frames: Arc::default(),
        }
    }

    /// Walk `prefix` links from `start` to the root, producing a leaf-first
    /// stack of interned [`FrameId`]s.
    ///
    /// A well-formed `stackTable` always references an earlier-inserted node as
    /// its prefix, so the index strictly decreases on every step. The guard
    /// below enforces that invariant, which both terminates the walk and makes a
    /// corrupt (cyclic) table safe instead of looping forever.
    fn build_stack(
        &self,
        start: Option<usize>,
        frames: &mut FrameTable,
        frame_ids: &mut [Option<FrameId>],
    ) -> Vec<FrameId> {
        let mut stack = Vec::new();
        let mut index = start;
        while let Some(node) = index {
            let Some(&frame_index) = self.stack_table.frame.get(node) else {
                break;
            };
            let id = match frame_ids.get(frame_index).copied().flatten() {
                Some(id) => id,
                None => {
                    let id = frames.intern(self.frame_at(frame_index));
                    if let Some(slot) = frame_ids.get_mut(frame_index) {
                        *slot = Some(id);
                    }
                    id
                }
            };
            stack.push(id);
            index = match self.stack_table.prefix.get(node).copied().flatten() {
                Some(prefix) if prefix < node => Some(prefix),
                _ => None,
            };
        }
        stack
    }

    fn frame_at(&self, frame_index: usize) -> Frame {
        let func_index = self.frame_table.func.get(frame_index).copied().unwrap_or(0);
        let name = self
            .func_table
            .name
            .get(func_index)
            .and_then(|&ni| self.string_array.get(ni))
            .cloned()
            .unwrap_or_default();

        let address = self.frame_table.address.get(frame_index).copied().unwrap_or(-1);
        let dso = self
            .func_table
            .file_name
            .get(func_index)
            .copied()
            .filter(|&fi| fi >= 0)
            .and_then(|fi| self.string_array.get(fi as usize))
            .cloned();

        // Most Gecko frames have no native address (-1); fall back to the symbol
        // name so the frame still has a stable identity.
        let ip = if address >= 0 {
            format!("0x{:x}", address)
        } else {
            name.clone()
        };

        Frame {
            ip,
            symbol: (!name.is_empty()).then_some(name),
            dso,
        }
    }
}

/// Accept a `u64` encoded as either a JSON number or string (Gecko is
/// inconsistent about how it encodes pids/tids).
fn de_u64_flexible<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Number(n) => n
            .as_u64()
            .ok_or_else(|| D::Error::custom("pid/tid is not a u64")),
        serde_json::Value::String(s) => s.parse().map_err(D::Error::custom),
        serde_json::Value::Null => Ok(0),
        _ => Err(D::Error::custom("pid/tid has unexpected type")),
    }
}
