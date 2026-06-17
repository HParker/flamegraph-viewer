// Synthetic `perf script` generator for large-file testing.
// Usage: gen_perf <target_gigabytes> <output_path>
use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};

fn main() {
    let mut args = env::args().skip(1);
    let gb: f64 = args.next().unwrap_or_else(|| "1".into()).parse().unwrap();
    let path = args.next().unwrap_or_else(|| "big.perf".into());
    let target = (gb * 1024.0 * 1024.0 * 1024.0) as u64;

    // A pool of functions; each line precomputed for speed.
    const NFUNCS: usize = 4000;
    let frame_lines: Vec<String> = (0..NFUNCS)
        .map(|i| format!("\t{:012x} func_{:04} (/app/lib/module_{:02}.so)\n", 0x400000 + i * 0x40, i, i % 50))
        .collect();

    // Simple acyclic call graph: function i may call a few functions with higher index.
    let out = File::create(&path).unwrap();
    let mut w = BufWriter::with_capacity(1 << 20, out);

    let mut rng: u64 = 0x9e3779b97f4a7c15;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };

    let mut written: u64 = 0;
    let mut ts: u64 = 1_000_000_000; // ns-ish; emitted as seconds
    let mut sample = 0u64;
    let mut header = String::with_capacity(64);

    // A persistent call stack that mutates slightly each sample, the way a real
    // program walks its call tree. Consecutive samples therefore share long
    // prefixes (producing wide, merged bricks) and every stack shares the same
    // root frame, so the flamegraph has the usual pyramid shape.
    let mut stack: Vec<usize> = vec![0, 1];
    while written < target {
        // Unwind a few frames, then descend to a fresh target depth.
        let pop = (next() % 4) as usize;
        for _ in 0..pop {
            if stack.len() > 2 {
                stack.pop();
            }
        }
        let target_depth = 8 + (next() % 40) as usize; // 8..48
        while stack.len() < target_depth {
            let cur = *stack.last().unwrap();
            // Child: a function with a slightly higher index (acyclic graph).
            let step = 1 + (next() % 80) as usize;
            let child = (cur + step).min(NFUNCS - 1);
            stack.push(child);
            if child == NFUNCS - 1 {
                break;
            }
        }

        ts += 1000 + (next() % 4000); // ~1-5us between samples
        let secs = ts / 1_000_000_000;
        let frac = ts % 1_000_000_000;
        header.clear();
        use std::fmt::Write as _;
        let _ = writeln!(header, "program 1000/1000 {}.{:09}: 1 cycles:", secs, frac);
        w.write_all(header.as_bytes()).unwrap();
        written += header.len() as u64;
        // leaf-first
        for &f in stack.iter().rev() {
            let line = &frame_lines[f];
            w.write_all(line.as_bytes()).unwrap();
            written += line.len() as u64;
        }
        w.write_all(b"\n").unwrap();
        written += 1;
        sample += 1;
    }
    w.flush().unwrap();
    eprintln!("wrote {} samples, {:.2} GiB to {}", sample, written as f64 / (1u64<<30) as f64, path);
}
