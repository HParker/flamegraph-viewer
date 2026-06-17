//! Parser for the `perf data --json` / `perf report --json` style output (the
//! format used by `perf.json`).

use serde::Deserialize;
use std::error::Error;
use std::io::Read;

use crate::profile::{Frame, Profile, Sample, ThreadBuilder};

/// Only the fields the viewer needs are deserialized; everything else (headers,
/// version, ...) is ignored.
#[derive(Deserialize)]
struct PerfData {
    samples: Vec<PerfSample>,
}

#[derive(Deserialize)]
struct PerfSample {
    timestamp: f64,
    pid: u64,
    tid: u64,
    #[serde(default)]
    comm: Option<String>,
    callchain: Vec<PerfFrame>,
}

#[derive(Deserialize)]
struct PerfFrame {
    ip: String,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    dso: Option<String>,
}

/// Parse a perf JSON document into a [`Profile`].
pub fn parse_str(input: &str) -> Result<Profile, Box<dyn Error>> {
    build(serde_json::from_str(input)?)
}

/// Parse a perf JSON document from a reader, avoiding a whole-file string.
pub fn parse_reader<R: Read>(reader: R) -> Result<Profile, Box<dyn Error>> {
    build(serde_json::from_reader(reader)?)
}

fn build(data: PerfData) -> Result<Profile, Box<dyn Error>> {
    let mut builder = ThreadBuilder::default();
    for sample in data.samples {
        let stack = sample
            .callchain
            .into_iter()
            .map(|f| {
                builder.intern(Frame {
                    ip: f.ip,
                    symbol: f.symbol,
                    dso: f.dso,
                })
            })
            .collect();
        builder.push(
            sample.pid,
            sample.tid,
            sample.comm,
            Sample {
                // perf timestamps are already in nanoseconds.
                timestamp_ns: sample.timestamp,
                stack,
                weight: 1,
            },
        );
    }

    Ok(builder.finish())
}
