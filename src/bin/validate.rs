//! Command-line profile validator.
//!
//! Parses a profile file (auto-detecting its format) and prints a summary plus
//! a few sanity checks. Useful for verifying parsers against real-world files
//! without launching the graphical viewer.
//!
//! Usage: `validate <path> [--format perf-json|perf-script|gecko]`

use std::error::Error;
use std::path::Path;
use std::process::ExitCode;

use flamegraph_viewer::flame::{self, CallNode};
use flamegraph_viewer::parsers::{self, Format};
use flamegraph_viewer::profile::Profile;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut path: Option<String> = None;
    let mut format: Option<Format> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--format" | "-f" => {
                format = match args.next().as_deref() {
                    Some(value) => match parse_format(value) {
                        Some(f) => Some(f),
                        None => {
                            eprintln!("unknown format: {value}");
                            return ExitCode::FAILURE;
                        }
                    },
                    None => {
                        eprintln!("--format requires a value");
                        return ExitCode::FAILURE;
                    }
                };
            }
            _ if path.is_none() => path = Some(arg),
            _ => {
                eprintln!("unexpected argument: {arg}");
                return ExitCode::FAILURE;
            }
        }
    }

    let Some(path) = path else {
        eprintln!("usage: validate <path> [--format perf-json|perf-script|gecko]");
        return ExitCode::FAILURE;
    };

    match validate(Path::new(&path), format) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("FAIL {path}: {error}");
            ExitCode::FAILURE
        }
    }
}

fn parse_format(value: &str) -> Option<Format> {
    match value {
        "perf-json" | "perf_json" => Some(Format::PerfJson),
        "perf-script" | "perf_script" => Some(Format::PerfScript),
        "gecko" => Some(Format::Gecko),
        _ => None,
    }
}

fn validate(path: &Path, format: Option<Format>) -> Result<(), Box<dyn Error>> {
    let profile = match format {
        Some(format) => parsers::load_as(path, format)?,
        None => parsers::load(path)?,
    };

    check(&profile)?;
    print_summary(path, &profile);
    Ok(())
}

/// Basic invariants every well-formed profile should satisfy.
fn check(profile: &Profile) -> Result<(), Box<dyn Error>> {
    if profile.is_empty() {
        return Err("profile contains no samples".into());
    }
    let all_samples = profile.threads.iter().flat_map(|t| &t.samples);
    if all_samples.clone().all(|s| s.stack.is_empty()) {
        return Err("every sample has an empty stack".into());
    }
    if all_samples.clone().any(|s| !s.timestamp_ns.is_finite()) {
        return Err("found a non-finite timestamp".into());
    }

    // Every thread's call tree must account for exactly its samples: each span's
    // self time plus its children's total time equals its own total, and the
    // roots cover every sample once. This is the same arithmetic `perf report`
    // does, so a mismatch means our aggregation disagrees with perf.
    for thread in &profile.threads {
        let tree = flame::call_tree(thread);
        if tree.total_samples != thread.event_count() {
            return Err(format!(
                "thread {}: call tree has {} events, expected {}",
                thread.label(),
                tree.total_samples,
                thread.event_count()
            )
            .into());
        }
        check_conservation(&tree.roots, &thread.label())?;
        let roots: u64 = tree.roots.iter().map(|n| n.total_samples).sum();
        if roots != tree.total_samples {
            return Err(format!(
                "thread {}: roots cover {roots} events, expected {}",
                thread.label(),
                tree.total_samples
            )
            .into());
        }
    }
    Ok(())
}

/// For every span, `self + Σ(children total) == own total`.
fn check_conservation(nodes: &[CallNode], label: &str) -> Result<(), Box<dyn Error>> {
    for node in nodes {
        let children: u64 = node.children.iter().map(|c| c.total_samples).sum();
        if node.self_samples + children != node.total_samples {
            return Err(format!(
                "thread {label}: span sample counts do not conserve \
                 ({} self + {children} children != {} total)",
                node.self_samples, node.total_samples
            )
            .into());
        }
        check_conservation(&node.children, label)?;
    }
    Ok(())
}

fn print_summary(path: &Path, profile: &Profile) {
    let order = profile.threads_by_samples();
    let max_depth = profile
        .threads
        .iter()
        .map(|t| t.max_depth())
        .max()
        .unwrap_or(0);

    println!("OK   {}", path.display());
    println!("  samples:    {}", profile.total_samples());
    println!("  events:     {}", profile.event_count());
    println!("  threads:    {}", profile.threads.len());
    println!("  max depth:  {max_depth}");

    println!("  threads by sample count:");
    for &i in order.iter().take(10) {
        let thread = &profile.threads[i];
        println!("    {:>6} samples  {}", thread.samples.len(), thread.label());
    }
    if order.len() > 10 {
        println!("    ... and {} more", order.len() - 10);
    }

    if let Some(&busiest) = order.first() {
        let thread = &profile.threads[busiest];
        if let Some(sample) = thread.samples.iter().find(|s| !s.stack.is_empty()) {
            let leaf = frame_label(thread.frame(sample.stack[0]));
            let root = frame_label(thread.frame(*sample.stack.last().unwrap()));
            println!("  example:    {leaf} ... {root}");
        }
    }
}

fn frame_label(frame: &flamegraph_viewer::profile::Frame) -> String {
    frame
        .symbol
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| frame.ip.clone())
}
