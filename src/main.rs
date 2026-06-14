use serde::{Deserialize, Serialize};
// use serde_json::{Value};
use std::fs;
use std::collections::HashMap;
// use bevy::prelude::*;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct PerfHeader {
    header_version: usize,
    captured_on: String, // "2026-06-11T22:27:25Z"
    data_offset: usize,
    data_size: usize,
    feat_offset: usize,
    hostname: String,
    os_release: String,
    arch: String,
    cpu_desc: String,
    cpuid: String,
    nrcpus_online: usize,
    nrcpus_avail: usize,
    perf_version: String,
    cmdline: Vec<String>
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct PerfData {
    headers: PerfHeader,
    samples: Vec<PerfSample>,
    linux_perf_json_version: usize
}

#[derive(Serialize, Deserialize, Hash, PartialEq, Eq, Debug)]
struct CallFrame {
    ip: String,
    symbol: Option<String>,
    dso: Option<String>
}



#[derive(Serialize, Deserialize)]
struct PerfSample {
    timestamp: usize,
    pid: usize,
    tid: usize,
    comm: String,
    callchain: Vec<CallFrame>
}

#[derive(Debug)]
struct FGraph {
    total_samples: usize,
    self_samples: usize,
    ip: String,
    symbol: Option<String>,
    dso: Option<String>
}

struct Flamegraph {
    // metadata,
    graph: HashMap<(usize, usize), HashMap<String, FGraph>>,
}

// draw verticle in terminal for now
// callstack -> samples
fn to_flamegraph(samples: Vec<PerfSample>) {
    let mut call_graph: HashMap<Vec<String>, f64> = HashMap::new();

    let pid = 64477;
    let tid = 64489;

    let mut time_sum = 0;
    let mut prev_timestamp: Option<usize> = None;

    for sample in &samples {
	if sample.pid == pid && sample.tid == tid {
	    match prev_timestamp {
		Some(ps) => {
		    time_sum += sample.timestamp - ps;
		    prev_timestamp = Some(sample.timestamp)
		}
		None => {
		    prev_timestamp = Some(sample.timestamp)
		}
	    }
	}
    }

    prev_timestamp = None;
    for sample in &samples {
	if sample.pid == pid && sample.tid == tid {
	    match prev_timestamp {
		Some(ps) => {
		    let cc: Vec<&String> = sample.callchain.iter().rev().map(|callframe| match &callframe.symbol { Some(symbol) => symbol, None => &callframe.ip }).collect();
		    println!("{:?} : {}/{} ({:.2}%)", cc, sample.timestamp - ps, time_sum, ((sample.timestamp - ps) as f64 / time_sum as f64) * 100.0);
		    prev_timestamp = Some(sample.timestamp)
		}
		None => {
		    prev_timestamp = Some(sample.timestamp)
		}
	    }
	}
    }
}

// self and total time counting
fn to_graph(samples: Vec<PerfSample>) {
    let mut root = Flamegraph {
	graph: HashMap::new()
    };

    for sample in samples {
	match root.graph.get_mut(&(sample.pid, sample.tid)) {
	    Some(thread_profile) => {
		let mut self_sample = true;
		for frame in sample.callchain {
		    match thread_profile.get_mut(&frame.ip) {
			Some(child) => {
			    child.total_samples += 1;
			    if self_sample {
				child.self_samples += 1;
			    }
			},
			None => {
			    let new_frame = FGraph {
				total_samples: 1,
				self_samples: if self_sample { 1 } else { 0 },
				ip: frame.ip.clone(),
				symbol: frame.symbol.clone(),
				dso: frame.dso.clone(),
			    };
			    thread_profile.insert(frame.ip.clone(), new_frame);
			},
		    }
		    self_sample = false;
		}
	    }
	    None => {
		root.graph.insert((sample.pid, sample.tid), HashMap::new());
		// TODO: insert sample
	    }
	}
    }

    let pid = 64477;
    let tid = 64489;

    let thread_data = root.graph.get(&(pid, tid)).unwrap();

    let mut sorted_vec: Vec<(&String, &FGraph)> = thread_data.iter().collect();
    sorted_vec.sort_unstable_by(|a, b| a.1.total_samples.cmp(&b.1.total_samples));

    println!("-----------------------------------------------------------------");

    sorted_vec.sort_unstable_by(|a, b| a.1.self_samples.cmp(&b.1.self_samples));

    for (key, value) in &sorted_vec {
        println!("{}: {:?}", key, value);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = fs::read_to_string("perf.json")?;
    let v: PerfData = serde_json::from_str(&input)?;
    let version_str = format!("version: {}", v.linux_perf_json_version);
    // to_graph(v.samples);
    to_flamegraph(v.samples);
    println!("{}",version_str);
    // App::new().run();
    return Ok(());
}
