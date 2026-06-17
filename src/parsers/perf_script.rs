//! Parser for the text output of `perf script` (the same stack format that
//! speedscope imports).
//!
//! The output is a sequence of records separated by blank lines. Each record
//! starts with a header line describing the sample followed by one indented
//! line per stack frame, innermost (leaf) frame first:
//!
//! ```text
//! rg   64477/64477   26795.734316:          1 cpu/cycles/Pu:
//!     ffffffff94f14db5 [unknown] ([unknown])
//!     7f3988eb7440 _start+0x0 (/usr/lib64/ld-linux-x86-64.so.2)
//! ```

use std::collections::HashMap;
use std::error::Error;
use std::io::BufRead;

use crate::parsers::Options;
use crate::profile::{Frame, FrameId, Profile, Sample, ThreadBuilder};

/// Header fields of a `perf script` record.
struct Record {
    comm: Option<String>,
    pid: u64,
    tid: u64,
    timestamp_ns: f64,
    weight: u64,
    stack: Vec<FrameId>,
}

/// Parse `perf script` text into a [`Profile`].
pub fn parse_str(input: &str, options: Options) -> Result<Profile, Box<dyn Error>> {
    parse_lines(input.lines().map(Ok), options)
}

/// Parse `perf script` output by streaming a reader line-by-line, so very large
/// files never need to be held in memory as a single string.
pub fn parse_reader<R: BufRead>(reader: R, options: Options) -> Result<Profile, Box<dyn Error>> {
    parse_lines(reader.lines(), options)
}

/// Core parser driven by an iterator of lines (each either borrowed or owned).
fn parse_lines<S, I>(lines: I, options: Options) -> Result<Profile, Box<dyn Error>>
where
    S: AsRef<str>,
    I: IntoIterator<Item = Result<S, std::io::Error>>,
{
    let mut builder = ThreadBuilder::default();
    let mut current: Option<Record> = None;
    // Distinct frame lines are few relative to their occurrences; caching the
    // raw line means each one is parsed and interned exactly once, which is the
    // dominant cost when streaming multi-gigabyte files.
    let mut frame_ids: HashMap<String, FrameId> = HashMap::new();

    for line in lines {
        let line = line?;
        let line = line.as_ref();
        if line.trim().is_empty() {
            flush(&mut builder, current.take());
            continue;
        }

        // Stack frame lines are indented; header lines are not.
        if line.starts_with(char::is_whitespace) {
            if let Some(record) = current.as_mut()
                && let Some(id) = frame_id(&mut builder, &mut frame_ids, line, options)
            {
                record.stack.push(id);
            }
        } else {
            // A new header line implicitly ends the previous record.
            flush(&mut builder, current.take());
            current = parse_header_line(line);
        }
    }
    flush(&mut builder, current.take());

    Ok(builder.finish())
}

/// Resolve a raw frame line to an interned [`FrameId`], parsing it only the
/// first time the exact line is seen.
fn frame_id(
    builder: &mut ThreadBuilder,
    cache: &mut HashMap<String, FrameId>,
    line: &str,
    options: Options,
) -> Option<FrameId> {
    let key = line.trim_end();
    if let Some(&id) = cache.get(key) {
        return Some(id);
    }
    let frame = parse_frame_line(line, options)?;
    let id = builder.intern(frame);
    cache.insert(key.to_string(), id);
    Some(id)
}

fn flush(builder: &mut ThreadBuilder, record: Option<Record>) {
    if let Some(record) = record
        && !record.stack.is_empty()
    {
        builder.push(
            record.pid,
            record.tid,
            record.comm,
            Sample {
                timestamp_ns: record.timestamp_ns,
                stack: record.stack,
                weight: record.weight,
            },
        );
    }
}

/// Parse a record header such as `rg   64477/64477   26795.734316:   1 cpu/...`.
///
/// The command name may contain spaces, so the `pid/tid` token is located
/// positionally; the timestamp is the following `<seconds>:` token and the
/// sample period (event weight) is the integer token after that.
fn parse_header_line(line: &str) -> Option<Record> {
    let tokens: Vec<&str> = line.split_whitespace().collect();

    let pid_tid_index = tokens.iter().position(|t| is_pid_tid(t))?;
    let (pid, tid) = split_pid_tid(tokens[pid_tid_index])?;

    // The timestamp is the next token ending in ':' (e.g. "26795.734316:").
    let timestamp_index = tokens[pid_tid_index + 1..]
        .iter()
        .position(|t| t.strip_suffix(':').and_then(|t| t.parse::<f64>().ok()).is_some())?
        + pid_tid_index
        + 1;
    let timestamp_seconds: f64 = tokens[timestamp_index].trim_end_matches(':').parse().ok()?;

    // The period follows the timestamp; absent or unparseable, weight is 1.
    let weight = tokens
        .get(timestamp_index + 1)
        .and_then(|t| t.parse::<u64>().ok())
        .unwrap_or(1)
        .max(1);

    let comm = tokens[..pid_tid_index].join(" ");

    Some(Record {
        comm: (!comm.is_empty()).then_some(comm),
        pid,
        tid,
        timestamp_ns: timestamp_seconds * 1_000_000_000.0,
        weight,
        stack: Vec::new(),
    })
}

/// Parse an indented stack line such as
/// `7f3988eb7440 _start+0x0 (/usr/lib64/ld-linux-x86-64.so.2)`.
fn parse_frame_line(line: &str, options: Options) -> Option<Frame> {
    let line = line.trim();
    let (addr, rest) = line.split_once(char::is_whitespace)?;

    // The dso is the last parenthesised group; the symbol is everything in
    // between. Both may be "[unknown]".
    let (symbol_part, dso_part) = match rest.rsplit_once(" (") {
        Some((sym, dso)) => (sym.trim(), Some(dso.trim_end_matches(')'))),
        None => (rest.trim(), None),
    };

    let symbol = clean(symbol_part).map(|s| {
        if options.include_offset {
            s
        } else {
            strip_offset(&s).to_string()
        }
    });

    Some(Frame {
        ip: format!("0x{}", addr.trim_start_matches("0x")),
        symbol,
        dso: dso_part.and_then(clean),
    })
}

/// Drop the trailing `+0x<hex>` instruction offset `perf script` appends to a
/// symbol, so every sample of a function groups together as `perf report` does.
fn strip_offset(symbol: &str) -> &str {
    match symbol.rsplit_once("+0x") {
        Some((name, offset)) if !name.is_empty() && offset.bytes().all(|b| b.is_ascii_hexdigit()) => {
            name
        }
        _ => symbol,
    }
}

/// Drop placeholder values perf emits when information is missing.
fn clean(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value == "[unknown]" {
        None
    } else {
        Some(value.to_string())
    }
}

fn is_pid_tid(token: &str) -> bool {
    split_pid_tid(token).is_some()
}

fn split_pid_tid(token: &str) -> Option<(u64, u64)> {
    let (pid, tid) = token.split_once('/')?;
    Some((pid.parse().ok()?, tid.parse().ok()?))
}
