//! End-to-end tests for `DynamicBsnLoader`: real `.bsn` text, served from an in-memory asset
//! source, loaded through a real `AssetServer` and spawned through the real scene pipeline.
//!
//! Everything here exercises the *asset* layer. Document lowering itself is covered by
//! `bevy_scene`'s unit tests in `src/dynamic/`.

extern crate alloc;

use alloc::sync::Arc;
use std::{path::Path, sync::Mutex};

use bevy_app::{App, TaskPoolPlugin, Update};
use bevy_asset::{
    io::{
        memory::{Dir, MemoryAssetReader},
        AssetSourceBuilder, AssetSourceId,
    },
    Asset, AssetApp, AssetLoadFailedEvent, AssetPlugin, AssetServer, Assets, Handle, LoadState,
};
use bevy_ecs::{
    hierarchy::Children,
    message::MessageReader,
    name::Name,
    prelude::{Component, Resource},
    reflect::{ReflectComponent, ReflectFromTemplate},
    system::Res,
    template::FromTemplate,
};
use bevy_reflect::{std_traits::ReflectDefault, Reflect};
use bevy_scene::{bsn, ScenePatch, ScenePatchInstance, ScenePlugin, WorldSceneExt};

/// A three-field component, so that partial patching is observable.
#[derive(Component, Reflect, Clone, Default, PartialEq, Debug)]
#[reflect(Component, Default)]
struct Position {
    x: f32,
    y: f32,
    z: f32,
}

/// A stand-in asset type, so that a `Handle` field has somewhere to point.
#[derive(Asset, Reflect, Default)]
struct Image;

/// A component with a handle field, for the asset-dependency test.
///
/// `#[template(reflect)]` generates a reflectable `SpriteTemplate` whose `image` field is a
/// `HandleTemplate<Image>`, which is what a string asset path converts into.
#[derive(Component, FromTemplate, Reflect, PartialEq, Debug)]
#[template(reflect)]
#[reflect(Component, FromTemplate)]
struct Sprite {
    image: Handle<Image>,
}

/// Builds an [`App`] serving `dir` as the default asset source.
///
/// The source must be registered *before* `AssetPlugin`, or the plugin logs an error and the
/// registration is ignored.
fn test_app(dir: &Dir) -> App {
    let mut app = App::new();
    let reader_dir = dir.clone();
    app.register_asset_source(
        AssetSourceId::Default,
        AssetSourceBuilder::new(move || {
            Box::new(MemoryAssetReader {
                root: reader_dir.clone(),
            })
        }),
    );
    app.add_plugins((
        TaskPoolPlugin::default(),
        AssetPlugin::default(),
        ScenePlugin,
    ));
    app.init_asset::<Image>();
    app.register_asset_reflect::<Image>();
    app.register_type::<Position>();
    app.register_type::<Sprite>();
    app.register_type::<SpriteTemplate>();
    app.register_type::<Name>();
    app.register_type::<Children>();
    app.finish();
    app.cleanup();
    app
}

/// Pumps `app` until `predicate` holds, or panics after a bounded number of frames.
///
/// The bound is what makes the "never resolves" failure modes (self-include, a missing base)
/// show up as a test failure rather than a hang.
fn run_app_until(app: &mut App, mut predicate: impl FnMut(&mut App) -> bool) {
    const MAX_FRAMES: usize = 10_000;
    for _ in 0..MAX_FRAMES {
        app.update();
        if predicate(app) {
            return;
        }
    }
    panic!("the app never reached the expected state");
}

/// Returns the asset server's rendered error for `handle`, or panics if it has not failed.
fn load_error(app: &App, handle: &Handle<ScenePatch>) -> String {
    let asset_server = app.world().resource::<AssetServer>();
    match asset_server.load_state(handle) {
        LoadState::Failed(error) => error.to_string(),
        other => panic!("expected the load to fail, but it is {other:?}"),
    }
}

/// Loads `path` and pumps the app until the load either succeeds or fails.
fn load_and_settle(app: &mut App, path: &'static str) -> Handle<ScenePatch> {
    let asset_server = app.world().resource::<AssetServer>().clone();
    let handle = asset_server.load::<ScenePatch>(path);
    let probe = handle.clone();
    run_app_until(app, |_| {
        !matches!(
            asset_server.load_state(&probe),
            LoadState::NotLoaded | LoadState::Loading
        )
    });
    handle
}

#[test]
fn loads_empty_bsn_file() {
    let dir = Dir::default();
    dir.insert_asset_text(Path::new("empty.bsn"), "// nothing but a comment\n");
    let mut app = test_app(&dir);

    let handle = load_and_settle(&mut app, "empty.bsn");
    assert!(
        matches!(
            app.world().resource::<AssetServer>().load_state(&handle),
            LoadState::Loaded
        ),
        "a comment-only `.bsn` file must load: {}",
        load_error(&app, &handle)
    );

    let patches = app.world().resource::<Assets<ScenePatch>>();
    assert!(
        patches.get(&handle).unwrap().resolved.is_some(),
        "the loaded patch must have been resolved by `resolve_scene_patches`"
    );
}

#[test]
fn spawns_scene_patch_instance_from_bsn() {
    let dir = Dir::default();
    dir.insert_asset_text(Path::new("a.bsn"), "Position { x: 1.0, y: 2.0 }");
    let mut app = test_app(&dir);

    // Spawn *before* the load finishes, so the entity goes through `WaitingScenes`.
    let asset_server = app.world().resource::<AssetServer>().clone();
    let handle = asset_server.load::<ScenePatch>("a.bsn");
    let id = app.world_mut().spawn(ScenePatchInstance(handle)).id();

    run_app_until(&mut app, |app| app.world().get::<Position>(id).is_some());

    assert_eq!(
        *app.world().get::<Position>(id).unwrap(),
        // `z` proves the patch is partial: it keeps the component's default.
        Position {
            x: 1.0,
            y: 2.0,
            z: 0.0
        }
    );
}

#[test]
fn spawns_children_and_names_from_bsn() {
    let dir = Dir::default();
    dir.insert_asset_text(
        Path::new("a.bsn"),
        "Children [ (#X), (#Y Position { x: 3.0 }) ]",
    );
    let mut app = test_app(&dir);

    let asset_server = app.world().resource::<AssetServer>().clone();
    let handle = asset_server.load::<ScenePatch>("a.bsn");
    let id = app.world_mut().spawn(ScenePatchInstance(handle)).id();

    run_app_until(&mut app, |app| app.world().get::<Children>(id).is_some());

    let children: Vec<_> = app.world().get::<Children>(id).unwrap().iter().collect();
    assert_eq!(children.len(), 2);
    assert_eq!(app.world().get::<Name>(*children[0]).unwrap().as_str(), "X");
    assert_eq!(app.world().get::<Name>(*children[1]).unwrap().as_str(), "Y");
    assert_eq!(
        app.world().get::<Position>(*children[1]).unwrap().x,
        3.0,
        "the second child's patch must be applied"
    );
}

#[test]
fn bsn_macro_inherits_from_bsn_asset() {
    let dir = Dir::default();
    dir.insert_asset_text(Path::new("a.bsn"), "Position { y: 2.0 }\nChildren [ (#X) ]");
    let mut app = test_app(&dir);

    let handle = load_and_settle(&mut app, "a.bsn");
    assert!(
        matches!(
            app.world().resource::<AssetServer>().load_state(&handle),
            LoadState::Loaded
        ),
        "{}",
        load_error(&app, &handle)
    );

    let world = app.world_mut();
    let id = world
        .spawn_scene(bsn! {
            :"a.bsn"
            Position { x: 1.0 }
            Children [ #Y ]
        })
        .unwrap()
        .id();

    assert_eq!(
        *world.get::<Position>(id).unwrap(),
        // `x` from the macro, `y` from the asset, `z` from neither: field-level merging.
        Position {
            x: 1.0,
            y: 2.0,
            z: 0.0
        }
    );

    let children: Vec<_> = world.get::<Children>(id).unwrap().iter().collect();
    assert_eq!(children.len(), 2);
    assert_eq!(world.get::<Name>(*children[0]).unwrap().as_str(), "X");
    assert_eq!(world.get::<Name>(*children[1]).unwrap().as_str(), "Y");
}

#[test]
fn nested_bsn_includes_resolve_base_first() {
    let dir = Dir::default();
    dir.insert_asset_text(Path::new("c.bsn"), "Position { z: 3.0 }");
    dir.insert_asset_text(Path::new("b.bsn"), ":\"c.bsn\"\nPosition { y: 2.0 }");
    dir.insert_asset_text(Path::new("a.bsn"), ":\"b.bsn\"\nPosition { x: 1.0 }");
    let mut app = test_app(&dir);

    let asset_server = app.world().resource::<AssetServer>().clone();
    let handle = asset_server.load::<ScenePatch>("a.bsn");
    let id = app.world_mut().spawn(ScenePatchInstance(handle)).id();

    run_app_until(&mut app, |app| app.world().get::<Position>(id).is_some());

    assert_eq!(
        *app.world().get::<Position>(id).unwrap(),
        Position {
            x: 1.0,
            y: 2.0,
            z: 3.0
        },
        "a three-level include chain must resolve base-first"
    );
}

#[test]
fn asset_path_field_becomes_dependency() {
    let dir = Dir::default();
    dir.insert_asset_text(Path::new("a.bsn"), "Sprite { image: \"logo.png\" }");
    dir.insert_asset_text(Path::new("logo.png"), "not really a png");
    let mut app = test_app(&dir);

    let handle = load_and_settle(&mut app, "a.bsn");
    let patches = app.world().resource::<Assets<ScenePatch>>();
    let patch = patches.get(&handle).expect("the patch must exist");
    assert!(
        !patch.dependencies.is_empty(),
        "a handle-valued field must become an asset dependency"
    );

    let asset_server = app.world().resource::<AssetServer>();
    assert!(
        asset_server.get_handle::<Image>("logo.png").is_some(),
        "the dependency must have been registered with the asset server"
    );
}

#[test]
fn parse_error_reports_line_and_column() {
    let dir = Dir::default();
    dir.insert_asset_text(Path::new("bad.bsn"), "Position {\n  x: ,\n}\n");
    let mut app = test_app(&dir);

    let handle = load_and_settle(&mut app, "bad.bsn");
    let error = load_error(&app, &handle);
    assert!(
        error.contains("bad.bsn:2:"),
        "the error must carry `path:line:`: {error}"
    );
}

#[test]
fn unregistered_type_reports_error_with_location() {
    let dir = Dir::default();
    dir.insert_asset_text(
        Path::new("a.bsn"),
        "Position { x: 1.0 }\nmy_game::NotRegistered",
    );
    let mut app = test_app(&dir);

    let handle = load_and_settle(&mut app, "a.bsn");
    let error = load_error(&app, &handle);
    assert!(
        error.contains("my_game::NotRegistered"),
        "the error must name the offending type: {error}"
    );
    assert!(
        error.contains("a.bsn:2:1"),
        "the error must carry `path:line:column`: {error}"
    );
}

#[test]
fn multiple_roots_rejected() {
    let dir = Dir::default();
    dir.insert_asset_text(
        Path::new("a.bsn"),
        "(Position { x: 1.0 }),\n(Position { y: 2.0 })",
    );
    let mut app = test_app(&dir);

    let handle = load_and_settle(&mut app, "a.bsn");
    let error = load_error(&app, &handle);
    assert!(
        error.contains("exactly one root entity"),
        "unexpected error: {error}"
    );
}

#[test]
fn spawn_after_load_applies_immediately() {
    let dir = Dir::default();
    dir.insert_asset_text(Path::new("a.bsn"), "Position { x: 1.0 }");
    let mut app = test_app(&dir);

    let handle = load_and_settle(&mut app, "a.bsn");
    assert!(app
        .world()
        .resource::<Assets<ScenePatch>>()
        .get(&handle)
        .unwrap()
        .resolved
        .is_some());

    // The asset is already resolved, so this entity never enters `WaitingScenes`.
    let id = app.world_mut().spawn(ScenePatchInstance(handle)).id();
    app.update();

    assert_eq!(app.world().get::<Position>(id).unwrap().x, 1.0);
}

#[test]
fn self_include_rejected() {
    let dir = Dir::default();
    dir.insert_asset_text(Path::new("a.bsn"), ":\"a.bsn\"\nPosition { x: 1.0 }");
    let mut app = test_app(&dir);

    // `load_and_settle` is bounded: a self-including file must fail fast rather than hang
    // waiting for its own recursive dependency state to become `Loaded`.
    let handle = load_and_settle(&mut app, "a.bsn");
    let error = load_error(&app, &handle);
    assert!(
        error.contains("inherits from itself"),
        "unexpected error: {error}"
    );
}

/// Collects every `AssetLoadFailedEvent<ScenePatch>` a test observes, across all frames.
#[derive(Resource, Default, Clone)]
struct ObservedFailures(Arc<Mutex<Vec<String>>>);

fn collect_failures(
    mut failures: MessageReader<AssetLoadFailedEvent<ScenePatch>>,
    observed: Res<ObservedFailures>,
) {
    let mut observed = observed.0.lock().unwrap();
    for failure in failures.read() {
        observed.push(failure.error.to_string());
    }
}

#[test]
fn failed_load_is_reported_once() {
    let dir = Dir::default();
    dir.insert_asset_text(Path::new("bad.bsn"), "Position {\n  x: ,\n}\n");
    let mut app = test_app(&dir);
    let observed = ObservedFailures::default();
    app.insert_resource(observed.clone());
    app.add_systems(Update, collect_failures);

    let handle = load_and_settle(&mut app, "bad.bsn");
    assert!(matches!(
        app.world().resource::<AssetServer>().load_state(&handle),
        LoadState::Failed(_)
    ));

    // Keep pumping: `Messages` retains events for two frames, but the reader's cursor means the
    // failure is seen — and therefore logged by `report_scene_patch_load_failures` — exactly once.
    for _ in 0..10 {
        app.update();
    }

    let observed = observed.0.lock().unwrap();
    assert_eq!(
        observed.len(),
        1,
        "expected exactly one load-failure event, got {observed:?}"
    );
    assert!(
        observed[0].contains("bad.bsn:"),
        "unexpected error: {}",
        observed[0]
    );
}
