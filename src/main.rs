use serde::{Deserialize, Serialize};
use std::fs;
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

const SAMPLE_SCALE: f32 = 0.001;
const ROW_HEIGHT: f32 = 20.0;
const BRICK_HEIGHT: f32 = 18.0;

const TARGET_PID: usize = 64477;
const TARGET_TID: usize = 64477;

/// The deepest `depth` frames of a callchain (innermost frames), or `None`
/// when the callchain is shallower than `depth`.
fn frames_at_depth(callchain: &[CallFrame], depth: usize) -> Option<&[CallFrame]> {
    if callchain.len() >= depth {
        Some(&callchain[callchain.len() - depth..])
    } else {
        None
    }
}

/// Two stacks form the same brick when their frames share the same instruction
/// pointers in order.
fn frames_match(a: &[CallFrame], b: &[CallFrame]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.ip == y.ip)
}

/// Spawn a single flamegraph brick for `frame` at the given row (`depth`),
/// horizontal offset (`x`) and `width`.
fn spawn_brick(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<ColorMaterial>,
    frame: &CallFrame,
    x: usize,
    width: usize,
    depth: usize,
) {
    let rect_mesh = meshes.add(Rectangle::default());
    let color = Color::hsl((x + depth % 360) as f32, 0.33, 0.44);

    let mut transform = Transform::from_xyz(
        -X_EXTENT + (x + width / 2) as f32 * SAMPLE_SCALE,
        -Y_EXTENT + depth as f32 * ROW_HEIGHT,
        0.0,
    );
    transform.scale = Vec3::new(width as f32 * SAMPLE_SCALE, BRICK_HEIGHT, 1.0);

    commands
        .spawn((
            Mesh2d(rect_mesh),
            MeshMaterial2d(materials.add(color)),
            transform,
            TrueScale { x, width, scale: SAMPLE_SCALE },
            HoverText(frame.clone()),
        ))
        .observe(spawn_tooltip)
        .observe(despawn_tooltip);
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    commands.spawn(Camera2d::default());

    let input = fs::read_to_string("perf.json").unwrap();
    let perf: PerfData = serde_json::from_str(&input).unwrap();
    let samples = &perf.samples;
    let sample_len = samples.len();

    // Weight of each sample is the time until the following sample; the last
    // sample reuses the previous gap.
    let mut sample_weight: Vec<usize> = Vec::with_capacity(sample_len);
    for window in samples.windows(2) {
        sample_weight.push(window[1].timestamp - window[0].timestamp);
    }
    sample_weight.push(samples[sample_len - 1].timestamp - samples[sample_len - 2].timestamp);

    // Start offset of each sample relative to the first sample.
    let first_samp = samples[0].timestamp;
    let sample_start: Vec<usize> =
        samples.iter().map(|s| s.timestamp - first_samp).collect();

    let max_depth = samples
        .iter()
        .filter(|s| s.pid == TARGET_PID && s.tid == TARGET_TID)
        .map(|s| s.callchain.len())
        .max()
        .unwrap_or(0);

    // Build one row of bricks per stack depth. Within a row, consecutive
    // samples sharing the same stack are merged into a single wide brick.
    for depth in 1..=max_depth {
        let mut cur_frames: Option<Vec<CallFrame>> = None;
        let mut cur_start: usize = 0;
        let mut cur_weight: usize = 0;

        for (sample_index, sample) in samples.iter().enumerate() {
            if sample.pid != TARGET_PID || sample.tid != TARGET_TID {
                continue;
            }

            match frames_at_depth(&sample.callchain, depth) {
                Some(frames) => match &cur_frames {
                    // Same stack: extend the current brick to cover this sample.
                    Some(cur) if frames_match(cur, frames) => {
                        cur_weight += sample_start[sample_index] - (cur_start + cur_weight);
                        cur_weight += sample_weight[sample_index];
                    }
                    // Different stack: flush the current brick, start a new one.
                    Some(cur) => {
                        spawn_brick(
                            &mut commands, &mut meshes, &mut materials,
                            &cur[0], cur_start, cur_weight, depth,
                        );
                        cur_frames = Some(frames.to_vec());
                        cur_start = sample_start[sample_index];
                        cur_weight = sample_weight[sample_index];
                    }
                    // No brick in progress: start one.
                    None => {
                        cur_frames = Some(frames.to_vec());
                        cur_start = sample_start[sample_index];
                        cur_weight = sample_weight[sample_index];
                    }
                },
                // Sample is shallower than this row: flush any brick in progress.
                None => {
                    if let Some(cur) = &cur_frames {
                        spawn_brick(
                            &mut commands, &mut meshes, &mut materials,
                            &cur[0], cur_start, cur_weight, depth,
                        );
                        cur_frames = None;
                        cur_start = 0;
                        cur_weight = 0;
                    }
                }
            }
        }

        // Flush the final brick of the row.
        if let Some(cur) = &cur_frames {
            spawn_brick(
                &mut commands, &mut meshes, &mut materials,
                &cur[0], cur_start, cur_weight, depth,
            );
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
            commands.spawn((
                Text2d::new(text),
                Tooltip,
                Transform::from_xyz(position.x, 30.0 + position.y, 0.0),
            ));
        }
    }
}

fn despawn_tooltip(
    _out: On<Pointer<Out>>,
    tooltips: Query<Entity, With<Tooltip>>,
    mut commands: Commands,
) {
    for entity in tooltips.iter() {
        commands.entity(entity).despawn();
    }
}

fn move_camera(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut query: Query<&mut Transform, With<Camera2d>>,
    time: Res<Time>,
) {
    let Ok(mut transform) = query.single_mut() else {
        return;
    };

    let mut direction = Vec2::ZERO;
    if keyboard_input.pressed(KeyCode::ArrowLeft) {
        direction.x -= 1.0;
    }
    if keyboard_input.pressed(KeyCode::ArrowRight) {
        direction.x += 1.0;
    }

    let speed = 1200.0;
    transform.translation += direction.extend(0.0) * speed * time.delta_secs();
}

fn zoom_samples(
    keyboard_input: Res<ButtonInput<KeyCode>>,
    mut query: Query<(&mut Transform, &mut TrueScale)>,
    time: Res<Time>,
) {
    let zoom_delta = if keyboard_input.pressed(KeyCode::KeyZ) {
        -0.001 * time.delta_secs()
    } else if keyboard_input.pressed(KeyCode::KeyX) {
        0.001 * time.delta_secs()
    } else {
        return;
    };

    for (mut transform, mut scale) in &mut query {
        scale.scale += zoom_delta;
        transform.translation.x = -X_EXTENT + (scale.x + scale.width / 2) as f32 * scale.scale;
        transform.scale.x = scale.width as f32 * scale.scale;
    }
}

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                resolution: WindowResolution::new(X_EXTENT as u32 * 2, Y_EXTENT as u32 * 2)
                    .with_scale_factor_override(1.0),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(MeshPickingPlugin)
        .add_systems(Startup, setup)
        .add_systems(Update, (move_camera, zoom_samples))
        .run();
}
