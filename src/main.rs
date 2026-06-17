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

#[derive(Serialize, Deserialize, Hash, PartialEq, Eq, Debug, Clone)]
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

// struct Flamegraph {
//     // metadata,
//     graph: HashMap<(usize, usize), HashMap<String, FGraph>>,
// }

#[derive(Component)]
struct HoverText(CallFrame);

#[derive(Component)]
struct Tooltip;

#[derive(Component)]
struct TrueScale {
    x: usize,
    width: usize,
    scale: f32,
}

const X_EXTENT: f32 = 1600.;
const Y_EXTENT: f32 = 900.;

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    // commands.spawn(Camera2d);
    commands.spawn(Camera2d::default());

    let input = fs::read_to_string("perf.json").unwrap();
    let v: PerfData = serde_json::from_str(&input).unwrap();

    let pid = 64477;
    let tid = 64477;

    // TODO: assert more than 3 samples
    let sample_len = v.samples.len();

    let first_samp: usize = v.samples[0].timestamp;
    let last_samp: usize = v.samples.last().unwrap().timestamp;

    let mut sample_weight: Vec<usize> = vec![];
    // sample_weight.push(v.samples[1].timestamp - v.samples[0].timestamp);
    for window in v.samples.windows(2) {
        let before = window[0].timestamp;
        let after = window[1].timestamp;
        let weight = after - before;
        sample_weight.push(weight);
    }
    // sample_weight.push((v.samples[sample_len - 1].timestamp - v.samples[sample_len - 2].timestamp) / 2);
    sample_weight.push(v.samples[sample_len - 1].timestamp - v.samples[sample_len - 2].timestamp);

    // sample start
    let mut sample_start: Vec<usize> = vec![];
    let mut debug_width = 0;
    for sample in v.samples.iter() {
        sample_start.push(sample.timestamp - first_samp);
    }

    let mut max_depth = 0;
    for sample in &v.samples {
	if sample.pid == pid && sample.tid == tid {
            if sample.callchain.len() > max_depth {
                max_depth = sample.callchain.len();
            }
	}
    }

    let sample_scale = 0.001;
    let row_height = 20.0;
    let brick_height = 18.0;

    let mut cur_sample: Option<Vec<CallFrame>> = None;
    let mut cur_sample_weight: usize = 0;
    let mut cur_sample_start: usize = 0;

    for depth in 1..=max_depth { // max_depth
        for (sample_index, sample) in v.samples.iter().enumerate() {
            if sample.pid == pid && sample.tid == tid {
                if sample.callchain.len() >= depth {
                    match sample.callchain.get(sample.callchain.len() - depth) {
                        Some(frame) => {
                            let sample_stack: Vec<String> = sample.callchain[(sample.callchain.len() - depth)..=(sample.callchain.len() - 1)].iter().map(|cc| cc.ip.clone()).collect();
                            match &cur_sample {
                                Some(cs) => {
                                    let cur_cc: Vec<String> = cs.iter().map(|cc| cc.ip.clone()).collect();
                                    if sample_stack == cur_cc {
                                        // join samples
                                        // if sample_start[sample_index] > (cur_sample_start + cur_sample_weight / 2) {

                                        // }
                                        cur_sample_weight += sample_start[sample_index] - (cur_sample_start + cur_sample_weight);
                                        cur_sample_weight += sample_weight[sample_index];
                                    } else {
                                        // new sample
                                        // TODO: build truescale here first and use it like when resizing
                                        let x = cur_sample_start;
                                        let width = cur_sample_weight;
                                        let text = cs[0].symbol.clone().unwrap_or(cs[0].ip.clone());
                                        // let rect = Rectangle::new(width as f32 * sample_scale, brick_height);
                                        let rect = Rectangle::default();
                                        let rect_mesh = meshes.add(rect);
                                        let color = Color::hsl((x + depth % 360) as f32, 0.33, 0.44);
                                        let mut transform = Transform::from_xyz(
                                            -X_EXTENT + (x + width/2) as f32 * sample_scale,
                                            -Y_EXTENT + depth as f32 * row_height,
                                            0.0,
                                        );
                                        transform.scale = Vec3::new(width as f32 * sample_scale, brick_height, 1.0);

                                        commands.spawn((
                                            Mesh2d(rect_mesh),
                                            MeshMaterial2d(materials.add(color)),
                                            transform,
                                            TrueScale { x: x, width: width, scale: sample_scale },
                                            HoverText(cs[0].clone()),
                                            // children![Text2d::new(text)],
                                        ))
                                            .observe(spawn_tooltip)
                                            .observe(despawn_tooltip);

                                        // sample start
                                        let scc: Vec<CallFrame> = sample.callchain[(sample.callchain.len() - depth)..=(sample.callchain.len() - 1)].into();
                                        cur_sample = Some(scc);
                                        cur_sample_start = sample_start[sample_index];
                                        cur_sample_weight = sample_weight[sample_index];
                                    }
                                }
                                None => {
                                    // sample start
                                    let scc: Vec<CallFrame> = sample.callchain[(sample.callchain.len() - depth)..=(sample.callchain.len() - 1)].into();
                                    cur_sample = Some(scc);
                                    cur_sample_start = sample_start[sample_index];
                                    cur_sample_weight = sample_weight[sample_index];
                                }
                            }


                        }
                        None => {},
                    }
                }  else {
                    match &cur_sample {
                        Some(cs) => {
                            let x = cur_sample_start;
                            let width = cur_sample_weight;
                            let text = cs[0].symbol.clone().unwrap_or(cs[0].ip.clone());
                            // let rect = Rectangle::new(width as f32 * sample_scale, brick_height);
                            let rect = Rectangle::default();
                            let rect_mesh = meshes.add(rect);
                            let color = Color::hsl((x + depth % 360) as f32, 0.33, 0.44);
                            let mut transform = Transform::from_xyz(
                                -X_EXTENT + (x + width/2) as f32 * sample_scale,
                                -Y_EXTENT + depth as f32 * row_height,
                                0.0,
                            );
                            transform.scale = Vec3::new(width as f32 * sample_scale, brick_height, 1.0);

                            commands.spawn((
                                Mesh2d(rect_mesh),
                                MeshMaterial2d(materials.add(color)),
                                transform,
                                TrueScale { x: x, width: width, scale: sample_scale },
                                HoverText(cs[0].clone()),
                                // children![Text2d::new(text)],
                            ))
                                .observe(spawn_tooltip)
                                .observe(despawn_tooltip);

                            cur_sample = None;
                            cur_sample_start = 0;
                            cur_sample_weight = 0;
                        }
                        None => {},
                    }
                }
            }
        }
        match &cur_sample {
            Some(cs) => {
                let x = cur_sample_start;
                let width = cur_sample_weight;
                let text = cs[0].symbol.clone().unwrap_or(cs[0].ip.clone());
                // let rect = Rectangle::new(width as f32 * sample_scale, brick_height);
                let rect = Rectangle::default();
                let rect_mesh = meshes.add(rect);
                let color = Color::hsl((x + depth % 360) as f32, 0.33, 0.44);
                let mut transform = Transform::from_xyz(
                    -X_EXTENT + (x + width/2) as f32 * sample_scale,
                    -Y_EXTENT + depth as f32 * row_height,
                    0.0,
                );
                transform.scale = Vec3::new(width as f32 * sample_scale, brick_height, 1.0);

                commands.spawn((
                    Mesh2d(rect_mesh),
                    MeshMaterial2d(materials.add(color)),
                    transform,
                    TrueScale { x: x, width: width, scale: sample_scale },
                    HoverText(cs[0].clone()),
                    // children![Text2d::new(text)],
                ))
                    .observe(spawn_tooltip)
                    .observe(despawn_tooltip);
                cur_sample = None;
                cur_sample_start = 0;
                cur_sample_weight = 0;
            }
            None => {},
        }
    }
}

fn spawn_tooltip(
    over: On<Pointer<Over>>,
    hover_texts: Query<&HoverText>,
    mut commands: Commands,
) {
    if let Ok(hover_text) = hover_texts.get(over.entity) {
        if let Some(position) = over.hit.position {
            let text = format!("ip = {} symbol = {:?}", hover_text.0.ip, hover_text.0.symbol);
            // Spawn the tooltip as a child of the hovered entity
            commands.spawn((
                Text2d::new(text),
                Tooltip,
                Transform::from_xyz(
                    0.0 + position.x,
                    30.0 + position.y,
                    0.0,
                ),
            ));
        }
    }
}

fn despawn_tooltip(
    out: On<Pointer<Out>>,
    tooltips: Query<Entity, With<Tooltip>>,
    mut commands: Commands,
) {
    // Despawn any tooltip attached to this entity
    for entity in tooltips.iter() {
        commands.entity(entity).despawn();
    }
}

fn move_camera(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut query: Query<(&mut Transform, &mut Projection), With<Camera2d>>,
    time: Res<Time>,
) {
    let Ok((mut transform, mut projection)) = query.single_mut() else { todo!() };
    let mut direction = Vec2::ZERO;

    if keyboard_input.pressed(KeyCode::ArrowUp) {
        // direction.y += 1.0;
    }
    if keyboard_input.pressed(KeyCode::ArrowDown) {
        // direction.y -= 1.0;
    }
    if keyboard_input.pressed(KeyCode::ArrowLeft) {
        direction.x -= 1.0;
    }
    if keyboard_input.pressed(KeyCode::ArrowRight) {
        direction.x += 1.0;
    }

    let speed = 1200.0;
    transform.translation += direction.extend(0.0) * speed * time.delta_secs();

    let zoom_speed = 2.0;

    // if let Projection::Orthographic(projection2d) = &mut *projection {
    //     // Zoom Out (Increase scale to see more)
    //     if keyboard_input.pressed(KeyCode::KeyZ) {
    //         projection2d.scale += zoom_speed * time.delta_secs();
    //     }

    //     // Zoom In (Decrease scale to see less)
    //     if keyboard_input.pressed(KeyCode::KeyX) {
    //         projection2d.scale -= zoom_speed * time.delta_secs();
    //     }
    //     projection2d.scale = projection2d.scale.clamp(0.1, 10.0);
    // }
}

fn zoom_samples(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut query: Query<(&mut Transform, &mut TrueScale)>,
    time: Res<Time>,
) {

    if keyboard_input.pressed(KeyCode::KeyZ) {
        for mut sample in &mut query {
            sample.1.scale -= 0.001 * time.delta_secs();
            sample.0.translation.x = -X_EXTENT + (sample.1.x + sample.1.width/2) as f32 * sample.1.scale;
            sample.0.scale.x = sample.1.width as f32 * sample.1.scale;
        }
    }

    if keyboard_input.pressed(KeyCode::KeyX) {
        for mut sample in &mut query {
            sample.1.scale += 0.001 * time.delta_secs();
            sample.0.translation.x = -X_EXTENT + (sample.1.x + sample.1.width/2) as f32 * sample.1.scale;
            sample.0.scale.x = sample.1.width as f32 * sample.1.scale;
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = fs::read_to_string("perf.json")?;
    let v: PerfData = serde_json::from_str(&input)?;
    let version_str = format!("version: {}", v.linux_perf_json_version);
    // to_graph(v.samples);
    // to_flamegraph(v.samples);
    // println!("{}",version_str);

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                resolution: WindowResolution::new(X_EXTENT as u32 *2.0 as u32, Y_EXTENT as u32 *2.0 as u32).with_scale_factor_override(1.0),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(MeshPickingPlugin)
        .add_systems(Startup, setup)
        .add_systems(Update, (move_camera, zoom_samples)).run();


    return Ok(());
}
