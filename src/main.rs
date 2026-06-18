//! Bevy flamegraph viewer.
//!
//! Loads a profile (perf JSON, `perf script` text or Gecko) dropped onto the
//! window. It opens on a "drop a file here" prompt; once a file is dropped it
//! shows an overview of every thread's flame graph; pick one to inspect in
//! detail as a flame chart, left-heavy flame graph or top table. Dropping
//! another file at any time loads it in place.
//!
//! Controls:
//! * drop a file – load (or replace) the profile being viewed
//! * overview   – arrow keys or click select a thread; `Enter` / click opens it
//! * `Tab`      – cycle view: overview → flame chart → flame graph → top
//! * `Esc`      – return to the thread overview
//! * `S`        – in the top view, cycle the sort column
//! * `+` / `-`  – in the top view, grow / shrink the table font (also `X`/`Z`)
//! * arrow keys – pan the view (select a thread in the overview)
//! * `Z` / `X`  – zoom out / in (time axis)
//! * `C` / `V`  – zoom out / in (depth axis)
//! * `[` / `]`  – switch to the previous / next thread
//! * `/`        – in a detail view, search functions by name: type to highlight
//!   every matching brick / row, `Enter` keeps the highlight, `Esc` clears it
//! * hold `Alt`  – reveal the per-sample tick lines in the flame chart and read
//!   the timestamp of the tick nearest the cursor
//! * hover      – highlight the brick under the cursor and every brick that
//!   shares its symbol / instruction pointer, and show the function's self and
//!   total time (and event share)
//!
//! In the flame chart a faint vertical tick can mark each sample's timestamp,
//! but the ticks are hidden until `Alt` is held (which also annotates the tick
//! nearest the cursor with its timestamp). The chart's left edge is the
//! displayed thread's own first sample, so the header also reports how far into
//! the profile that thread actually started.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use bevy::{
    input::keyboard::{Key, KeyboardInput},
    input::ButtonState,
    prelude::*,
    sprite::Anchor,
    window::FileDragAndDrop,
    window::PrimaryWindow,
    window::WindowResolution,
};

use flamegraph_viewer::flame::{self, FunctionStat};
use flamegraph_viewer::parsers;
use flamegraph_viewer::profile::{Profile, Thread};

const X_EXTENT: f32 = 1600.0;
const Y_EXTENT: f32 = 900.0;

/// Default (and maximum) height of a flame-chart row. Deep threads use a
/// smaller row height so the whole tower fits on screen; shallow threads are
/// capped here so they are not stretched.
const ROW_HEIGHT: f32 = 20.0;
/// Fraction of a row a brick fills, leaving a small gap between rows.
const BRICK_FILL: f32 = 0.9;
/// Vertical padding kept clear above and below the flame chart when fitting a
/// thread's full depth into the viewport.
const VERTICAL_MARGIN: f32 = 80.0;

const LABEL_FONT_SIZE: f32 = 12.0;
/// Upper bound for brick label fonts: taller rows grow the text up to here so
/// it stays legible without overflowing the brick.
const MAX_LABEL_FONT: f32 = 24.0;
/// Fraction of a brick's height used for its label font, so taller rows render
/// larger, more readable text (see [`label_font_size`]).
const LABEL_FILL: f32 = 0.7;
/// Rough average glyph advance for the default font as a fraction of the font
/// size, used to estimate how many characters fit inside a brick.
const LABEL_CHAR_RATIO: f32 = 0.5;
const LABEL_PADDING: f32 = 2.0;

/// Bricks narrower than this many pixels at the initial fit scale are not
/// spawned: they are invisible yet would dominate the entity count (a deep
/// Gecko thread produces millions of sub-pixel slivers).
const MIN_BRICK_PX: f32 = 1.0;

/// Adjacent sample tick lines closer than this many pixels (at the initial fit
/// scale) are dropped, so a thread with very many samples still produces a
/// bounded, legible set of ticks.
const MIN_TICK_PX: f32 = 4.0;
/// Width of a sample tick line, in world units (~pixels at the default zoom).
const TICK_WIDTH: f32 = 1.5;
/// While Alt is held, the timestamp annotation snaps to the nearest tick within
/// this many world units of the cursor; further away, nothing is shown.
const TICK_SNAP_PX: f32 = 60.0;
/// Gap between a tick line and its Alt timestamp annotation.
const TICK_LABEL_PADDING: f32 = 4.0;

/// World-space margins reserved around the overview grid: a wider strip at the
/// top leaves room for the header readout.
const OVERVIEW_MARGIN: f32 = 60.0;
const OVERVIEW_TOP_MARGIN: f32 = 150.0;
/// Gap between adjacent overview cells, in world units.
const OVERVIEW_GAP: f32 = 16.0;
/// Thumbnails skip bricks narrower than this many world units (~pixels).
const OVERVIEW_MIN_BRICK_PX: f32 = 0.7;

/// Starting font size for the top-functions table, and the bounds the `+`/`-`
/// (or `X`/`Z`) keys may resize it within.
const TOP_FONT_SIZE: f32 = 13.0;
const MIN_TOP_FONT: f32 = 7.0;
const MAX_TOP_FONT: f32 = 40.0;

/// Text shown centred on the window while no profile is loaded.
const DROP_PROMPT: &str =
    "Drop a profile file here to open it\n\nperf script, perf JSON, or Firefox/Gecko JSON";

const PALETTE_SIZE: usize = 64;

/// Highlight colour for bricks (and top-table rows) whose function name matches
/// the active search query. Deliberately distinct from the hover colours
/// (white / orange) and the muted HSL palette so matches jump out.
const SEARCH_COLOR: Color = Color::srgb(1.0, 0.25, 0.85);

/// Default colour of a top-table row; matching rows switch to [`SEARCH_COLOR`].
const TOP_TEXT_COLOR: Color = Color::srgb(0.9, 0.9, 0.95);

/// The whole parsed profile.
/// The profile currently being viewed, or `None` before any file is dropped on
/// the window (the app opens on a "drop a file here" prompt).
#[derive(Resource)]
struct LoadedProfile(Option<Profile>);

/// Which thread is shown and the order to cycle through them.
#[derive(Resource)]
struct ThreadView {
    /// Thread indices, busiest first.
    order: Vec<usize>,
    cursor: usize,
}

/// How the current thread is visualised.
#[derive(Resource, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    /// Grid of every thread's flame graph; pick one to inspect in detail.
    Overview,
    /// Time-ordered flame chart (the x axis is wall-clock time).
    FlameChart,
    /// Left-heavy flame graph (stacks merged and sorted widest-first).
    FlameGraph,
    /// Flat `perf report`-style function table.
    Top,
}

impl ViewMode {
    fn next(self) -> Self {
        match self {
            ViewMode::Overview => ViewMode::FlameChart,
            ViewMode::FlameChart => ViewMode::FlameGraph,
            ViewMode::FlameGraph => ViewMode::Top,
            ViewMode::Top => ViewMode::Overview,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ViewMode::Overview => "overview (all threads)",
            ViewMode::FlameChart => "flame chart (time)",
            ViewMode::FlameGraph => "flame graph (left-heavy)",
            ViewMode::Top => "top functions",
        }
    }
}

/// Column the [`ViewMode::Top`] table is sorted by.
#[derive(Resource, Clone, Copy, PartialEq, Eq)]
enum TopSort {
    SelfTime,
    TotalTime,
    Name,
}

impl TopSort {
    fn next(self) -> Self {
        match self {
            TopSort::SelfTime => TopSort::TotalTime,
            TopSort::TotalTime => TopSort::Name,
            TopSort::Name => TopSort::SelfTime,
        }
    }

    fn label(self) -> &'static str {
        match self {
            TopSort::SelfTime => "self time",
            TopSort::TotalTime => "total time",
            TopSort::Name => "name",
        }
    }
}

/// GPU assets shared by every brick so rebuilding a thread does not allocate a
/// mesh and material per brick.
#[derive(Resource)]
struct SharedAssets {
    mesh: Handle<Mesh>,
    palette: Vec<Handle<ColorMaterial>>,
    /// Highlight for the brick directly under the cursor.
    hover_self: Handle<ColorMaterial>,
    /// Highlight for other bricks sharing the hovered symbol.
    hover_group: Handle<ColorMaterial>,
    /// Highlight for bricks whose function name matches the active search.
    search_match: Handle<ColorMaterial>,
    /// Faint overlay colour for the per-sample timestamp tick lines.
    tick: Handle<ColorMaterial>,
    /// Background panel behind each overview thumbnail.
    cell_bg: Handle<ColorMaterial>,
    /// Background panel behind the currently selected overview thumbnail.
    cell_selected: Handle<ColorMaterial>,
}

/// Per-function aggregates for the currently displayed thread, used by the info
/// panel and the top-functions table.
#[derive(Resource, Default)]
struct FuncStats {
    by_key: HashMap<String, FunctionStat>,
    rows: Vec<FunctionStat>,
    total_events: u64,
}

/// The brick (and symbol) currently under the cursor.
#[derive(Resource, Default)]
struct Hover {
    entity: Option<Entity>,
    key: Option<String>,
}

/// Live function-name search shared by the three detail views. While `active`
/// the typed query is captured character by character (and the normal keyboard
/// shortcuts are suppressed); any brick or top-table row whose function name
/// contains the query (case-insensitively) is highlighted.
#[derive(Resource, Default)]
struct Search {
    query: String,
    active: bool,
}

impl Search {
    /// The lower-cased needle to match against, or `None` when the query is
    /// empty (nothing highlighted).
    fn needle(&self) -> Option<String> {
        let trimmed = self.query.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_lowercase())
        }
    }

    fn clear(&mut self) {
        self.query.clear();
        self.active = false;
    }
}

/// True when `key` contains the search `needle` (already lower-cased).
fn matches_needle(key: &str, needle: &str) -> bool {
    key.to_lowercase().contains(needle)
}

/// Current flame-chart row height, fitted to the displayed thread's depth and
/// adjustable with the vertical-zoom keys.
#[derive(Resource)]
struct RowHeight(f32);

impl Default for RowHeight {
    fn default() -> Self {
        RowHeight(ROW_HEIGHT)
    }
}

/// Current horizontal zoom: world units per axis unit (nanoseconds for the time
/// chart, events for the left-heavy graph). Fitted to the view on rebuild and
/// scaled by the time-zoom keys. This is the horizontal companion of
/// [`RowHeight`] (vertical zoom); together they map a brick's profile-space
/// geometry to the screen. The factor is identical for every brick, label and
/// sample tick, so it lives here once instead of being copied onto each entity.
#[derive(Resource)]
struct Zoom(f32);

impl Default for Zoom {
    fn default() -> Self {
        Zoom(1.0)
    }
}

/// Current font size of the top-functions table, resizable in that view.
#[derive(Resource)]
struct TopFontSize(f32);

impl Default for TopFontSize {
    fn default() -> Self {
        TopFontSize(TOP_FONT_SIZE)
    }
}

/// Geometry of the overview grid, rebuilt with the view. Each cell's world-space
/// rectangle is stored at the same index its thread occupies in
/// [`ThreadView::order`], so the cursor and click hit-testing share one mapping.
#[derive(Resource, Default)]
struct OverviewLayout {
    cols: usize,
    cells: Vec<Rect>,
}

/// Marks every entity that belongs to the current flamegraph so a rebuild can
/// despawn them all.
#[derive(Component)]
struct FlamegraphEntity;

/// Per-brick data used for hit-testing, highlighting and the info panel.
#[derive(Component)]
struct BrickView {
    /// Symbol / ip identity shared by bricks of the same function.
    key: String,
    base_material: Handle<ColorMaterial>,
}

/// A brick's immutable layout record in profile units, kept so the entity can
/// be repositioned when the view is zoomed. Wraps the [`flame::Brick`] produced
/// by the layout directly instead of re-declaring its geometry; the current
/// zoom factor is *not* stored here but shared in the [`Zoom`] resource, since
/// it is identical for every brick.
#[derive(Component)]
struct BrickGeometry(flame::Brick);

/// Text drawn on top of a brick, with the geometry needed to reposition and
/// re-truncate it on zoom. The zoom factor is shared in the [`Zoom`] resource.
#[derive(Component)]
struct BrickLabel {
    full: String,
    start_ns: f64,
    width_ns: f64,
    depth: usize,
}

/// On-screen thread picker readout.
#[derive(Component)]
struct ThreadIndicator;

/// A vertical line marking the timestamp of one sample in the flame chart. Its
/// `offset_ns` (time since the thread's first sample) is kept so it can be
/// repositioned on time zoom, exactly like a brick, using the shared [`Zoom`].
/// `time_ns` is the same sample's time relative to the *profile* start, used for
/// the Alt annotation.
#[derive(Component)]
struct SampleTick {
    offset_ns: f64,
    time_ns: f64,
}

/// The single floating label that, while `Alt` is held, reports the timestamp of
/// the tick nearest the cursor.
#[derive(Component)]
struct TickAnnotation;

/// Marks the top-functions table text so its font size can be adjusted in place.
#[derive(Component)]
struct TopTable;

/// One row of the top-functions table, carrying its function key so the search
/// highlighter can recolour matching rows independently.
#[derive(Component)]
struct TopRow {
    key: String,
}

/// On-screen panel showing the clicked symbol's timing.
#[derive(Component)]
struct InfoPanel;

/// Search readout shown in the detail views: the current query and how many
/// functions match it. Hidden in the overview.
#[derive(Component)]
struct SearchBar;

/// Centred prompt shown while no profile is loaded, inviting the user to drop a
/// file onto the window. Doubles as the place load errors are reported.
#[derive(Component)]
struct DropPrompt;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "flamegraph-viewer".to_string(),
                resolution: WindowResolution::new(X_EXTENT as u32 * 2, Y_EXTENT as u32 * 2)
                    .with_scale_factor_override(1.0),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(LoadedProfile(None))
        .insert_resource(ThreadView {
            order: Vec::new(),
            cursor: 0,
        })
        .insert_resource(ViewMode::Overview)
        .insert_resource(TopSort::SelfTime)
        .init_resource::<FuncStats>()
        .init_resource::<Hover>()
        .init_resource::<Search>()
        .init_resource::<RowHeight>()
        .init_resource::<Zoom>()
        .init_resource::<TopFontSize>()
        .init_resource::<OverviewLayout>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                handle_file_drop,
                search_input,
                switch_thread,
                toggle_view,
                overview_input,
                rebuild_flamegraph,
                move_camera,
                zoom_samples,
                zoom_ticks,
                resize_top_font,
                update_hover,
                recolor_bricks,
                recolor_top_rows,
                toggle_sample_ticks,
                update_tick_annotation,
                update_info_panel,
                update_search_bar,
                update_chrome_visibility,
            )
                .chain(),
        )
        .run();
}

fn setup(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
) {
    commands.spawn(Camera2d);

    let mesh = meshes.add(Rectangle::default());
    let palette: Vec<Handle<ColorMaterial>> = (0..PALETTE_SIZE)
        .map(|i| {
            let hue = i as f32 / PALETTE_SIZE as f32 * 360.0;
            materials.add(Color::hsl(hue, 0.45, 0.5))
        })
        .collect();
    let hover_self = materials.add(Color::srgb(1.0, 1.0, 1.0));
    let hover_group = materials.add(Color::srgb(1.0, 0.75, 0.2));
    let search_match = materials.add(SEARCH_COLOR);
    // Translucent so the bricks beneath a tick stay visible through it.
    let tick = materials.add(Color::srgba(0.85, 0.9, 1.0, 0.08));
    let cell_bg = materials.add(Color::srgb(0.12, 0.12, 0.15));
    let cell_selected = materials.add(Color::srgb(0.28, 0.34, 0.5));

    commands.insert_resource(SharedAssets {
        mesh,
        palette,
        hover_self,
        hover_group,
        search_match,
        tick,
        cell_bg,
        cell_selected,
    });

    let panel_font = TextFont::from_font_size(16.0);

    commands.spawn((
        Text::new(String::new()),
        panel_font.clone(),
        TextColor(Color::WHITE),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            left: Val::Px(8.0),
            ..default()
        },
        ThreadIndicator,
    ));

    commands.spawn((
        Text::new("Hover a brick to see its timing"),
        panel_font,
        TextColor(Color::srgb(0.85, 0.85, 0.9)),
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6)),
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            right: Val::Px(8.0),
            padding: UiRect::all(Val::Px(6.0)),
            ..default()
        },
        InfoPanel,
    ));

    // Search readout, shown only in the detail views (see `update_search_bar`).
    commands.spawn((
        Text::new(String::new()),
        TextFont::from_font_size(16.0),
        TextColor(Color::srgb(0.95, 0.85, 0.95)),
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6)),
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(8.0),
            left: Val::Px(8.0),
            padding: UiRect::all(Val::Px(6.0)),
            ..default()
        },
        Visibility::Hidden,
        SearchBar,
    ));

    // A single reusable label, shown only while Alt is held, that snaps to the
    // sample tick nearest the cursor (see `update_tick_annotation`). It lives
    // for the whole session rather than being rebuilt with the flamegraph.
    commands.spawn((
        Text2d::new(String::new()),
        TextFont::from_font_size(13.0),
        TextColor(Color::srgb(0.95, 0.97, 1.0)),
        Anchor::BOTTOM_LEFT,
        Transform::from_xyz(0.0, 0.0, 3.0),
        Visibility::Hidden,
        TickAnnotation,
    ));

    // Full-screen container that centres the drop prompt; the prompt text itself
    // carries the marker so its visibility can be toggled and load errors written
    // to it (see `update_chrome_visibility` and `handle_file_drop`).
    commands
        .spawn(Node {
            position_type: PositionType::Absolute,
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            ..default()
        })
        .with_children(|parent| {
            parent.spawn((
                Text::new(DROP_PROMPT),
                TextFont::from_font_size(28.0),
                TextColor(Color::srgb(0.85, 0.87, 0.92)),
                TextLayout::new_with_justify(Justify::Center),
                DropPrompt,
            ));
        });
}

/// Cycle the displayed thread with the `[` and `]` keys.
fn switch_thread(
    keyboard: Res<ButtonInput<KeyCode>>,
    search: Res<Search>,
    mut view: ResMut<ThreadView>,
) {
    if search.active {
        return;
    }
    let count = view.order.len();
    if count < 2 {
        return;
    }
    if keyboard.just_pressed(KeyCode::BracketRight) {
        view.cursor = (view.cursor + 1) % count;
    } else if keyboard.just_pressed(KeyCode::BracketLeft) {
        view.cursor = (view.cursor + count - 1) % count;
    }
}

/// `Tab` cycles the view mode, `Esc` jumps back to the overview, and `S` cycles
/// the top-table sort order.
fn toggle_view(
    keyboard: Res<ButtonInput<KeyCode>>,
    search: Res<Search>,
    mut mode: ResMut<ViewMode>,
    mut sort: ResMut<TopSort>,
) {
    if search.active {
        return;
    }
    if keyboard.just_pressed(KeyCode::Tab) {
        *mode = mode.next();
    }
    // Esc returns to the thread overview from any detailed view.
    if keyboard.just_pressed(KeyCode::Escape) && *mode != ViewMode::Overview {
        *mode = ViewMode::Overview;
    }
    // The sort key only matters while the table is on screen.
    if keyboard.just_pressed(KeyCode::KeyS) && *mode == ViewMode::Top {
        *sort = sort.next();
    }
}

/// Capture the function-name search query in the detail views. `/` opens the
/// search (re-opening keeps the current query for editing); typing edits it,
/// `Backspace` deletes, `Enter` commits (keeping the highlight) and `Esc`
/// clears it. While the search is active the normal keyboard shortcuts are
/// suppressed (every shortcut system early-returns on `search.active`), and the
/// `Esc` that clears the search is consumed so it does not also return to the
/// overview in the same frame.
fn search_input(
    mut events: MessageReader<KeyboardInput>,
    mut keyboard: ResMut<ButtonInput<KeyCode>>,
    mode: Res<ViewMode>,
    mut search: ResMut<Search>,
) {
    for event in events.read() {
        // Always drain the queue, but only act on the per-thread detail views;
        // this keeps a keypress made in the overview from leaking into a view
        // opened later in the same frame.
        if *mode == ViewMode::Overview || event.state != ButtonState::Pressed {
            continue;
        }
        if apply_search_key(&mut search, &classify_key(event)) == SearchAction::ConsumeEscape {
            // Stop the same Esc from also returning to the overview this frame.
            keyboard.clear_just_pressed(KeyCode::Escape);
        }
    }
}

/// A keypress reduced to the cases the search box cares about.
#[derive(Debug, PartialEq, Eq)]
enum SearchKey {
    /// The `/` that opens the box (or a literal slash once it is open).
    Slash,
    Escape,
    Enter,
    Backspace,
    /// Printable text produced by the key (e.g. a letter, digit or space).
    Text(String),
    /// Anything else (modifiers, arrows, …): ignored by the search box.
    Other,
}

/// What the caller must do after [`apply_search_key`] processed a key.
#[derive(Debug, PartialEq, Eq)]
enum SearchAction {
    None,
    /// The key cleared the search; the `Esc` must be consumed so it does not
    /// also trigger the return-to-overview shortcut.
    ConsumeEscape,
}

/// Reduce a keyboard event to the [`SearchKey`] the search box reacts to.
fn classify_key(event: &KeyboardInput) -> SearchKey {
    match &event.logical_key {
        Key::Escape => SearchKey::Escape,
        Key::Enter => SearchKey::Enter,
        Key::Backspace => SearchKey::Backspace,
        key if is_slash(key) => SearchKey::Slash,
        _ => match &event.text {
            Some(text) => SearchKey::Text(text.to_string()),
            None => SearchKey::Other,
        },
    }
}

/// Apply one classified keypress to the search state (already known to be in a
/// detail view). Returns whether the `Esc` that cleared the search must be
/// consumed by the caller.
fn apply_search_key(search: &mut Search, key: &SearchKey) -> SearchAction {
    if !search.active {
        // While the box is closed only `/` matters; every other key keeps its
        // normal shortcut behaviour.
        if *key == SearchKey::Slash {
            search.active = true;
        }
        return SearchAction::None;
    }

    match key {
        SearchKey::Escape => {
            search.clear();
            return SearchAction::ConsumeEscape;
        }
        SearchKey::Enter => search.active = false,
        SearchKey::Backspace => {
            search.query.pop();
        }
        // An open box treats `/` as a literal character to search for.
        SearchKey::Slash => search.query.push('/'),
        SearchKey::Text(text) => {
            for c in text.chars() {
                if !c.is_control() {
                    search.query.push(c);
                }
            }
        }
        SearchKey::Other => {}
    }
    SearchAction::None
}

/// Whether a logical key is the `/` that opens the search box.
fn is_slash(key: &Key) -> bool {
    matches!(key, Key::Character(s) if s.as_str() == "/")
}

/// Initial scale (world units per axis unit) that fits a layout `width` units
/// wide across the view. The unit is nanoseconds for the time layout and events
/// for the left-heavy graph.
fn fit_scale(width: f64) -> f32 {
    let width = (width as f32).max(1.0);
    2.0 * X_EXTENT / width
}

/// Row height that fits a thread's full stack depth into the viewport, capped
/// at [`ROW_HEIGHT`] so shallow threads keep a comfortable size.
fn fit_row_height(thread: &Thread) -> f32 {
    let max_depth = thread.max_depth().max(1) as f32;
    let available = 2.0 * Y_EXTENT - VERTICAL_MARGIN;
    (available / max_depth).min(ROW_HEIGHT)
}

/// Rebuild the view whenever the selected thread, view mode or sort changes
/// (including the initial build, since the resources are freshly added).
#[allow(clippy::too_many_arguments)] // Bevy systems take their data as params.
fn rebuild_flamegraph(
    mut commands: Commands,
    view: Res<ThreadView>,
    mode: Res<ViewMode>,
    sort: Res<TopSort>,
    profile: Res<LoadedProfile>,
    shared: Res<SharedAssets>,
    top_font: Res<TopFontSize>,
    mut stats: ResMut<FuncStats>,
    mut hover: ResMut<Hover>,
    mut search: ResMut<Search>,
    mut row_height: ResMut<RowHeight>,
    mut zoom: ResMut<Zoom>,
    mut overview: ResMut<OverviewLayout>,
    existing: Query<Entity, With<FlamegraphEntity>>,
    mut indicator: Query<&mut Text, With<ThreadIndicator>>,
    mut cameras: Query<&mut Transform, With<Camera2d>>,
) {
    if !view.is_changed() && !mode.is_changed() && !sort.is_changed() {
        return;
    }

    for entity in &existing {
        commands.entity(entity).despawn();
    }
    hover.entity = None;
    hover.key = None;

    // Recentre the camera so each rebuilt view starts framed, regardless of any
    // panning done in the previous view.
    if let Ok(mut camera) = cameras.single_mut() {
        camera.translation.x = 0.0;
        camera.translation.y = 0.0;
    }

    let Some(loaded) = profile.0.as_ref() else {
        // No profile yet: leave the window empty behind the drop prompt.
        if let Ok(mut text) = indicator.single_mut() {
            text.0 = String::new();
        }
        return;
    };

    let Some(&thread_idx) = view.order.get(view.cursor) else {
        return;
    };
    let thread = &loaded.threads[thread_idx];

    let rows = flame::flat_profile(thread);
    stats.by_key = rows.iter().cloned().map(|r| (r.key.clone(), r)).collect();
    stats.total_events = thread.event_count();
    stats.rows = rows;

    let profile_start = loaded.start_ns().unwrap_or(0.0);
    let needle = search.needle();
    match *mode {
        ViewMode::Overview => {
            // The search bar and highlight belong to the detail views; leaving
            // to the overview drops the query so re-opening a thread is clean.
            if !search.query.is_empty() || search.active {
                search.clear();
            }
            spawn_overview(&mut commands, &shared, loaded, &view, &mut overview);
        }
        ViewMode::FlameChart | ViewMode::FlameGraph => {
            // Fit both zoom axes to the thread, then spawn; the returned fit
            // scale becomes the shared horizontal zoom (`RowHeight` is the
            // vertical one).
            row_height.0 = fit_row_height(thread);
            zoom.0 = spawn_bricks(
                &mut commands,
                &shared,
                row_height.0,
                thread,
                *mode == ViewMode::FlameGraph,
                profile_start,
                needle.as_deref(),
            );
        }
        ViewMode::Top => spawn_top_table(&mut commands, &stats, *sort, top_font.0, needle.as_deref()),
    }

    if let Ok(mut text) = indicator.single_mut() {
        text.0 = if *mode == ViewMode::Overview {
            format!(
                "Overview — {} threads  ·  selected: {}\n\
                 arrows or click select a thread  ·  Enter / click opens it  ·  Tab cycles views",
                view.order.len(),
                thread.label(),
            )
        } else {
            let sort_hint = if *mode == ViewMode::Top {
                format!("  ·  +/- font ({:.0}px)  ·  S sort ({})", top_font.0, sort.label())
            } else {
                String::new()
            };
            format!(
                "Thread {}/{}: {}  ({} samples, {} events, depth {})\n\
                 {}\n\
                 view: {}  ·  Tab switch view{}\n\
                 [ ] thread  ·  arrows pan  ·  Z/X time zoom  ·  C/V depth zoom  ·  / search  ·  hold Alt: sample ticks + timestamps",
                view.cursor + 1,
                view.order.len(),
                thread.label(),
                thread.samples.len(),
                thread.event_count(),
                thread.max_depth(),
                thread_timing_summary(loaded, thread),
                mode.label(),
                sort_hint,
            )
        };
    }
}

/// Load a profile when a file is dropped on the window. On success this swaps in
/// the new [`LoadedProfile`], resets the thread selection to the overview (which
/// triggers a rebuild) and retitles the window; on failure it reports the error
/// in the drop prompt and leaves the current state untouched.
fn handle_file_drop(
    mut drops: MessageReader<FileDragAndDrop>,
    mut profile: ResMut<LoadedProfile>,
    mut view: ResMut<ThreadView>,
    mut mode: ResMut<ViewMode>,
    mut windows: Query<&mut Window, With<PrimaryWindow>>,
    mut prompt: Query<&mut Text, With<DropPrompt>>,
) {
    // Only the last dropped file matters; act on it once the batch is drained.
    let Some(path) = drops.read().find_map(|event| match event {
        FileDragAndDrop::DroppedFile { path_buf, .. } => Some(path_buf.clone()),
        _ => None,
    }) else {
        return;
    };

    let result = parsers::load(&path).and_then(|profile| {
        if profile.is_empty() {
            Err(format!("{} contains no samples", path.display()).into())
        } else {
            Ok(profile)
        }
    });

    match result {
        Ok(loaded) => {
            *view = ThreadView {
                order: loaded.threads_by_samples(),
                cursor: 0,
            };
            *mode = ViewMode::Overview;
            *profile = LoadedProfile(Some(loaded));
            if let Ok(mut window) = windows.single_mut() {
                window.title = format!("flamegraph-viewer — {}", path.display());
            }
        }
        Err(err) => {
            if let Ok(mut text) = prompt.single_mut() {
                text.0 = format!("Could not open {}\n\n{err}\n\n{DROP_PROMPT}", path.display());
            }
        }
    }
}

/// Show the drop prompt (and hide the chart chrome) while no profile is loaded,
/// and the reverse once one is. Runs only when [`LoadedProfile`] changes.
fn update_chrome_visibility(
    profile: Res<LoadedProfile>,
    mut prompt: Query<
        &mut Visibility,
        (With<DropPrompt>, Without<InfoPanel>, Without<ThreadIndicator>),
    >,
    mut info: Query<
        &mut Visibility,
        (With<InfoPanel>, Without<DropPrompt>, Without<ThreadIndicator>),
    >,
    mut indicator: Query<
        &mut Visibility,
        (With<ThreadIndicator>, Without<DropPrompt>, Without<InfoPanel>),
    >,
) {
    if !profile.is_changed() {
        return;
    }
    let loaded = profile.0.is_some();
    let chrome = if loaded {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    if let Ok(mut v) = prompt.single_mut() {
        *v = if loaded {
            Visibility::Hidden
        } else {
            Visibility::Visible
        };
    }
    if let Ok(mut v) = info.single_mut() {
        *v = chrome;
    }
    if let Ok(mut v) = indicator.single_mut() {
        *v = chrome;
    }
}

/// One-line description of where the thread sits on the profile timeline: how
/// long it ran and, when it did not begin at the very start of the capture, how
/// far into the profile its first sample was taken. The flame chart's left edge
/// is always the thread's *own* first sample, so this is the cue that the thread
/// did not actually start when the profile did.
fn thread_timing_summary(profile: &Profile, thread: &Thread) -> String {
    let span = format_ns(thread.span_ns());
    match (profile.start_ns(), thread.first_timestamp_ns()) {
        (Some(profile_start), Some(thread_start)) if thread_start - profile_start > 0.0 => {
            format!(
                "spans {span}  ·  first sample +{} after profile start (profile spans {})",
                format_ns(thread_start - profile_start),
                format_ns(profile.span_ns()),
            )
        }
        _ => format!("spans {span}  ·  starts at profile start"),
    }
}

/// Render every thread as a full-width time-ordered flame-chart preview, one per
/// row stacked top-to-bottom, highlighting the currently selected row. All rows
/// share one global time axis so equal wall-clock times line up vertically. Also
/// records each cell's world rectangle in [`OverviewLayout`] so the keyboard and
/// mouse selection systems agree on where each thread is.
fn spawn_overview(
    commands: &mut Commands,
    shared: &SharedAssets,
    profile: &Profile,
    view: &ThreadView,
    layout: &mut OverviewLayout,
) {
    let n = view.order.len();
    layout.cells.clear();
    layout.cols = 0;
    if n == 0 {
        return;
    }

    // One full-width row per thread, stacked top-to-bottom (a single column).
    let cols = 1usize;
    let rows = n;
    layout.cols = cols;

    // A shared global time axis so a sample at a given wall-clock time lines up
    // vertically across every thread's row.
    let profile_start = profile.start_ns().unwrap_or(0.0);
    let profile_span = profile.span_ns();

    let left = -X_EXTENT + OVERVIEW_MARGIN;
    let top = Y_EXTENT - OVERVIEW_TOP_MARGIN;
    let area_w = 2.0 * X_EXTENT - 2.0 * OVERVIEW_MARGIN;
    let area_h = (Y_EXTENT - OVERVIEW_TOP_MARGIN) - (-Y_EXTENT + OVERVIEW_MARGIN);
    let cell_w = area_w;
    let cell_h = area_h / rows as f32;

    for (slot, &thread_idx) in view.order.iter().enumerate() {
        let col = slot % cols;
        let row = slot / cols;
        let center = Vec2::new(
            left + (col as f32 + 0.5) * cell_w,
            top - (row as f32 + 0.5) * cell_h,
        );
        layout
            .cells
            .push(Rect::from_center_size(center, Vec2::new(cell_w, cell_h)));

        let selected = slot == view.cursor;
        let bg = if selected {
            shared.cell_selected.clone()
        } else {
            shared.cell_bg.clone()
        };
        let mut bg_transform = Transform::from_xyz(center.x, center.y, -1.0);
        bg_transform.scale = Vec3::new(cell_w - OVERVIEW_GAP, cell_h - OVERVIEW_GAP, 1.0);
        commands.spawn((
            Mesh2d(shared.mesh.clone()),
            MeshMaterial2d(bg),
            bg_transform,
            FlamegraphEntity,
        ));

        let inner_w = cell_w - 2.0 * OVERVIEW_GAP;
        let inner_h = cell_h - 2.0 * OVERVIEW_GAP;
        let label_h = (inner_h * 0.18).min(26.0);

        spawn_thumbnail(
            commands,
            shared,
            &profile.threads[thread_idx],
            center,
            inner_w,
            inner_h - label_h,
            profile_start,
            profile_span,
        );

        // Thread label across the top of the cell.
        let label = format!(
            "{}  ({})",
            &profile.threads[thread_idx].label(),
            profile.threads[thread_idx].samples.len(),
        );
        commands.spawn((
            Text2d::new(truncate_name(&label, (inner_w / 8.0) as usize)),
            TextFont::from_font_size(12.0),
            TextColor(if selected {
                Color::WHITE
            } else {
                Color::srgb(0.7, 0.72, 0.78)
            }),
            Anchor::TOP_LEFT,
            Transform::from_xyz(center.x - inner_w / 2.0, center.y + inner_h / 2.0, 1.0),
            FlamegraphEntity,
        ));
    }
}

/// Draw one thread's time-ordered flame chart scaled to fit a thumbnail centred
/// at `center`, occupying `width` × `height` world units (the flame grows up from
/// the bottom of that box). This matches the detailed flame chart a click opens,
/// so the thumbnail and the opened view share the same time-series layout.
///
/// `width` maps the whole profile timeline (`profile_start` .. `profile_start +
/// profile_span`), so every row shares one x scale: a thread that begins later
/// starts further right, and equal wall-clock times line up vertically across
/// rows. Sub-pixel bricks are dropped to bound the entity count for busy threads.
fn spawn_thumbnail(
    commands: &mut Commands,
    shared: &SharedAssets,
    thread: &Thread,
    center: Vec2,
    width: f32,
    height: f32,
    profile_start: f64,
    profile_span: f64,
) {
    if profile_span <= 0.0 || width <= 0.0 {
        return;
    }
    // Shared time-to-pixel scale: one thumbnail pixel is this many ns, used both
    // to cull sub-pixel bricks up front and to place every row on one axis.
    let xscale = width / profile_span as f32;
    let min_width_ns = OVERVIEW_MIN_BRICK_PX as f64 / xscale as f64;

    let bricks = flame::layout(thread, min_width_ns);
    let max_depth = bricks.iter().map(|b| b.depth).max().unwrap_or(0);
    if max_depth == 0 {
        return;
    }

    // Offset of this thread's first sample from the profile start; brick
    // `start_ns` values are relative to that first sample (see `flame::layout`).
    let thread_offset = thread.first_timestamp_ns().unwrap_or(profile_start) - profile_start;

    let row_h = height / max_depth as f32;
    let flame_left = center.x - width / 2.0;
    let flame_bottom = center.y - height / 2.0;

    for brick in &bricks {
        let bw = brick.width_ns as f32 * xscale;
        if bw < OVERVIEW_MIN_BRICK_PX {
            continue;
        }
        let center_ns = thread_offset + brick.start_ns + brick.width_ns / 2.0;
        let bx = flame_left + center_ns as f32 * xscale;
        let by = flame_bottom + (brick.depth as f32 - 0.5) * row_h;
        let material = shared.palette[palette_index(thread.key(brick.frame))].clone();
        let mut transform = Transform::from_xyz(bx, by, 0.0);
        transform.scale = Vec3::new(bw, row_h * 0.92, 1.0);
        commands.spawn((
            Mesh2d(shared.mesh.clone()),
            MeshMaterial2d(material),
            transform,
            FlamegraphEntity,
        ));
    }
}

/// Lay out the current thread as bricks, either time-ordered ([`flame::layout`])
/// or left-heavy ([`flame::left_heavy`]), and spawn them. Both layouts share the
/// same brick geometry; only the x-axis unit differs (time vs events), which is
/// absorbed into the fit scale. Returns that fit scale so the caller can store
/// it as the initial [`Zoom`]; `row_height` is the already-fitted [`RowHeight`].
fn spawn_bricks(
    commands: &mut Commands,
    shared: &SharedAssets,
    row_height: f32,
    thread: &Thread,
    left_heavy: bool,
    profile_start: f64,
    needle: Option<&str>,
) -> f32 {
    let bricks = if left_heavy {
        flame::left_heavy(thread)
    } else {
        // Cull sub-pixel bricks up front so huge profiles never materialise an
        // unbounded brick list (only possible for the time layout).
        let scale = fit_scale(thread.span_ns());
        flame::layout(thread, (MIN_BRICK_PX / scale) as f64)
    };

    // Fit the whole layout across the view: its total width is the last brick's
    // right edge (time span, or total events for the left-heavy graph). This is
    // the shared zoom every brick, label and tick is positioned with.
    let width: f64 = bricks
        .iter()
        .map(|b| b.start_ns + b.width_ns)
        .fold(0.0, f64::max);
    let scale = fit_scale(width);

    for brick in bricks {
        if (brick.width_ns as f32 * scale) < MIN_BRICK_PX {
            continue;
        }
        let key = thread.key(brick.frame);
        spawn_brick(commands, shared, brick, key, scale, row_height, needle);
    }

    // Overlay a tick at each sample's timestamp on the time-ordered chart so it
    // is clear when each stack was actually captured. The left-heavy graph has
    // no time axis, so ticks would be meaningless there.
    if !left_heavy {
        spawn_ticks(commands, shared, thread, scale, profile_start);
    }
    scale
}

/// World-space x of a sample taken `offset_ns` after the thread's first sample.
fn tick_x(offset_ns: f64, scale: f32) -> f32 {
    -X_EXTENT + offset_ns as f32 * scale
}

/// Spawn a faint full-height line at each sample's timestamp. Ticks that would
/// land within [`MIN_TICK_PX`] of the previous one are skipped so a thread with
/// a great many samples stays both legible and bounded in entity count.
fn spawn_ticks(
    commands: &mut Commands,
    shared: &SharedAssets,
    thread: &Thread,
    scale: f32,
    profile_start: f64,
) {
    let Some(first_ts) = thread.first_timestamp_ns() else {
        return;
    };

    let mut last_x = f32::NEG_INFINITY;
    for sample in &thread.samples {
        let offset = sample.timestamp_ns - first_ts;
        let x = tick_x(offset, scale);
        if x - last_x < MIN_TICK_PX {
            continue;
        }
        last_x = x;

        let mut transform = Transform::from_xyz(x, 0.0, 0.5);
        transform.scale = Vec3::new(TICK_WIDTH, 2.0 * Y_EXTENT, 1.0);
        commands.spawn((
            Mesh2d(shared.mesh.clone()),
            MeshMaterial2d(shared.tick.clone()),
            transform,
            // Hidden by default; shown only while Alt is held (see
            // `toggle_sample_ticks`), so the chart stays uncluttered.
            Visibility::Hidden,
            SampleTick {
                offset_ns: offset,
                time_ns: sample.timestamp_ns - profile_start,
            },
            FlamegraphEntity,
        ));
    }
}

/// Header row shared by every sort order of the top-functions table.
const TOP_TABLE_HEADER: &str = "  self%   self time   total time  function\n";

/// Spawn the flat function table for [`ViewMode::Top`]. The header is the root
/// text and each function is a child [`TextSpan`] tagged with its key, so the
/// search highlighter can recolour matching rows in place ([`recolor_top_rows`])
/// without rebuilding the table. Rows matching the active search are spawned
/// already highlighted.
fn spawn_top_table(
    commands: &mut Commands,
    stats: &FuncStats,
    sort: TopSort,
    font_size: f32,
    needle: Option<&str>,
) {
    let font = TextFont::from_font_size(font_size);
    commands
        .spawn((
            Text::new(TOP_TABLE_HEADER),
            font.clone(),
            TextColor(TOP_TEXT_COLOR),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(112.0),
                left: Val::Px(8.0),
                ..default()
            },
            TopTable,
            FlamegraphEntity,
        ))
        .with_children(|parent| {
            for row in top_table_rows(stats, sort) {
                let color = if needle.is_some_and(|n| matches_needle(&row.key, n)) {
                    SEARCH_COLOR
                } else {
                    TOP_TEXT_COLOR
                };
                parent.spawn((
                    TextSpan::new(format_top_row(stats, &row)),
                    font.clone(),
                    TextColor(color),
                    TopTable,
                    TopRow { key: row.key },
                ));
            }
        });
}

/// Up to 40 of the hottest functions, ordered by the requested column. This is
/// the view that points straight at the code worth optimising.
fn top_table_rows(stats: &FuncStats, sort: TopSort) -> Vec<FunctionStat> {
    let mut rows = stats.rows.clone();
    match sort {
        TopSort::SelfTime => rows.sort_by(|a, b| b.self_ns.total_cmp(&a.self_ns)),
        TopSort::TotalTime => rows.sort_by(|a, b| b.total_ns.total_cmp(&a.total_ns)),
        TopSort::Name => rows.sort_by(|a, b| a.key.cmp(&b.key)),
    }
    rows.truncate(40);
    rows
}

/// Format one aligned table line (with trailing newline) for a function row.
fn format_top_row(stats: &FuncStats, row: &FunctionStat) -> String {
    let pct = if stats.total_events == 0 {
        0.0
    } else {
        row.self_events as f64 / stats.total_events as f64 * 100.0
    };
    format!(
        "  {:>5.1}  {:>10}  {:>11}  {}\n",
        pct,
        format_ns(row.self_ns),
        format_ns(row.total_ns),
        truncate_name(&row.key, 70),
    )
}

/// Truncate an over-long symbol so the table stays readable.
fn truncate_name(name: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if name.chars().count() <= max {
        return name.to_string();
    }
    if max == 1 {
        return name.chars().take(1).collect();
    }
    let kept: String = name.chars().take(max - 1).collect();
    format!("{kept}…")
}

/// Spawn one brick (and its label when it fits) using the shared assets. Takes
/// the layout `brick` by value and stores it on the entity as [`BrickGeometry`];
/// `scale` is the current [`Zoom`] used to place it now.
fn spawn_brick(
    commands: &mut Commands,
    shared: &SharedAssets,
    brick: flame::Brick,
    key: &str,
    scale: f32,
    row_height: f32,
    needle: Option<&str>,
) {
    let base_material = shared.palette[palette_index(key)].clone();
    // Bricks spawned while a search is active start already highlighted, so the
    // change-detecting recolour system never has to touch them on rebuild.
    let display_material = if needle.is_some_and(|n| matches_needle(key, n)) {
        shared.search_match.clone()
    } else {
        base_material.clone()
    };

    let mut transform = Transform::from_xyz(
        brick_center_x(brick.start_ns, brick.width_ns, scale),
        brick_y(brick.depth, row_height),
        0.0,
    );
    transform.scale = Vec3::new(
        brick.width_ns as f32 * scale,
        brick_thickness(row_height),
        1.0,
    );

    // Copy out the geometry the label needs before the brick is moved onto the
    // brick entity.
    let (start_ns, width_ns, depth) = (brick.start_ns, brick.width_ns, brick.depth);

    commands.spawn((
        Mesh2d(shared.mesh.clone()),
        MeshMaterial2d(display_material),
        transform,
        BrickGeometry(brick),
        BrickView {
            key: key.to_string(),
            base_material,
        },
        FlamegraphEntity,
    ));

    let full = key.to_string();
    // A label entity is created whenever the brick is wide enough to hold text;
    // whether the text is actually shown also depends on the row being tall
    // enough (see `labels_visible`), so depth-zooming can reveal it later.
    let font_size = label_font_size(row_height);
    let fitted = fit_label(&full, width_ns, scale, font_size);
    if fitted.is_empty() {
        return;
    }
    let text = if labels_visible(row_height) {
        fitted
    } else {
        String::new()
    };
    commands.spawn((
        Text2d::new(text),
        TextFont::from_font_size(font_size),
        Anchor::CENTER_LEFT,
        Transform::from_xyz(
            label_left_x(start_ns, scale),
            brick_y(depth, row_height),
            1.0,
        ),
        BrickLabel {
            full,
            start_ns,
            width_ns,
            depth,
        },
        FlamegraphEntity,
    ));
}

/// World-space y of a brick's centre for a given 1-based `depth`.
fn brick_y(depth: usize, row_height: f32) -> f32 {
    -Y_EXTENT + depth as f32 * row_height
}

/// Height of a brick: most of the row, leaving a small inter-row gap.
fn brick_thickness(row_height: f32) -> f32 {
    row_height * BRICK_FILL
}

/// Labels are only legible when the row is at least as tall as the font, so a
/// 12px label is not smeared across a dozen sub-pixel rows in a deep thread.
fn labels_visible(row_height: f32) -> bool {
    brick_thickness(row_height) >= LABEL_FONT_SIZE
}

/// Brick label font size, scaled to the brick height so taller rows get larger,
/// more legible text, bounded by [`LABEL_FONT_SIZE`] and [`MAX_LABEL_FONT`].
fn label_font_size(row_height: f32) -> f32 {
    (brick_thickness(row_height) * LABEL_FILL).clamp(LABEL_FONT_SIZE, MAX_LABEL_FONT)
}

/// World-space x of a brick's centre.
fn brick_center_x(start_ns: f64, width_ns: f64, scale: f32) -> f32 {
    -X_EXTENT + (start_ns + width_ns / 2.0) as f32 * scale
}

/// World-space x of a brick's left edge plus label padding.
fn label_left_x(start_ns: f64, scale: f32) -> f32 {
    -X_EXTENT + start_ns as f32 * scale + LABEL_PADDING
}

/// Stable palette slot for a symbol so the same function keeps one colour.
fn palette_index(key: &str) -> usize {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    (hasher.finish() % PALETTE_SIZE as u64) as usize
}

/// Truncate `full` to fit a brick `width_ns` wide at `scale`, appending ".."
/// when characters are dropped. Empty when not even one character fits.
fn fit_label(full: &str, width_ns: f64, scale: f32, font_size: f32) -> String {
    let usable = width_ns as f32 * scale - 2.0 * LABEL_PADDING;
    let char_width = font_size * LABEL_CHAR_RATIO;
    let max_chars = (usable / char_width).floor();
    if max_chars < 1.0 {
        return String::new();
    }

    let max_chars = max_chars as usize;
    if full.chars().count() <= max_chars {
        return full.to_string();
    }
    if max_chars <= 2 {
        return full.chars().take(max_chars).collect();
    }
    let mut truncated: String = full.chars().take(max_chars - 2).collect();
    truncated.push_str("..");
    truncated
}

fn move_camera(
    keyboard: Res<ButtonInput<KeyCode>>,
    mode: Res<ViewMode>,
    search: Res<Search>,
    mut query: Query<&mut Transform, With<Camera2d>>,
    time: Res<Time>,
) {
    // In the overview the arrow keys select a thread instead of panning, and
    // while typing a search query they edit the query instead.
    if *mode == ViewMode::Overview || search.active {
        return;
    }
    let Ok(mut transform) = query.single_mut() else {
        return;
    };

    let mut direction = Vec2::ZERO;
    if keyboard.pressed(KeyCode::ArrowLeft) {
        direction.x -= 1.0;
    }
    if keyboard.pressed(KeyCode::ArrowRight) {
        direction.x += 1.0;
    }
    if keyboard.pressed(KeyCode::ArrowUp) {
        direction.y += 1.0;
    }
    if keyboard.pressed(KeyCode::ArrowDown) {
        direction.y -= 1.0;
    }

    let speed = 1200.0;
    transform.translation += direction.extend(0.0) * speed * time.delta_secs();
}

/// In the overview, move the selection with the arrow keys and open the selected
/// thread on Enter or a left click inside its cell. Selecting a different cell
/// changes [`ThreadView::cursor`], which triggers a rebuild and moves the
/// highlight; opening switches to the detailed flame chart.
fn overview_input(
    mut mode: ResMut<ViewMode>,
    keyboard: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform)>,
    layout: Res<OverviewLayout>,
    mut view: ResMut<ThreadView>,
) {
    if *mode != ViewMode::Overview {
        return;
    }
    let count = view.order.len();
    if count == 0 {
        return;
    }
    let cols = layout.cols.max(1) as isize;

    let mut cursor = view.cursor as isize;
    if keyboard.just_pressed(KeyCode::ArrowRight) {
        cursor += 1;
    }
    if keyboard.just_pressed(KeyCode::ArrowLeft) {
        cursor -= 1;
    }
    if keyboard.just_pressed(KeyCode::ArrowDown) {
        cursor += cols;
    }
    if keyboard.just_pressed(KeyCode::ArrowUp) {
        cursor -= cols;
    }
    let cursor = cursor.clamp(0, count as isize - 1) as usize;
    if cursor != view.cursor {
        view.cursor = cursor;
    }

    let mut open = keyboard.any_just_pressed([
        KeyCode::Enter,
        KeyCode::NumpadEnter,
        KeyCode::Space,
    ]);

    if buttons.just_pressed(MouseButton::Left) {
        let clicked = windows
            .single()
            .ok()
            .zip(cameras.single().ok())
            .and_then(|(window, (camera, camera_transform))| {
                cursor_world(window, camera, camera_transform)
            })
            .and_then(|point| layout.cells.iter().position(|cell| cell.contains(point)));
        if let Some(slot) = clicked {
            view.cursor = slot;
            open = true;
        }
    }

    if open {
        *mode = ViewMode::FlameChart;
    }
}

fn zoom_samples(
    keyboard: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    search: Res<Search>,
    mut zoom: ResMut<Zoom>,
    mut row_height: ResMut<RowHeight>,
    mut bricks: Query<(&mut Transform, &BrickGeometry), Without<BrickLabel>>,
    mut labels: Query<(&mut Transform, &mut Text2d, &mut TextFont, &BrickLabel), Without<BrickGeometry>>,
) {
    if search.active {
        return;
    }
    let rate = 1.5 * time.delta_secs();
    let time_factor = axis_factor(&keyboard, KeyCode::KeyX, KeyCode::KeyZ, rate);
    let depth_factor = axis_factor(&keyboard, KeyCode::KeyV, KeyCode::KeyC, rate);

    if time_factor != 1.0 {
        zoom.0 *= time_factor;
    }
    if depth_factor != 1.0 {
        row_height.0 *= depth_factor;
    }
    if time_factor == 1.0 && depth_factor == 1.0 {
        return;
    }

    let scale = zoom.0;
    let row_height = row_height.0;
    let show_labels = labels_visible(row_height);
    let font_size = label_font_size(row_height);
    for (mut transform, geometry) in &mut bricks {
        let brick = &geometry.0;
        transform.translation.x = brick_center_x(brick.start_ns, brick.width_ns, scale);
        transform.scale.x = brick.width_ns as f32 * scale;
        transform.translation.y = brick_y(brick.depth, row_height);
        transform.scale.y = brick_thickness(row_height);
    }

    for (mut transform, mut text, mut font, label) in &mut labels {
        transform.translation.x = label_left_x(label.start_ns, scale);
        transform.translation.y = brick_y(label.depth, row_height);
        font.font_size = font_size;
        text.0 = if show_labels {
            fit_label(&label.full, label.width_ns, scale, font_size)
        } else {
            String::new()
        };
    }
}

/// Keep the sample-timestamp ticks aligned with the bricks as the time axis is
/// zoomed. Ticks span the full height, so vertical zoom never touches them; they
/// only need repositioning when the shared [`Zoom`] changed.
fn zoom_ticks(zoom: Res<Zoom>, mut ticks: Query<(&mut Transform, &SampleTick)>) {
    if !zoom.is_changed() {
        return;
    }
    for (mut transform, tick) in &mut ticks {
        transform.translation.x = tick_x(tick.offset_ns, zoom.0);
    }
}

/// Resize the top-functions table while it is on screen. `+`/`-` (or `X`/`Z`,
/// which are otherwise idle in this view) grow and shrink the font; the size is
/// clamped to a legible range and applied to the live table in place.
fn resize_top_font(
    keyboard: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mode: Res<ViewMode>,
    search: Res<Search>,
    mut size: ResMut<TopFontSize>,
    mut tables: Query<&mut TextFont, With<TopTable>>,
) {
    if *mode != ViewMode::Top || search.active {
        return;
    }
    let rate = 24.0 * time.delta_secs();
    let grow = keyboard.any_pressed([KeyCode::Equal, KeyCode::NumpadAdd, KeyCode::KeyX]);
    let shrink = keyboard.any_pressed([KeyCode::Minus, KeyCode::NumpadSubtract, KeyCode::KeyZ]);
    let delta = match (grow, shrink) {
        (true, false) => rate,
        (false, true) => -rate,
        _ => return,
    };

    let next = (size.0 + delta).clamp(MIN_TOP_FONT, MAX_TOP_FONT);
    if next == size.0 {
        return;
    }
    size.0 = next;
    for mut font in &mut tables {
        font.font_size = next;
    }
}

/// Multiplicative zoom factor for one axis: `> 1` while `grow` is held, `< 1`
/// while `shrink` is held, `1.0` (no change) otherwise.
fn axis_factor(
    keyboard: &ButtonInput<KeyCode>,
    grow: KeyCode,
    shrink: KeyCode,
    rate: f32,
) -> f32 {
    if keyboard.pressed(grow) {
        1.0 + rate
    } else if keyboard.pressed(shrink) {
        1.0 / (1.0 + rate)
    } else {
        1.0
    }
}

/// World-space cursor position, if the cursor is inside the window.
fn cursor_world(window: &Window, camera: &Camera, camera_transform: &GlobalTransform) -> Option<Vec2> {
    let cursor = window.cursor_position()?;
    camera.viewport_to_world_2d(camera_transform, cursor).ok()
}

/// The topmost brick containing `point`: rows do not overlap, so ties are
/// resolved by preferring the narrowest (deepest) brick.
fn brick_at<'a>(
    point: Vec2,
    bricks: impl Iterator<Item = (Entity, &'a Transform, &'a BrickView)>,
) -> Option<(Entity, &'a str)> {
    let mut best: Option<(Entity, &str, f32)> = None;
    for (entity, transform, view) in bricks {
        let half = transform.scale.truncate() / 2.0;
        let center = transform.translation.truncate();
        if (point.x - center.x).abs() <= half.x && (point.y - center.y).abs() <= half.y {
            let width = transform.scale.x;
            if best.is_none_or(|(_, _, w)| width < w) {
                best = Some((entity, view.key.as_str(), width));
            }
        }
    }
    best.map(|(entity, key, _)| (entity, key))
}

/// Track which brick (and symbol) the cursor is over, recording it in [`Hover`]
/// so the colouring system and info panel can react. Only the hover *state* is
/// updated here; [`recolor_bricks`] turns it (and the search) into colours.
fn update_hover(
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform)>,
    mut hover: ResMut<Hover>,
    bricks: Query<(Entity, &Transform, &BrickView)>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let Ok((camera, camera_transform)) = cameras.single() else {
        return;
    };

    let hovered = cursor_world(window, camera, camera_transform)
        .and_then(|point| brick_at(point, bricks.iter()).map(|(entity, key)| (entity, key.to_string())));

    let (hovered_entity, hovered_key) = match hovered {
        Some((entity, key)) => (Some(entity), Some(key)),
        None => (None, None),
    };

    if hovered_entity == hover.entity && hovered_key == hover.key {
        return;
    }
    hover.entity = hovered_entity;
    hover.key = hovered_key;
}

/// Colour every brick from the current hover and search state: the hovered brick
/// is white, bricks sharing its symbol are orange, bricks matching the search
/// query are [`SEARCH_COLOR`], and the rest keep their palette colour. Runs only
/// when the hover or the search changed, so steady-state panning/zooming is free
/// (freshly spawned bricks already carry the right search colour, see
/// [`spawn_brick`]).
fn recolor_bricks(
    hover: Res<Hover>,
    search: Res<Search>,
    shared: Res<SharedAssets>,
    mut bricks: Query<(Entity, &BrickView, &mut MeshMaterial2d<ColorMaterial>)>,
) {
    if !hover.is_changed() && !search.is_changed() {
        return;
    }
    let needle = search.needle();
    for (entity, view, mut material) in &mut bricks {
        let desired = brick_material(entity, view, &hover, needle.as_deref(), &shared);
        if material.0 != desired {
            material.0 = desired;
        }
    }
}

/// The material a brick should display given the hover and search state.
fn brick_material(
    entity: Entity,
    view: &BrickView,
    hover: &Hover,
    needle: Option<&str>,
    shared: &SharedAssets,
) -> Handle<ColorMaterial> {
    match brick_highlight(entity, &view.key, hover, needle) {
        BrickHighlight::Hovered => shared.hover_self.clone(),
        BrickHighlight::Group => shared.hover_group.clone(),
        BrickHighlight::Search => shared.search_match.clone(),
        BrickHighlight::Base => view.base_material.clone(),
    }
}

/// How a brick should be highlighted, in descending priority: the brick under
/// the cursor, a brick sharing the hovered symbol, a brick matching the search
/// query, otherwise its plain palette colour.
#[derive(Debug, PartialEq, Eq)]
enum BrickHighlight {
    Hovered,
    Group,
    Search,
    Base,
}

/// Decide a brick's highlight from the hover and search state. Hover (the
/// momentary, interactive cue) takes precedence over the search match so the
/// pointer feedback is never lost, and the search match takes precedence over
/// the plain palette colour.
fn brick_highlight(
    entity: Entity,
    key: &str,
    hover: &Hover,
    needle: Option<&str>,
) -> BrickHighlight {
    if hover.entity == Some(entity) {
        BrickHighlight::Hovered
    } else if hover.key.as_deref() == Some(key) {
        BrickHighlight::Group
    } else if needle.is_some_and(|n| matches_needle(key, n)) {
        BrickHighlight::Search
    } else {
        BrickHighlight::Base
    }
}

/// Recolour the top-table rows whose function name matches the search query.
/// Like the bricks, rows are spawned already highlighted (see [`spawn_top_table`]),
/// so this only has to react to the query changing while the table is on screen.
fn recolor_top_rows(search: Res<Search>, mut rows: Query<(&TopRow, &mut TextColor)>) {
    if !search.is_changed() {
        return;
    }
    let needle = search.needle();
    for (row, mut color) in &mut rows {
        let desired = if needle.as_deref().is_some_and(|n| matches_needle(&row.key, n)) {
            SEARCH_COLOR
        } else {
            TOP_TEXT_COLOR
        };
        if color.0 != desired {
            color.0 = desired;
        }
    }
}

/// Show the search query and how many functions in the current thread match it.
/// The bar is hidden in the overview (search applies to the detail views only).
fn update_search_bar(
    search: Res<Search>,
    stats: Res<FuncStats>,
    mode: Res<ViewMode>,
    mut bar: Query<(&mut Text, &mut Visibility), With<SearchBar>>,
) {
    if !search.is_changed() && !stats.is_changed() && !mode.is_changed() {
        return;
    }
    let Ok((mut text, mut visibility)) = bar.single_mut() else {
        return;
    };

    let visible = *mode != ViewMode::Overview;
    *visibility = if visible {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    if !visible {
        return;
    }

    text.0 = match search.needle() {
        Some(needle) => {
            let matches = stats
                .rows
                .iter()
                .filter(|row| matches_needle(&row.key, &needle))
                .count();
            let plural = if matches == 1 { "" } else { "s" };
            let hint = if search.active {
                "Enter keep · Esc clear"
            } else {
                "/ edit"
            };
            let cursor = if search.active { "_" } else { "" };
            format!(
                "Search: {}{}   {matches} matching function{plural}   ({hint})",
                search.query, cursor,
            )
        }
        None if search.active => "Search: _   (type a function name · Esc cancel)".to_string(),
        None => "Press / to search functions".to_string(),
    };
}

/// Sample tick lines are only shown while `Alt` is held — the same modifier that
/// reveals per-sample timestamps — so the chart stays uncluttered by default.
fn toggle_sample_ticks(
    keyboard: Res<ButtonInput<KeyCode>>,
    mut ticks: Query<&mut Visibility, With<SampleTick>>,
) {
    let target = if keyboard.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]) {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };
    for mut visibility in &mut ticks {
        if *visibility != target {
            *visibility = target;
        }
    }
}

/// While `Alt` is held, snap the floating [`TickAnnotation`] label to the sample
/// tick nearest the cursor and show that sample's profile-relative timestamp.
/// Released (or when no tick is close), the label is hidden.
#[allow(clippy::type_complexity)] // Bevy query filters read worse as aliases.
fn update_tick_annotation(
    keyboard: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform)>,
    ticks: Query<(&Transform, &SampleTick), Without<TickAnnotation>>,
    mut annotation: Query<
        (&mut Transform, &mut Text2d, &mut Visibility),
        (With<TickAnnotation>, Without<SampleTick>),
    >,
) {
    let Ok((mut transform, mut text, mut visibility)) = annotation.single_mut() else {
        return;
    };

    fn hide(v: &mut Visibility) {
        if *v != Visibility::Hidden {
            *v = Visibility::Hidden;
        }
    }

    if !keyboard.any_pressed([KeyCode::AltLeft, KeyCode::AltRight]) {
        hide(&mut visibility);
        return;
    }

    let cursor = windows
        .single()
        .ok()
        .zip(cameras.single().ok())
        .and_then(|(window, (camera, camera_transform))| {
            cursor_world(window, camera, camera_transform)
        });
    let Some(cursor) = cursor else {
        hide(&mut visibility);
        return;
    };

    // Nearest tick by horizontal distance; ticks are full-height, so only x
    // matters. Ignore ticks further than [`TICK_SNAP_PX`] so pointing at empty
    // space shows nothing.
    let nearest = ticks
        .iter()
        .map(|(t, tick)| ((t.translation.x - cursor.x).abs(), t.translation.x, tick.time_ns))
        .min_by(|a, b| a.0.total_cmp(&b.0));

    match nearest {
        Some((dist, x, time_ns)) if dist <= TICK_SNAP_PX => {
            text.0 = format_ns(time_ns);
            transform.translation.x = x + TICK_LABEL_PADDING;
            transform.translation.y = cursor.y;
            if *visibility != Visibility::Visible {
                *visibility = Visibility::Visible;
            }
        }
        _ => hide(&mut visibility),
    }
}

/// Update the info panel with the hovered function's time and event share.
fn update_info_panel(
    hover: Res<Hover>,
    stats: Res<FuncStats>,
    mut panel: Query<&mut Text, With<InfoPanel>>,
) {
    if !hover.is_changed() {
        return;
    }
    let Ok(mut text) = panel.single_mut() else {
        return;
    };
    let pct = |events: u64| {
        if stats.total_events == 0 {
            0.0
        } else {
            events as f64 / stats.total_events as f64 * 100.0
        }
    };
    match hover
        .key
        .as_deref()
        .and_then(|key| stats.by_key.get(key).map(|stat| (key, stat)))
    {
        Some((key, stat)) => {
            text.0 = format!(
                "{key}\n\
                 self:  {} ({:.1}%, {} ev)\n\
                 total: {} ({:.1}%, {} ev)",
                format_ns(stat.self_ns),
                pct(stat.self_events),
                stat.self_events,
                format_ns(stat.total_ns),
                pct(stat.total_events),
                stat.total_events,
            );
        }
        None => {
            text.0 = String::from("Hover a brick to see its timing");
        }
    }
}

/// Human-readable duration from nanoseconds.
fn format_ns(ns: f64) -> String {
    if ns >= 1.0e9 {
        format!("{:.3} s", ns / 1.0e9)
    } else if ns >= 1.0e6 {
        format!("{:.3} ms", ns / 1.0e6)
    } else if ns >= 1.0e3 {
        format!("{:.3} µs", ns / 1.0e3)
    } else {
        format!("{ns:.0} ns")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> SearchKey {
        SearchKey::Text(s.to_string())
    }

    fn feed(search: &mut Search, keys: &[SearchKey]) {
        for key in keys {
            apply_search_key(search, key);
        }
    }

    #[test]
    fn needle_is_trimmed_lowercased_and_empty_is_none() {
        let mut search = Search::default();
        assert_eq!(search.needle(), None);
        search.query = "  ".to_string();
        assert_eq!(search.needle(), None);
        search.query = "  MainLoop ".to_string();
        assert_eq!(search.needle().as_deref(), Some("mainloop"));
    }

    #[test]
    fn matches_needle_is_case_insensitive_substring() {
        assert!(matches_needle("std::vec::Vec::push", "push"));
        assert!(matches_needle("MyType::Render", "render"));
        assert!(!matches_needle("alloc", "free"));
    }

    #[test]
    fn slash_opens_box_without_being_typed() {
        let mut search = Search::default();
        // While closed, ordinary keys are ignored (they keep their shortcuts).
        feed(&mut search, &[text("x"), SearchKey::Backspace, SearchKey::Enter]);
        assert!(!search.active);
        assert_eq!(search.query, "");

        feed(&mut search, &[SearchKey::Slash]);
        assert!(search.active);
        assert_eq!(search.query, "", "the opening slash is not part of the query");
    }

    #[test]
    fn typing_editing_and_committing() {
        let mut search = Search::default();
        feed(
            &mut search,
            &[SearchKey::Slash, text("m"), text("a"), text("i"), text("n")],
        );
        assert_eq!(search.query, "main");

        feed(&mut search, &[SearchKey::Backspace]);
        assert_eq!(search.query, "mai");

        // Enter commits: the query (and its highlight) survive, input closes.
        feed(&mut search, &[SearchKey::Enter]);
        assert!(!search.active);
        assert_eq!(search.query, "mai");

        // Re-opening keeps the query for editing rather than starting over.
        feed(&mut search, &[SearchKey::Slash, text("n")]);
        assert!(search.active);
        assert_eq!(search.query, "main");
    }

    #[test]
    fn open_box_accepts_space_and_literal_slash_but_not_control_chars() {
        let mut search = Search::default();
        feed(
            &mut search,
            &[SearchKey::Slash, text("a"), SearchKey::Slash, text(" "), text("b")],
        );
        assert_eq!(search.query, "a/ b");

        let before = search.query.clone();
        feed(&mut search, &[text("\u{7}"), SearchKey::Other]);
        assert_eq!(search.query, before, "control chars and other keys are ignored");
    }

    #[test]
    fn escape_clears_and_is_consumed_only_when_active() {
        // Inactive: Escape is left for the return-to-overview shortcut.
        let mut search = Search::default();
        assert_eq!(apply_search_key(&mut search, &SearchKey::Escape), SearchAction::None);

        // Active: Escape clears the query and must be consumed.
        feed(&mut search, &[SearchKey::Slash, text("f"), text("o")]);
        assert_eq!(
            apply_search_key(&mut search, &SearchKey::Escape),
            SearchAction::ConsumeEscape
        );
        assert!(!search.active);
        assert_eq!(search.query, "");
    }

    #[test]
    fn brick_highlight_priority_orders_hover_above_search() {
        let mut world = World::new();
        let hovered = world.spawn_empty().id();
        let other = world.spawn_empty().id();

        // The hovered brick wins even when it also matches the search.
        let hover = Hover {
            entity: Some(hovered),
            key: Some("render".to_string()),
        };
        assert_eq!(
            brick_highlight(hovered, "render", &hover, Some("render")),
            BrickHighlight::Hovered
        );
        // Another brick sharing the hovered symbol is the group colour.
        assert_eq!(
            brick_highlight(other, "render", &hover, None),
            BrickHighlight::Group
        );

        // With nothing hovered, a search match is highlighted and the rest plain.
        let idle = Hover::default();
        assert_eq!(
            brick_highlight(other, "render", &idle, Some("ren")),
            BrickHighlight::Search
        );
        assert_eq!(
            brick_highlight(other, "parse", &idle, Some("ren")),
            BrickHighlight::Base
        );
        assert_eq!(
            brick_highlight(other, "parse", &idle, None),
            BrickHighlight::Base
        );
    }
}
