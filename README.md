# flamegraph-viewer

An interactive flame graph viewer and profile analyser for native programs. It
reads three common profile formats into one in-memory model and lets you explore
where a program spends its time — either visually (a Bevy GUI) or from the
command line (LLM- and script-friendly output).

## Supported formats

The format is auto-detected from the file contents:

| Format            | Produced by                              | Notes                                   |
| ----------------- | ---------------------------------------- | --------------------------------------- |
| `perf` JSON       | `perf data convert --to-json`            | one sample = one event (no periods)     |
| `perf script`     | `perf script` (text)                     | samples weighted by their `period`      |
| Gecko / Firefox   | Firefox Profiler, [Vernier](https://github.com/jhawthorn/vernier) | one sample = one event |

Counts are matched to `perf report`: stacks are weighted by their sample
`period` where the format records one, and symbol offsets (`+0x…`) are stripped
so all occurrences of a function group together (pass `--offset` to keep them).

## Generating a profile with `perf`

`perf` is Linux-only. First **record** your program; this writes `perf.data`
into the current directory:

```sh
# Sample at 999 Hz and capture call graphs (-g) for a command you launch.
perf record -F 999 -g -- ./your-program --your --args

# ...or attach to a running process by PID:
perf record -F 999 -g -p <PID>

# ...or sample the whole system until you press Ctrl-C:
perf record -F 999 -g -a
```

> Tip: build (or keep) your program with frame pointers, or use `perf record
> --call-graph dwarf`, so the call stacks resolve. Install debug symbols for the
> libraries you care about, otherwise frames show up as raw addresses.

Then convert `perf.data` into one of the two text formats the viewer reads.

**`perf script` (text):**

```sh
# Reads ./perf.data by default; -i points at a specific file.
perf script > out.perf
perf script -i perf.data > out.perf
```

This is the richer of the two formats — it carries each sample's `period`, so
the viewer's event counts match `perf report` exactly.

**perf JSON:**

```sh
perf data convert --to-json perf.json
perf data convert -i perf.data --to-json perf.json
```

JSON has no `period` field, so every sample counts as one event. Use `perf
script` output when you need counts that line up with `perf report`.

## Build and run

Build everything once (the GUI and both CLIs), then pass your profile as the
first argument. The format is detected automatically, so the same file works
with every entry point:

```sh
cargo build --release

./target/release/flamegraph-viewer out.perf     # interactive GUI
./target/release/flamegraph-viewer perf.json     # JSON works too

./target/release/hotspots out.perf               # text bottleneck report
./target/release/validate out.perf               # summary + sanity check
```

## The GUI

Launch with a profile path. It opens on an **overview** of every thread; pick
one to inspect in detail.

| Key          | Action                                              |
| ------------ | --------------------------------------------------- |
| arrows       | overview: select a thread (otherwise pan)           |
| `Enter` / click | overview: open the selected thread               |
| `Tab`        | cycle view: overview → flame chart → flame graph → top |
| `Esc`        | return to the thread overview                       |
| `S`          | in the top table, cycle the sort column             |
| `+` / `-`    | in the top table, grow / shrink the font (also `X`/`Z`) |
| `[` / `]`    | switch to the previous / next thread                |
| `Z` / `X`    | zoom the time axis                                  |
| `C` / `V`    | zoom the depth axis                                 |
| hover        | highlight the brick and every brick sharing its symbol |
| click        | show the function's self and total time             |

Views:

- **Overview** (default) – a grid of every thread's flame graph thumbnail.
  Select one with the arrow keys or mouse and press `Enter` (or click) to open
  it; `Tab` cycles through the detailed views below for that thread.
- **Flame chart** – samples in time order (left-to-right is wall-clock time). A
  faint vertical tick marks the timestamp of each sample (hold `Alt` to read the
  nearest tick's timestamp). The chart's left edge is the displayed thread's
  *own* first sample, so the header reports how far into the profile that thread
  actually started — making it obvious when a thread did not begin at the start
  of the capture.
- **Flame graph** – left-heavy / size-ordered icicle (widest sibling first);
  best for spotting the largest stacks regardless of when they happened.
- **Top table** – every function with its self and total time, sortable by self
  time, total time, or name; resize the text with `+`/`-`.

## `hotspots` — machine-friendly bottleneck report

A token-efficient summary designed to be fed to a script or an LLM.

```
hotspots <profile> [--format table|tree|json] [--top N]
                   [--thread N | --all] [--min-pct P] [--offset]
```

- `--format table` (default) – a flat self/total table per thread.
- `--format tree` – a pruned call tree (use `--min-pct` to prune).
- `--format json` – the same data as JSON for further processing.
- `--top N` – limit table rows / tree breadth (default 20).
- `--thread N` – inspect one thread (default: the busiest); `--all` for every thread.
- `--offset` – keep `+0x…` offsets instead of grouping by function.

## Validation

Accuracy is checked against `perf`'s own numbers and the external
[`inferno`](https://github.com/jonhoo/inferno) collapser as an offline ground
truth:

```sh
cargo test            # unit + cross-tool validation tests
./scripts/validate.sh # runs the tests and validates any real files present
```

`tests/external_tools.rs` cross-checks per-function self/total event counts
against `inferno-collapse-perf` when it is installed (the test is skipped if it
is not). `tests/consistency.rs` re-derives every thread's call tree with an
independent oracle and asserts the conservation laws `self + Σchild.total =
total` hold throughout.

## Layout / source map

- `src/profile.rs` – the common IR (frames, interning, samples, threads).
- `src/parsers/` – `perf_json`, `perf_script`, `gecko`, plus format detection.
- `src/flame.rs` – layout (`layout`, `left_heavy`) and aggregation
  (`call_tree`, `flat_profile`, `symbol_stats`).
- `src/main.rs` – the Bevy renderer.
- `src/bin/hotspots.rs`, `src/bin/validate.rs` – the CLIs.
