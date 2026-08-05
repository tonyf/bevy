//! Demonstrates loading scenes from `.bsn` asset files.
//!
//! This example shows two ways to use a `.bsn` file:
//!
//! 1. Loading it directly as a [`ScenePatch`](bevy::scene::ScenePatch) asset and spawning it with
//!    [`ScenePatchInstance`].
//! 2. Inheriting from it inside a [`bsn!`] macro with `:"path.bsn"`, overriding individual fields.
//!
//! Note the difference in what the two forms accept: the `bsn!` macro below uses Rust function
//! calls (`px(24)`, `Color::srgb(...)`) and could use constants like `Color::WHITE`, while a
//! `.bsn` file — which is data, not code — supports neither, and spells the same values out as
//! struct literals (`Color::Srgba(Srgba { red: 0.95, ... })`).
//!
//! Requires the `bsn_asset` cargo feature (enabled by default).

use bevy::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn(Camera2d);

    // 1. Spawn the `.bsn` file directly. The entity gets its components as soon as the asset and
    //    all of its dependencies (here: the logo image) have loaded.
    commands.spawn(ScenePatchInstance(
        asset_server.load("scenes/dynamic_bsn_example.bsn"),
    ));

    // 2. Inherit from a `.bsn` file in Rust, overriding the background color and the label.
    //    `queue_spawn_scene` waits until "scenes/dynamic_bsn_button.bsn" is loaded. Fields the
    //    macro does not mention keep the values the asset gave them.
    commands.queue_spawn_scene(bsn! {
        :"scenes/dynamic_bsn_button.bsn"
        Node {
            position_type: PositionType::Absolute,
            bottom: px(24),
            right: px(24),
        }
        BackgroundColor(Color::srgb(0.15, 0.35, 0.15))
        Children [ (Text("Overridden!")) ]
    });
}
