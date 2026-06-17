//! Profile format parsers and format detection.
//!
//! Every parser converts an on-disk profile into the common
//! [`crate::profile::Profile`] representation.

pub mod gecko;
pub mod perf_json;
pub mod perf_script;

use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::profile::Profile;

/// A supported profile format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `perf data --json` style output (e.g. `perf.json`).
    PerfJson,
    /// Text output of `perf script` (also importable by speedscope).
    PerfScript,
    /// Gecko / Firefox profiler "processed profile" (e.g. Vernier output).
    Gecko,
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Format::PerfJson => "perf json",
            Format::PerfScript => "perf script",
            Format::Gecko => "gecko",
        };
        f.write_str(name)
    }
}

/// Options controlling how a profile is parsed.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Keep the `+0x<offset>` suffix that `perf script` prints after a symbol
    /// name. By default it is stripped so that every sample of a function is
    /// grouped together, matching `perf report` and the FlameGraph tools. Enable
    /// this for instruction-level detail at the cost of that grouping.
    pub include_offset: bool,
}

/// Guess the format of `path` from its extension and a prefix of its contents.
pub fn detect_format(path: &Path, head: &str) -> Option<Format> {
    if path.extension().and_then(|e| e.to_str()) == Some("perf") {
        return Some(Format::PerfScript);
    }

    // JSON formats start with an object; anything else is treated as the
    // line-oriented `perf script` text format.
    if !head.trim_start().starts_with('{') {
        return Some(Format::PerfScript);
    }

    if head.contains("preprocessedProfileVersion")
        || head.contains("\"frameTable\"")
        || head.contains("\"stringArray\"")
    {
        return Some(Format::Gecko);
    }

    if head.contains("linux-perf-json-version") || head.contains("\"headers\"") {
        return Some(Format::PerfJson);
    }

    None
}

/// Detect the format of `path` and parse it into a [`Profile`].
pub fn load(path: &Path) -> Result<Profile, Box<dyn Error>> {
    load_with(path, Options::default())
}

/// Detect the format of `path` and parse it with explicit [`Options`].
pub fn load_with(path: &Path, options: Options) -> Result<Profile, Box<dyn Error>> {
    let head = read_head(path)?;
    let format = detect_format(path, &head)
        .ok_or_else(|| format!("could not detect profile format for {}", path.display()))?;
    load_as_with(path, format, options)
}

/// Parse `path` using an explicitly chosen `format`.
pub fn load_as(path: &Path, format: Format) -> Result<Profile, Box<dyn Error>> {
    load_as_with(path, format, Options::default())
}

/// Parse `path` using an explicit `format` and [`Options`].
pub fn load_as_with(path: &Path, format: Format, options: Options) -> Result<Profile, Box<dyn Error>> {
    match format {
        // Stream the (potentially very large) Gecko file straight from disk.
        Format::Gecko => gecko::parse_reader(BufReader::new(File::open(path)?)),
        Format::PerfJson => perf_json::parse_reader(BufReader::new(File::open(path)?)),
        // Stream line-by-line so multi-gigabyte `perf script` dumps never need
        // to be buffered into a single string.
        Format::PerfScript => {
            perf_script::parse_reader(BufReader::new(File::open(path)?), options)
        }
    }
}

/// Read up to 8 KiB from the start of a file for format sniffing.
fn read_head(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut buffer = vec![0u8; 8192];
    let read = File::open(path)?.read(&mut buffer)?;
    buffer.truncate(read);
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}
