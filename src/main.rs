use serde::{Deserialize, Serialize};
// use serde_json::{Value};
use std::fs;
use std::collections::HashMap;
use bevy::{prelude::*, window::WindowResolution};

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
    let tid = 64477;

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

const X_EXTENT: f32 = 1100.;
const Y_EXTENT: f32 = 500.;

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    commands.spawn(Camera2d);

    let input = fs::read_to_string("perf.json").unwrap();
    let v: PerfData = serde_json::from_str(&input).unwrap();

    let pid = 64477;
    let tid = 64477;

    let mut max_depth = 0;
    for sample in &v.samples {
	if sample.pid == pid && sample.tid == tid {
            if sample.callchain.len() > max_depth {
                max_depth = sample.callchain.len();
            }
	}
    }

    let row_height = 20.0;
    let brick_height = 18.0;

    // TODO: rename to program start / finish
    let first_samp: usize = v.samples[0].timestamp;
    let last_samp: usize = v.samples.last().unwrap().timestamp;

    let sample_width: f32 = X_EXTENT*2.0 / (last_samp - first_samp) as f32;

    let mut cur_sample: Option<String> = None;

    let mut cur_sample_start: usize = 0;
    let mut cur_sample_end: usize = 0;

    for depth in 1..=max_depth {
        for sample in &v.samples {
            if sample.pid == pid && sample.tid == tid {
                if sample.callchain.len() >= depth {
                    match sample.callchain.get(sample.callchain.len() - depth) {
                        Some(frame) => {
                            match &cur_sample {
                                Some(cs) => {
                                    if *cs == frame.ip {
                                        // continue
                                        cur_sample_end = sample.timestamp;
                                    } else {
                                        // new frame type sample
                                        // cur_sample_start = sample.timestamp;
                                        // cur_sample_end = sample.timestamp; // TODO: is this wrong?
                                        let mut width = cur_sample_end - cur_sample_start;
                                        if cur_sample_end - cur_sample_start < 1 {
                                            width = 10000;
                                        }

                                        // middle origin point
                                        let x = (cur_sample_start - first_samp) + width / 2;
                                        let y = depth;

                                        let rect_mesh = meshes.add(Rectangle::new(width as f32 * sample_width, 18.0));
                                        let color = Color::hsl((x % 360) as f32, (y % 360) as f32, (x+y % 360) as f32);
                                        commands.spawn((
                                            Mesh2d(rect_mesh),
                                            MeshMaterial2d(materials.add(color)),
                                            Transform::from_xyz(
                                                -X_EXTENT + x as f32 * sample_width,
                                                -Y_EXTENT + y as f32 * row_height,
                                                0.0,
                                            ),
                                        ));
                                        // sample switch
                                        cur_sample = Some(frame.ip.clone());
                                        cur_sample_start = sample.timestamp;
                                        cur_sample_end = sample.timestamp;
                                    }
                                }
                                None => {
                                    // sample start
                                    cur_sample = Some(frame.ip.clone());
                                    cur_sample_start = sample.timestamp;
                                    cur_sample_end = sample.timestamp;
                                }
                            }
                        }
                        None => {
                            // no frame found at this depth
                            match &cur_sample {
                                Some(cs) => {
                                    // sample started, print it and end it.
                                    // cur_sample_end = sample.timestamp;
                                    let mut width = cur_sample_end - cur_sample_start;
                                    if cur_sample_end - cur_sample_start < 1 {
                                        width = 10000;
                                    }
                                    // middle origin point
                                    let x = cur_sample_start - first_samp + width / 2;
                                    let y = depth;

                                    let rect_mesh = meshes.add(Rectangle::new(width as f32 * sample_width, 18.0));
                                    let color = Color::hsl((x % 360) as f32, (y % 360) as f32, (x+y % 360) as f32);
                                    commands.spawn((
                                        Mesh2d(rect_mesh),
                                        MeshMaterial2d(materials.add(color)),
                                        Transform::from_xyz(
                                            -X_EXTENT + x as f32 * sample_width,
                                            -Y_EXTENT + y as f32 * row_height,
                                            0.0,
                                        ),
                                    ));
                                    cur_sample = None;
                                }
                                None => {
                                    // TODO: unneeded
                                }
                            }
                        }
                    }
                }
            } else {
                // match &cur_sample {
                //     Some(cs) => {
                //         cur_sample_end = sample.timestamp;
                //         // new thread started, print it and end it.
                //         let width = cur_sample_end - cur_sample_start;
                //         // middle origin point
                //         let x = cur_sample_start - first_samp + width / 2;
                //         let y = depth;

                //         let rect_mesh = meshes.add(Rectangle::new(width as f32 * sample_width, 18.0));
                //         let color = Color::hsl((x % 360) as f32, (y % 360) as f32, (x+y % 360) as f32);
                //         commands.spawn((
                //             Mesh2d(rect_mesh),
                //             MeshMaterial2d(materials.add(color)),
                //             Transform::from_xyz(
                //                 -X_EXTENT + x as f32 * sample_width,
                //                 -Y_EXTENT + y as f32 * row_height,
                //                 0.0,
                //             ),
                //         ));
                //         cur_sample = None;
                //     }
                //     None => {}
                // }
            }

        }
        // last sample if one is running
        match &cur_sample {
            Some(cs) => {
                // cur_sample_end = last_samp;

                let width = cur_sample_end - cur_sample_start;
                let x = cur_sample_start - first_samp + width / 2;
                let y = depth;

                let rect_mesh = meshes.add(Rectangle::new(width as f32 * sample_width, 18.0));
                let color = Color::hsl((x % 360) as f32, (y % 360) as f32, (x+y % 360) as f32);
                commands.spawn((
                    Mesh2d(rect_mesh),
                    MeshMaterial2d(materials.add(color)),
                    Transform::from_xyz(
                        -X_EXTENT + x as f32 * sample_width,
                        -Y_EXTENT + y as f32 * row_height,
                        0.0,
                    ),
                ));
                cur_sample = None;
            }
            None => {
                // TODO: unneeded
            }
        }

    }

    // for depth in 1..=2 { // max_depth
    //     for sample in &v.samples {
    //         if sample.pid == pid && sample.tid == tid {
    //             if sample.callchain.len() >= depth {
    //                 match sample.callchain.get(sample.callchain.len() - depth) {
    //                     Some(frame) => {
    //                         println!("frame {:?} at {}", frame, depth);
    //                         match &cur_sample {
    //                             Some(cs) => {
    //                                 if *cs == frame.ip {
    //                                     println!("CONTINUE");
    //                                     cur_sample_end = sample.timestamp;
    //                                 } else {
    //                                     cur_sample = Some(frame.ip.clone());
    //                                     cur_sample_start = sample.timestamp;
    //                                     cur_sample_end = sample.timestamp;

    //                                     let rect_mesh = if ((cur_sample_end - cur_sample_start) as f32) < 1.0 {
    //                                         // min sample width
    //                                         meshes.add(Rectangle::new(1.0 * sample_width, brick_height))
    //                                     } else {
    //                                         println!("NON MIN SiZE 1");
    //                                         meshes.add(Rectangle::new((cur_sample_end - cur_sample_start) as f32 * sample_width, brick_height))
    //                                     };
    //                                     let color = Color::hsl(0.22, 0.91, 0.7);
    //                                     commands.spawn((
    //                                         Mesh2d(rect_mesh),
    //                                         MeshMaterial2d(materials.add(color)),
    //                                         Transform::from_xyz(
    //                                             (-X_EXTENT + 100.0) + (cur_sample_start - first_samp) as f32 * sample_width,
    //                                             0.0 + depth as f32 * row_height,
    //                                             0.0,
    //                                         ),
    //                                     ));
    //                                 }
    //                             }
    //                             None => {
    //                                 println!("start set {}", sample.timestamp as f64);
    //                                 cur_sample = Some(frame.ip.clone());
    //                                 cur_sample_start = sample.timestamp;
    //                                 cur_sample_end = sample.timestamp;
    //                             }
    //                         }
    //                     }
    //                     None => {
    //                         match cur_sample {
    //                             Some(cs) => {
    //                                 let rect_mesh = if ((cur_sample_end - cur_sample_start) as f32) < 1.0 {
    //                                     // min sample width
    //                                     meshes.add(Rectangle::new(1.0 * sample_width, brick_height))
    //                                 } else {
    //                                     println!("NON MIN SiZE 2");
    //                                     meshes.add(Rectangle::new((cur_sample_end - cur_sample_start) as f32 * sample_width, brick_height))
    //                                 };
    //                                 let color = Color::hsl(0.22, 0.75, 0.92);

    //                                 println!("x2 = {}", (cur_sample_start - first_samp) as f32 * sample_width);
    //                                 println!("y2 = {}", Y_EXTENT - depth as f32);
    //                                 commands.spawn((
    //                                     Mesh2d(rect_mesh),
    //                                     MeshMaterial2d(materials.add(color)),
    //                                     Transform::from_xyz(
    //                                         -X_EXTENT + (cur_sample_start - first_samp) as f32 * sample_width,
    //                                         0.0 + depth as f32 * row_height,
    //                                         0.0,
    //                                     ),
    //                                 ));
    //                                 println!("cur sample reset 2");
    //                                 cur_sample = None;
    //                             }
    //                             None => {
    //                                 // println!("cur sample reset 3");
    //                                 // cur_sample = None
    //                             }
    //                         }
    //                     }
    //                 }
    //             } else {
    //                 match cur_sample {
    //                     Some(cs) => {
    //                         let rect_mesh = if ((cur_sample_end - cur_sample_start) as f32) < 1.0 {
    //                             // min sample width
    //                             meshes.add(Rectangle::new(1.0 * sample_width, brick_height))
    //                         } else {
    //                             println!("NON MIN SiZE 2");
    //                             meshes.add(Rectangle::new((cur_sample_end - cur_sample_start) as f32 * sample_width, brick_height))
    //                         };
    //                         let color = Color::hsl(0.22, 0.95, 0.45);

    //                         println!("x2 = {}", (cur_sample_start - first_samp) as f32 * sample_width);
    //                         println!("y2 = {}", Y_EXTENT - depth as f32);
    //                         commands.spawn((
    //                             Mesh2d(rect_mesh),
    //                             MeshMaterial2d(materials.add(color)),
    //                             Transform::from_xyz(
    //                                 -X_EXTENT + (cur_sample_start - first_samp) as f32 * sample_width,
    //                                 0.0 + depth as f32 * row_height,
    //                                 0.0,
    //                             ),
    //                         ));
    //                         println!("cur sample reset 2");
    //                         cur_sample = None;
    //                     }
    //                     None => {
    //                         // println!("cur sample reset 3");
    //                         // cur_sample = None
    //                     }
    //                 }
    //                 cur_sample = None;
    //             }
    //         }
    //     }
    //     match cur_sample {
    //         Some(cs) => {
    //             let rect_mesh = if ((cur_sample_end - cur_sample_start) as f32) < 1.0 {
    //                 // min sample width
    //                 meshes.add(Rectangle::new(1.0 * sample_width, brick_height))
    //             } else {
    //                 println!("NON MIN SiZE 2");
    //                 meshes.add(Rectangle::new((cur_sample_end - cur_sample_start) as f32 * sample_width, brick_height))
    //             };

    //             let color = Color::hsl(0.22, 0.34, 0.7);

    //             println!("x2 = {}", (cur_sample_start - first_samp) as f32 * sample_width);
    //             println!("y2 = {}", Y_EXTENT - depth as f32);
    //             commands.spawn((
    //                 Mesh2d(rect_mesh),
    //                 MeshMaterial2d(materials.add(color)),
    //                 Transform::from_xyz(
    //                     -X_EXTENT + (cur_sample_start - first_samp) as f32 * sample_width,
    //                     0.0 + depth as f32 * row_height,
    //                     0.0,
    //                 ),
    //             ));
    //             println!("cur sample reset 2");
    //             cur_sample = None;
    //         }
    //         None => {
    //             // println!("cur sample reset 3");
    //             // cur_sample = None
    //         }
    //     }
    //     cur_sample = None
    // }
}



fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = fs::read_to_string("perf.json")?;
    let v: PerfData = serde_json::from_str(&input)?;
    let version_str = format!("version: {}", v.linux_perf_json_version);
    // to_graph(v.samples);
    to_flamegraph(v.samples);
    // println!("{}",version_str);



    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                resolution: WindowResolution::new(X_EXTENT as u32 *2.0 as u32, Y_EXTENT as u32 *2.0 as u32).with_scale_factor_override(1.0),
                ..default()
            }),
            ..default()
        }))
        .add_systems(Startup, setup).run();

    return Ok(());
}
