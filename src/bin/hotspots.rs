//! `hotspots`: a token-efficient, machine-readable performance report.
//!
//! Where the graphical viewer is for humans, this binary is for tools (and
//! language models) that need to know *where a program spends its time* without
//! paging through a flame graph. It prints the same numbers `perf report`
//! would — period-weighted, grouped per function, offsets stripped — as a
//! compact table, a pruned hot-path tree, or JSON.
//!
//! Usage:
//!   hotspots <profile> [--format table|tree|json] [--top N]
//!                      [--thread N | --all] [--min-pct P] [--offset]

use std::error::Error;
use std::process::ExitCode;

use flamegraph_viewer::flame::{self, CallNode};
use flamegraph_viewer::parsers::{self, Options};
use flamegraph_viewer::profile::{Profile, Thread};

#[derive(Clone, Copy, PartialEq)]
enum Layout {
    Table,
    Tree,
    Json,
}

struct Args {
    path: String,
    layout: Layout,
    top: usize,
    thread: Option<usize>,
    all_threads: bool,
    min_pct: f64,
    include_offset: bool,
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            eprintln!(
                "usage: hotspots <profile> [--format table|tree|json] [--top N] \
                 [--thread N | --all] [--min-pct P] [--offset]"
            );
            return ExitCode::FAILURE;
        }
    };

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let mut path = None;
    let mut layout = Layout::Table;
    let mut top = 15;
    let mut thread = None;
    let mut all_threads = false;
    let mut min_pct = 1.0;
    let mut include_offset = false;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--format" | "-f" => {
                layout = match value()?.as_str() {
                    "table" => Layout::Table,
                    "tree" => Layout::Tree,
                    "json" => Layout::Json,
                    other => return Err(format!("unknown format: {other}")),
                }
            }
            "--top" | "-n" => top = value()?.parse().map_err(|_| "--top needs a number")?,
            "--thread" | "-t" => {
                thread = Some(value()?.parse().map_err(|_| "--thread needs an index")?)
            }
            "--all" | "-a" => all_threads = true,
            "--min-pct" | "-m" => {
                min_pct = value()?.parse().map_err(|_| "--min-pct needs a number")?
            }
            "--offset" => include_offset = true,
            "-h" | "--help" => return Err("show performance hot spots in a profile".into()),
            _ if path.is_none() => path = Some(arg),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }

    Ok(Args {
        path: path.ok_or("a profile path is required")?,
        layout,
        top,
        thread,
        all_threads,
        min_pct,
        include_offset,
    })
}

fn run(args: &Args) -> Result<(), Box<dyn Error>> {
    let options = Options { include_offset: args.include_offset };
    let profile = parsers::load_with(std::path::Path::new(&args.path), options)?;
    if profile.is_empty() {
        return Err("profile contains no samples".into());
    }

    // Which threads to report: an explicit one, all of them, or the busiest.
    let order = profile.threads_by_samples();
    let selected: Vec<usize> = match (args.thread, args.all_threads) {
        (Some(i), _) => vec![i],
        (None, true) => order.clone(),
        (None, false) => order.iter().take(1).copied().collect(),
    };

    match args.layout {
        Layout::Json => print_json(&profile, &selected, args),
        _ => print_text(&profile, &order, &selected, args),
    }
    Ok(())
}

fn print_text(profile: &Profile, order: &[usize], selected: &[usize], args: &Args) {
    let events = profile.event_count();
    println!(
        "profile: {} samples, {} events, {} threads, {}",
        profile.total_samples(),
        events,
        profile.threads.len(),
        format_duration(profile),
    );

    // Thread overview, busiest first, so the worst offenders are obvious.
    println!("\nthreads (by events):");
    for &i in order.iter().take(8) {
        let t = &profile.threads[i];
        let hottest = flame::flat_profile(t)
            .into_iter()
            .find(|r| r.self_events > 0)
            .map(|r| r.key)
            .unwrap_or_else(|| "-".into());
        println!(
            "  [{i}] {:>5.1}%  {:>10} ev  {}  hot:{}",
            pct(t.event_count(), events),
            t.event_count(),
            t.label(),
            hottest,
        );
    }
    if order.len() > 8 {
        println!("  ... {} more (use --all)", order.len() - 8);
    }

    for &i in selected {
        let Some(thread) = profile.threads.get(i) else {
            eprintln!("no thread with index {i}");
            continue;
        };
        let total = thread.event_count();
        println!("\n=== thread [{i}] {} — {total} events ===", thread.label());
        match args.layout {
            Layout::Tree => print_tree(thread, total, args),
            _ => print_table(thread, total, args),
        }
    }
}

/// Flat `perf report`-style table: hottest self time first.
fn print_table(thread: &Thread, total: u64, args: &Args) {
    println!("  self%  total%        self  function");
    let rows = flame::flat_profile(thread);
    let mut shown = 0;
    for row in &rows {
        let self_pct = pct(row.self_events, total);
        if self_pct < args.min_pct || shown >= args.top {
            continue;
        }
        shown += 1;
        println!(
            "  {:>5.1}  {:>6.1}  {:>10}  {}",
            self_pct,
            pct(row.total_events, total),
            row.self_events,
            row.key,
        );
    }
    if shown == 0 {
        println!("  (no function above --min-pct {:.1}%)", args.min_pct);
    }
}

/// Pruned top-down call tree, the structural view of where time goes.
fn print_tree(thread: &Thread, total: u64, args: &Args) {
    let tree = flame::call_tree(thread);
    println!("  total%  self%  function");
    for root in &tree.roots {
        print_node(thread, root, total, 0, args);
    }
}

fn print_node(thread: &Thread, node: &CallNode, total: u64, depth: usize, args: &Args) {
    if pct(node.total_samples, total) < args.min_pct {
        return;
    }
    println!(
        "  {:>6.1}  {:>5.1}  {:indent$}{}",
        pct(node.total_samples, total),
        pct(node.self_samples, total),
        "",
        thread.key(node.frame),
        indent = depth * 2,
    );
    for child in &node.children {
        print_node(thread, child, total, depth + 1, args);
    }
}

/// Minimal JSON so other tools can consume the report without scraping text.
fn print_json(profile: &Profile, selected: &[usize], args: &Args) {
    let mut threads = Vec::new();
    for &i in selected {
        let Some(thread) = profile.threads.get(i) else { continue };
        let total = thread.event_count();
        let functions: Vec<String> = flame::flat_profile(thread)
            .iter()
            .filter(|r| pct(r.self_events, total) >= args.min_pct)
            .take(args.top)
            .map(|r| {
                format!(
                    "{{\"function\":{},\"self\":{},\"total\":{},\"self_pct\":{:.2},\"total_pct\":{:.2}}}",
                    json_string(&r.key),
                    r.self_events,
                    r.total_events,
                    pct(r.self_events, total),
                    pct(r.total_events, total),
                )
            })
            .collect();
        threads.push(format!(
            "{{\"index\":{i},\"label\":{},\"events\":{total},\"functions\":[{}]}}",
            json_string(&thread.label()),
            functions.join(","),
        ));
    }
    println!(
        "{{\"samples\":{},\"events\":{},\"threads\":[{}]}}",
        profile.total_samples(),
        profile.event_count(),
        threads.join(","),
    );
}

fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 { 0.0 } else { part as f64 / whole as f64 * 100.0 }
}

/// Wall-clock span of the busiest thread, formatted for the header.
fn format_duration(profile: &Profile) -> String {
    let ns = profile
        .threads
        .iter()
        .map(|t| t.span_ns())
        .fold(0.0_f64, f64::max);
    if ns >= 1e9 {
        format!("{:.2}s", ns / 1e9)
    } else if ns >= 1e6 {
        format!("{:.1}ms", ns / 1e6)
    } else if ns >= 1e3 {
        format!("{:.1}us", ns / 1e3)
    } else {
        format!("{ns:.0}ns")
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
