# SPEC-6: Hot Reload of `.bsn` Scenes & End-to-End Validation

**Status:** Draft, conforms to SPEC-0 (NORMATIVE master). Provides Contract G.
**Depends on:** SPEC-5 (`DynamicBsnLoader` + `bsn_asset` feature). Test matrix depends on
SPEC-3 (grammar) and SPEC-4 (resolution) landing first.
**Target:** `/home/tony/workspace/bevy`, Bevy `main` @ `25368b78c`, 0.20.0-dev.

> **Contract G is amended by this spec.** SPEC-0 §5 Contract G proposes a
> `ScenePatch::reload_source` retention field and explicitly authorizes SPEC-6 to
> "reconcile these two paths and pick the minimal correct one". This spec **kills
> `reload_source`** with justification in §4.1. No new field is added to `ScenePatch`.

---

## 1. Goals

- **G1.** Editing a `.bsn` file on disk, with asset watching enabled, updates every live
  entity spawned from that file — no restart, no leaked ghost entities.
- **G2.** Editing a *base* `.bsn` file updates live instances of every `.bsn` that inherits
  it via `:"base.bsn"`.
- **G3.** No new asset-level API surface: no new `ScenePatch` fields, no new resources, no
  new cargo features. Hot reload is a behavior of the existing
  `resolve_scene_patches` / `spawn_queued` pair.
- **G4.** Zero measurable cost on the immediate (`World::spawn_scene`) path; bounded,
  documented cost on the queued/instance path.
- **G5.** A validation plan proving the *whole* SPEC-1..6 series: a static→dynamic parity
  matrix, cross-feature (static↔dynamic) integration tests, hot-reload tests, a perf
  guardrail, and a manual QA checklist.

## 2. Non-goals

- **State-preserving reconciliation** (villor's `cart/bevy#36`). Hot reload here is
  *re-resolve + despawn scene-owned descendants + re-apply*; runtime mutations on scene
  entities are lost (SPEC-0 §2 / §6.7).
- **Removing components from a live root** that the edited `.bsn` no longer declares
  (§6.4). Needs bundle-write introspection; deferred with `cart/bevy#36`.
- **Hot reload of `SceneListPatch`** (`bsn_list!` / `queue_spawn_scene_list`): scene lists
  spawn N roots with no owning instance entity, so there is nothing to key re-application
  off.
- **Hot reload of immediately-spawned scenes** (`World::spawn_scene`,
  `EntityWorldMut::apply_scene`) — they never enter `Assets<ScenePatch>`
  (`spawn.rs:188-193`, `:503-508`) so they have no asset identity.
- **The copy-on-write overlap for in-code `bsn!` patches inheriting a `.bsn` base**
  (§4.5 Case C). Partial liveness only; boundary documented and tested.
- Editor `SceneDocument`, world→BSN write-back, binary formats (SPEC-0 §2).

---

## 3. Background — what actually happens today (all claims verified against code)

### 3.1 The reload event trace

Given `AssetPlugin { watch_for_changes_override: Some(true) }` (or the `file_watcher`
feature) and a change to `assets/player.bsn`:

1. **Frame N, `PreUpdate`, `handle_internal_asset_events`** (`server/mod.rs:2061`,
   registered in `PreUpdate` at `crates/bevy_asset/src/lib.rs:428-436`): the
   `AssetSourceEvent::ModifiedAsset(path)` branch (`server/mod.rs:2168`) calls
   `reload_path` → `AssetServer::reload_internal` (`:962`), spawning an `IoTaskPool` task
   that re-runs the loader for every handle at that path.
2. **Frame N+k, `PreUpdate`, same system**: the task's `InternalAssetEvent::Loaded` arrives
   and `AssetInfos::process_asset_load` (`server/info.rs:397`) runs:
   - `loaded_asset.value.insert(index, world)` (`info.rs:426`) →
     `Assets::insert_with_index` (`assets.rs:383-397`). The slot was occupied, so this
     pushes **`AssetEvent::Modified` into `Assets::queued_events`** — *not* into
     `Messages`. **The whole `ScenePatch` value is replaced** by whatever the loader
     returned: `ScenePatch { scene: Some(_), dependencies, resolved: None }`
     (`scene_patch.rs:41-53`).
   - All recursive deps are already `Loaded`, so the `(0, 0)` arm at `info.rs:489-497`
     sends `InternalAssetEvent::LoadedWithDependencies` on the **same channel** being
     drained by `try_iter`; it is picked up in the same loop and `sender(world, index)`
     (`server/mod.rs:2081` → `:206-210`) writes **`AssetEvent::LoadedWithDependencies`
     directly into `Messages<AssetEvent<ScenePatch>>`**.
3. **Frame N+k, `SpawnScene`** (order `First, PreUpdate, RunFixedMainLoop, Update,
   SpawnScene, PostUpdate, Last` — `crates/bevy_app/src/main_schedule.rs:224-231`;
   `ScenePlugin` puts `(resolve_scene_patches, spawn_queued).chain()` there,
   `crates/bevy_scene/src/lib.rs:946-957`):
   - `resolve_scene_patches` (`spawn.rs:607`) reads `LoadedWithDependencies` and
     `patches.get_mut(id).and_then(|mut p| p.scene.take())` (`:618`) **succeeds** — the
     reloaded value has a fresh `scene: Some(_)` — storing a fresh
     `Arc<ResolvedSceneRoot>` into `patch.resolved`.
   - `spawn_queued` (`:708`) reads the same event, gets the fresh `resolved`, then
     `waiting.scene_entities.remove(id)` (`:723`) → **`None`**: the instances left
     `waiting` on their first apply. Nothing happens.
4. **Frame N+k, `PostUpdate`**: `Assets::<ScenePatch>::asset_events`
   (`crates/bevy_asset/src/lib.rs:677-684`, `AssetEventSystems`) flushes the queued
   `Modified` into `Messages`.
5. **Frame N+k+1, `SpawnScene`**: `resolve_scene_patches` sees `Modified` → `_ => {}`
   (`:637`). `spawn_queued` ignores it too.

**Three consequences, all load-bearing for this design:**

- **F1.** `AssetEvent::LoadedWithDependencies` **does re-fire on every reload**. The
  existing `resolve_scene_patches` therefore *already* re-resolves reloaded `.bsn`
  assets, correctly, today. The only missing piece is re-application.
- **F2.** From `SpawnScene`'s point of view, `LoadedWithDependencies` arrives **one frame
  before** `Modified` for the same reload, because the former is written directly to
  `Messages` in `PreUpdate` while the latter is queued in `Assets` and flushed in
  `PostUpdate`. Any design that keys on `Modified` is both later and racier.
- **F3.** `Assets::get_mut` returns an `AssetMut` whose drop guard queues *another*
  `Modified` (`assets.rs:440-454`, `assets.rs:630-655`). So `resolve_scene_patches`
  currently emits a spurious `Modified` on **every** resolve, including first load. Any
  design keying re-application on `Modified` would re-apply immediately after the first
  load — a self-inflicted duplicate spawn. This is the concrete reason `Modified` is
  unusable as the trigger, and it is what `scene_patch.rs:27`'s TODO is about.

### 3.2 What a naive re-apply does — the `#24939` mechanism

`bevyengine/bevy#24939` reports duplicate/ghost children in `FeathersListView` when a
`ScenePatch` is re-applied. The mechanism, read out of `resolved_scene.rs`:

- `ResolvedSceneRoot::apply` (`resolved_scene.rs:65-81`) creates a **fresh**
  `SceneEntityReferences` on every call (line 70). `SceneEntityReferences::get`
  (`crates/bevy_ecs/src/template.rs:100-114`) spawns a new empty entity for any reference
  it has not seen — so on a second apply, every `#Name` maps to a **new** entity.
- `ResolvedScene::apply_related` (`resolved_scene.rs:349-399`) either takes the entity
  from that fresh map or calls `world.spawn_empty()` (line 367) — so **every child is
  spawned anew on every apply**.
- Before that, `apply_with` pushes a fresh
  `RelationshipTarget::with_capacity(n)` into the `BundleWriter`
  (`resolved_scene.rs:298-304`) and writes it over the root's existing `Children`. The
  replace fires `RelationshipTarget::on_discard`
  (`crates/bevy_ecs/src/relationship/mod.rs:307-337`), which only
  `try_remove::<ChildOf>()`s the old children — it does **not** despawn them.

So a second apply leaves the previous generation of children alive as parentless
"ghosts", with all their components, observers and UI nodes, while a fresh generation is
spawned. That is exactly #24939. It is also already documented behavior for the
single-apply case: `apply_scene_replaces_and_orphans_children` (`spawn.rs:894-926`)
asserts the pre-existing child survives but is unlinked.

⇒ **Any correct re-apply must despawn the previous generation first.** This is the same
conclusion `bevy_world_serialization` reached: `update_spawned_instances`
(`crates/bevy_world_serialization/src/world_asset_spawner.rs:358-382`) calls
`despawn_instance_internal` before `spawn_sync_internal`, with the comment *"Despawn the
world asset before respawning it. This is a very heavy operation, but otherwise, entities
may be left behind, or be left in an otherwise invalid state (e.g., invalid
relationships)."* It knows what to despawn because it records
`InstanceInfo::entity_map` (`world_asset_spawner.rs:42-47`) at spawn time. **We mirror
that precedent exactly.**

### 3.3 Cached-include staleness — the hard correctness question

Setup mirroring `loaded_asset_cached_patching` (`crates/bevy_scene/src/lib.rs:1084`):
`derived.bsn` = `:"base.bsn"` + `Position { x: 1. }` + `Children [...]`.

At **`derived` resolve time**:

- `CachedSceneAsset::resolve` (`crates/bevy_scene/src/scene.rs:421-441`) looks up base's
  `Handle`, calls `scene.include_cached(handle)` — which stores
  `CachedSceneInfo { handle, duplicate_templates: {} }` (`resolved_scene.rs:549-567`,
  `:572-579`) — and sets `context.cached = Some(&base_patch)` (a `&ScenePatch`,
  `scene.rs:176`).
- Every `get_or_insert_erased_template` for a type base also has
  (`resolved_scene.rs:461-498`) takes the **copy-on-write** branch: `is_cached = true`,
  `cached_template.clone_template()` — a **deep value snapshot of base's template as of
  this moment** — is pushed into derived's own `component_templates` and then patched, and
  the type is recorded in `duplicate_templates`.

At **apply time** (`resolved_scene.rs:234-288`):

- `cached.handle` is looked up **freshly** in `Assets<ScenePatch>` (line 236) and
  `patch.resolved` is `Arc`-cloned **at apply time** (line 248).

**Verdict on "do dependents hold stale `Arc`s?" — No.** `CachedSceneInfo` stores a
`Handle<ScenePatch>`, not an `Arc<ResolvedSceneRoot>`. Base's resolved root is re-read on
every single apply. Therefore, when `base.bsn` reloads:

| Base content | Dependent's live behavior without re-resolve | Correct? |
| --- | --- | --- |
| Templates **not** patched by dependent | read live from base's fresh `resolved` | ✅ |
| Base's related scenes (children) | read live (`apply_related` on `resolved_cached.scene`, line 284-286) | ✅ |
| Base's `entity_references` | read live | ✅ |
| Templates in `duplicate_templates` (patched by **both**) | dependent's frozen `clone_template()` snapshot is applied; base's is skipped via `SkipTemplate` (`resolved_scene.rs:325`, `:768-773`) | ❌ **stale** |
| A component base **gains** that dependent also patches | not in `duplicate_templates`, so base's is applied then dependent's whole-value template overwrites it — a *replace* where a *field merge* was intended | ❌ **wrong** |
| A component base **loses** that is in `duplicate_templates` | skip-set entry for an absent type; harmless | ✅ |

**So dependent staleness is real but narrow: it is exactly the copy-on-write overlap
set.** The fix is to re-resolve dependents.

### 3.4 Dependents are not auto-reloaded by the AssetServer

`ScenePatch::load_with` registers the base via
`load_from_path.load_from_path_erased(...)` (`scene_patch.rs:46`) →
`LoadContext::load_builder().load_erased(...)` (`crates/bevy_asset/src/reflect.rs:406-414`)
→ `self.load_context.dependencies.insert(index)`
(`crates/bevy_asset/src/loader_builders.rs:135`). That is a **runtime dependency**, not a
**loader dependency** (`LoadContext::loader_dependencies`, `loader.rs:615`, `:675`).
`AssetServer`'s hot-reload ancestor walk `queue_ancestors` (`server/mod.rs:2122-2132`)
only follows `loader_dependents`, which is populated exclusively from
`loaded_asset.loader_dependencies` (`server/info.rs:503-519`).

Furthermore, the dependent's `AssetInfo::dependents_waiting_on_recursive_dep_load` was
`core::mem::take`n when it first finished loading (`info.rs:534-540`), so re-running
`process_asset_load` for base propagates **nothing** to the dependent
(`info.rs:559-563`).

⇒ **Editing `base.bsn` produces zero events for `derived.bsn` today.** SPEC-6 must add
the invalidation explicitly.

---

## 4. Detailed design

### 4.1 Decision D-1 — `ScenePatch::reload_source` is **not** added (Contract G amended)

**Verdict: killed.** Rationale:

1. **It is unnecessary.** By F1 (§3.1), the AssetServer replaces the whole `ScenePatch`
   value with a freshly-loaded one carrying `scene: Some(_)`, and re-fires
   `LoadedWithDependencies`. The existing `scene.take()` at `scene_patch.rs:62` therefore
   operates on *fresh* data on every reload. There is nothing to retain.
2. **It would be wrong.** A retained `reload_source` is a snapshot of the **old** file.
   Re-resolving it would faithfully reproduce the scene the user just edited away.
3. **It is not implementable for the general case.** `Scene::resolve` is by-value
   (`scene.rs:48-122`) precisely so scenes can move data out; `Box<dyn Scene>` is not
   cloneable. Only Contract E's `Arc`-inner `DynamicScene` could be retained, producing a
   two-class `ScenePatch` where a public field is populated by exactly one loader.
4. **`bsn!`-in-code patches are unaffected either way.** `World::queue_spawn_scene`
   (`spawn.rs:195-206`) builds a patch with `AssetServer::add` (`server/mod.rs:1014`,
   path `None`), whose `scene` is `take()`n once and stays `None`. No file, no watcher, no
   reload, no behavior change. Confirmed.

The one thing `reload_source` *could* have bought — re-resolving a dependent after its
base changed — is delivered instead by D-5 (§4.5) via `AssetServer::reload`, which also
works for the transitive case and needs no new state.

### 4.2 Decision D-2 — the trigger is `LoadedWithDependencies` + a per-instance "applied" flag

`Modified` is unusable (F3). `LoadedWithDependencies` is the trigger. But it also fires on
first load, so we need to distinguish. Note that `patch.resolved.is_some()` does **not**
work as a discriminator, because the reloaded value is a brand-new `ScenePatch` with
`resolved: None`.

**Discriminate per *instance*, not per *asset*:** an instance that has already been
applied is marked by `SceneInstanceState::applied`. On a `LoadedWithDependencies`:

- instances still sitting in `WaitingScenes::scene_entities` → first apply (existing path);
- instances with `applied == true` → **re-apply** (new path).

These sets are disjoint by construction, so there is no double-apply. This also makes
the design correct when a reload lands in the same frame that a new instance is queued
(§4.8).

### 4.3 New public types & signatures

#### 4.3.1 `bevy_scene` — instance bookkeeping (`crates/bevy_scene/src/scene_patch.rs`)

```rust
/// Records what the most recent application of a [`ScenePatch`] created on this entity,
/// so that the scene can be re-applied when its source asset changes (hot reload).
///
/// Automatically added alongside [`ScenePatchInstance`]; you should not add it manually.
#[derive(Component, Debug, Default)]
pub struct SceneInstanceState {
    /// `true` once the instance's [`ScenePatch`] has been applied at least once.
    pub applied: bool,
    /// Every [`Entity`] spawned by the most recent application of the scene, *excluding*
    /// the instance entity itself. These are despawned before the scene is re-applied.
    pub spawned: Vec<Entity>,
}

/// A component that, when added, will queue applying the given [`ScenePatch`] ...
#[derive(Component, FromTemplate, Deref, DerefMut)]
#[require(SceneInstanceState)]           // NEW
pub struct ScenePatchInstance(pub Handle<ScenePatch>);
```

Using `#[require]` (rather than inserting the state after apply) means the component lands
in the same archetype move as `ScenePatchInstance` itself, on a still-empty entity, so
**the apply hot path performs no extra archetype move** (G4).

#### 4.3.2 `bevy_scene` — recording apply (`crates/bevy_scene/src/resolved_scene.rs`)

```rust
impl ResolvedSceneRoot {
    /// Applies this scene to `entity` exactly like [`ResolvedSceneRoot::apply`], and
    /// additionally appends every [`Entity`] spawned during the application (related
    /// entities and entities materialized for forward `#Name` references) to `spawned`.
    /// `entity` itself is never appended. `spawned` is sorted and deduplicated on return.
    pub fn apply_recording(
        &self,
        entity: &mut EntityWorldMut,
        bundle_scratch: &mut BundleScratch,
        spawned: &mut Vec<Entity>,
    ) -> Result<(), ApplySceneError>;
}
```

`ResolvedSceneRoot::apply` keeps its exact current signature and becomes a thin wrapper
that passes a non-recording recorder — **no allocation, no behavior change** for the
immediate path.

Internal plumbing, all private to `resolved_scene.rs`:

```rust
/// Optional sink for entities spawned while applying a `ResolvedScene`.
enum SpawnRecorder<'a> {
    None,
    Record(&'a mut Vec<Entity>),
}

impl SpawnRecorder<'_> {
    #[inline] fn push(&mut self, entity: Entity) {
        if let Self::Record(v) = self { v.push(entity); }
    }
    #[inline] fn reborrow(&mut self) -> SpawnRecorder<'_> {
        match self { Self::None => SpawnRecorder::None, Self::Record(v) => SpawnRecorder::Record(v) }
    }
}
```

Threaded as a trailing parameter through the three private methods
`ResolvedScene::apply` (`:203`), `ResolvedScene::apply_with` (`:222`) and
`ResolvedScene::apply_related` (`:349`). The single recording site is in `apply_related`:
immediately after the `let mut entity = if let Some(entity_reference) = ... { ... } else {
world.spawn_empty() };` block (`resolved_scene.rs:361-368`), push `entity.id()` — **in
both branches**, since a related entity is scene-owned regardless of how it was obtained.

`ResolvedSceneListRoot::spawn_with` (`:116-148`) passes `SpawnRecorder::None`
(scene-list hot reload is a non-goal).

#### 4.3.3 `bevy_ecs` — reference-map enumeration (`crates/bevy_ecs/src/template.rs`)

A forward `#Name` reference used only in a component value (e.g. `Reference(#Ghost)` with
no `#Ghost` entity in the scene) materializes an entity via
`SceneEntityReferences::get` (`template.rs:100-114`) that never becomes a related entity
and would otherwise be missed. Add, next to `get`/`set`:

```rust
impl SceneEntityReferences {
    /// Iterates every [`Entity`] currently associated with a [`SceneEntityReference`].
    pub fn iter(&self) -> impl Iterator<Item = Entity> + '_ {
        self.0.values().copied()
    }
}
```

`apply_recording` then, after `self.scene.apply(...)` returns, unions the map values into
`spawned`, removes the root entity, sorts and dedups:

```rust
spawned.extend(entity_references.iter());
spawned.retain(|e| *e != root);
spawned.sort_unstable();
spawned.dedup();
```

`root` is captured as `entity.id()` **before** the apply. Union + dedup is exhaustive:
every entity created during an apply comes from either `world.spawn_empty()` in
`apply_related` (recorded directly) or `SceneEntityReferences::get` (in the map).

### 4.4 Decision D-3 — the re-apply pass, inside `spawn_queued`

`spawn_queued` (`spawn.rs:708`) is already an exclusive system with a `BundleScratch`
local and an `AssetEvent<ScenePatch>` cursor. The re-apply pass goes there. New shared
helper (private to `spawn.rs`):

```rust
/// Applies `resolved` to `entity`, recording the spawned entities into the entity's
/// [`SceneInstanceState`] and marking it applied.
fn apply_to_instance(
    world: &mut World,
    entity: Entity,
    resolved: &ResolvedSceneRoot,
    bundle_scratch: &mut BundleScratch,
) -> Result<(), ApplySceneError> {
    // Reuse the previous Vec's allocation.
    let mut spawned = world
        .get_mut::<SceneInstanceState>(entity)
        .map(|mut s| { s.applied = true; core::mem::take(&mut s.spawned) })
        .unwrap_or_default();
    spawned.clear();

    let mut entity_mut = world.entity_mut(entity);
    let result = resolved.apply_recording(&mut entity_mut, bundle_scratch, &mut spawned);

    if let Some(mut state) = world.get_mut::<SceneInstanceState>(entity) {
        state.spawned = spawned;
    }
    result
}
```

All three apply sites route through it:

- the `waiting` branch (`spawn.rs:725-735`),
- `QueuedScenes::spawn_queued`'s `new_scene_entities` loop (`spawn.rs:805-826`),
- the new re-apply pass.

Entities without `SceneInstanceState` (there are none once `#[require]` lands, but be
defensive) simply do not record.

**The re-apply pass**, run inside the existing `Messages<AssetEvent<ScenePatch>>`
`resource_scope` in `spawn_queued` (`spawn.rs:718-738`), after the existing waiting-branch
loop:

```rust
// Collapse duplicate reload events for the same asset within this frame (see §4.8).
let mut reloaded: Vec<AssetId<ScenePatch>> = /* ids from LoadedWithDependencies this frame */;
reloaded.sort_unstable(); reloaded.dedup();

for id in reloaded {
    let Some(resolved) = world.resource::<Assets<ScenePatch>>().get(id).and_then(|p| p.resolved.clone())
    else { continue };

    // PERF: linear in the number of live ScenePatch instances, but only on reload.
    let instances: Vec<Entity> = scene_patch_instances
        .iter(world)
        .filter(|(_, instance, state)| state.applied && instance.0.id() == id)
        .map(|(entity, _, _)| entity)
        .collect();

    for entity in instances {
        // 1. Despawn the previous generation FIRST (see §3.2 / #24939).
        let previous = world
            .get_mut::<SceneInstanceState>(entity)
            .map(|mut s| core::mem::take(&mut s.spawned))
            .unwrap_or_default();
        for spawned in previous {
            if let Ok(entity_mut) = world.get_entity_mut(spawned) {
                entity_mut.despawn();
            }
        }
        // 2. Re-apply.
        if let Err(err) = apply_to_instance(world, entity, &resolved, &mut bundle_scratch) {
            error!("Failed to re-apply reloaded scene (id: {id}) to entity {entity}: {err}");
        }
    }
}
```

`scene_patch_instances` changes type from `&mut QueryState<&ScenePatchInstance>` to
`&mut QueryState<(Entity, &ScenePatchInstance, &SceneInstanceState)>`; the existing
diagnostic use at `spawn.rs:810` destructures the tuple. `QueryState::iter` updates
archetypes internally, so instances spawned earlier this frame are visible.

**Despawn semantics.** `EntityWorldMut::despawn` on a scene child recursively despawns its
children, because `Children` is declared `#[relationship_target(relationship = ChildOf,
linked_spawn)]` (`crates/bevy_ecs/src/hierarchy.rs:148`) and
`RelationshipTarget::on_despawn` (`relationship/mod.rs:331-338`) `try_despawn`s the
sources. This intentionally destroys runtime-added descendants of scene entities too —
they are logically scene-owned. Documented as state loss (§6.3).

### 4.5 Decision D-4/D-5 — dependent invalidation for `:"base.bsn"`

Three cases, distinguished by whether the dependent has an asset path:

**Case A — `.bsn` inherits `.bsn` (dependent has a path).** After successfully
re-resolving asset `id` in `resolve_scene_patches`, find every *other* `ScenePatch` asset
whose `dependencies` contain `id`, and force-reload it by path. Added to
`resolve_scene_patches` (which already holds `Res<AssetServer>` and
`ResMut<Assets<ScenePatch>>`):

```rust
/// Force-reloads every `ScenePatch` asset that includes `changed` as a cached base
/// (`:"base.bsn"`), so that its copy-on-write template snapshots are rebuilt.
///
/// This is deliberately *not* done via the AssetServer's own ancestor walk: a cached
/// scene include is a runtime dependency (`LoadBuilder::load_erased`,
/// `loader_builders.rs:135`), not a loader dependency, so `queue_ancestors`
/// (`server/mod.rs:2122`) does not see it.
fn reload_dependents(
    changed: AssetId<ScenePatch>,
    assets: &AssetServer,
    patches: &Assets<ScenePatch>,
) {
    let changed_untyped = changed.untyped();
    // PERF: linear in the number of ScenePatch assets, but only on reload.
    let dependents: Vec<AssetPath<'static>> = patches
        .iter()
        .filter(|(dependent_id, patch)| {
            *dependent_id != changed
                && patch.dependencies.iter().any(|d| d.id() == changed_untyped)
        })
        .filter_map(|(dependent_id, _)| assets.get_path(dependent_id).map(AssetPath::into_owned))
        .collect();
    for path in dependents {
        assets.reload(path);
    }
}
```

Called only when the resolve of `changed` was itself a reload. `AssetServer::reload`
(`server/mod.rs:958`) is public and works regardless of `watching_for_changes`.

The reloaded dependent then re-enters the pipeline through the normal
`LoadedWithDependencies` path a frame or two later and its own instances are re-applied
by §4.4, and *its* dependents are invalidated transitively. The include graph is acyclic
(a cycle already fails at first resolve), so this terminates.

*Deriving the reverse map on demand rather than caching it is a deliberate simplicity
trade (G3): it costs one `Assets<ScenePatch>` scan per reload event, and reloads are
human-paced. A cached reverse index is a follow-up if profiling ever demands it.*

**Case B — `bsn!`-in-code patch, no path, no overlap with the base.** Fully live: the
base handle is resolved fresh at every apply (§3.3). But no *event* reaches the dependent
today. §4.4's re-apply pass keys on the *changed* asset's id, not the dependent's, so the
dependent's instances would not be re-applied. **Fix:** the re-apply pass's instance scan
matches `instance.0.id() == id` **or** "the instance's patch depends on `id`":

```rust
.filter(|(_, instance, state)| {
    state.applied && (instance.0.id() == id || patch_depends_on(patches, instance.0.id(), id))
})
```

where `patch_depends_on` checks `patches.get(dependent).dependencies` for `id`. This
covers Case A's dependents in the interim frames too (harmless: they get re-applied once
from the base's live data, then again from their own reload).

**Case C — `bsn!`-in-code patch that *overlaps* the base (copy-on-write set).** Its
`scene` was consumed at first resolve and there is no file to re-read, so the overlap
snapshot (§3.3) **cannot** be refreshed. Case B's re-apply still updates everything else.

> **Documented limitation.** An in-code `bsn!` scene that both includes `:"base.bsn"` and
> patches a component the base also patches keeps the base's values *as of the first
> resolve* for that component. Children, non-overlapping components and entity references
> all hot reload correctly. Move the overlapping patch into a `.bsn` file to get full hot
> reload. This is a direct consequence of the by-value `Scene::resolve` design
> (`scene.rs:48-122`) and is tracked as future work with `cart/bevy#36`.

### 4.6 Decision D-6 — resolution stops emitting spurious `Modified`

Change `resolve_scene_patches` (`spawn.rs:618-623`) to use `Assets::get_mut_untracked`
(`crates/bevy_asset/src/assets.rs:456-466`) for both the `scene.take()` and the `resolved`
write. Verified safe: nothing in-tree reads `AssetEvent<ScenePatch>::Modified` (only
`spawn.rs:608`, `:713`, `:718` touch that message type, and none handle `Modified`). This
makes `Modified` on `ScenePatch` mean "the value actually changed", removes two message
writes per resolve, and partially discharges the TODO at `scene_patch.rs:27`.

### 4.7 Decision D-7 — queued scenes become tracked instances

`World::queue_spawn_scene` (`spawn.rs:195-206`) and `EntityWorldMut::queue_apply_scene`
(`spawn.rs:510-518`) push `(entity, handle)` into `QueuedScenes::new_scene_entities` by
hand, bypassing `ScenePatchInstance`. Replace the manual push with inserting
`ScenePatchInstance(handle)`; the existing `on_add_scene_patch_instance` observer
(`spawn.rs:695-705`) performs the identical push. Behavior-identical, and these entities
become hot-reload-tracked — they are exactly the ones that can include `:"base.bsn"`
(Case B). Visible change: they now carry `ScenePatchInstance` + `SceneInstanceState`
(§8, release notes; not a breaking change).

### 4.8 Ordering, scheduling, and debounce

- **Placement unchanged.** `(resolve_scene_patches, spawn_queued).chain()` in `SpawnScene`,
  `SceneSpawnerSystems::SceneSpawn` (`lib.rs:946-957`). `resolve_scene_patches` re-resolves
  and invalidates dependents; `spawn_queued` re-applies. The chain guarantees `resolved` is
  fresh before re-application within the same frame.
- **Reload in the same frame as a new instance spawn.** The re-apply pass runs inside
  `spawn_queued`'s event loop, i.e. *before* the `QueuedScenes` drain loop
  (`spawn.rs:773-785`). A brand-new instance has `applied == false`, so the re-apply pass
  skips it; the drain loop then applies it once, using the already-refreshed `resolved`.
  No double apply, no stale apply.
- **Reload while an instance is still waiting for first load.** The instance is in
  `waiting.scene_entities` and has `applied == false`; the existing waiting branch applies
  it once. Correct.
- **Debounce: collapse per-asset within the frame, not across frames.**
  `bevy_world_serialization` debounces across `WORLD_ASSET_AGE_THRESHOLD = 2` frames
  (`world_asset_spawner.rs:88-95`, `:640-676`) as a workaround for
  bevyengine/bevy#12756 — glTF sub-asset loads each fire a parent event. `ScenePatch` has
  no sub-assets, so a frame-count debounce would only add latency and could *swallow* a
  genuine second edit. We instead dedup the `LoadedWithDependencies` ids read in a single
  `spawn_queued` invocation (`sort_unstable` + `dedup`), which removes the only real
  duplicate source (two reload tasks completing in the same frame) at zero latency cost.
  Cite the divergence in a code comment referencing `world_asset_spawner.rs:88`.
- **Ambiguity.** No new resources or components are read outside `SpawnScene`, so no new
  system-ordering ambiguities are introduced (cf. `#25222`). Run
  `cargo run -p ci -- test` with ambiguity detection on the `bevy_scene` tests.

### 4.9 Feature gating

**No cargo feature gates hot reload.** Justification:

- The re-apply pass is inert unless `LoadedWithDependencies` fires for an id that already
  has an *applied* instance — which requires either a real file change (needs
  `bevy_asset/file_watcher` or `AssetPlugin { watch_for_changes_override: Some(true) }`)
  or an explicit `AssetServer::reload` call. There is nothing to switch off.
- `file_watcher` is a `bevy_asset` feature; `bevy_scene` neither declares nor should
  declare it. `bevy_asset` itself does not gate systems on it either — it gates *inside*
  `handle_internal_asset_events` with a runtime `if !infos.watching_for_changes { return; }`
  (`server/mod.rs:2116-2120`). We follow the same spirit.
- Gating on `AssetServer::watching_for_changes()` (`server/mod.rs:194`) would break the
  legitimate manual `AssetServer::reload` API — and would make the tests in §7.4
  untestable without a real filesystem watcher.
- The `.bsn` **loader** remains behind SPEC-5's `bsn_asset` feature. The hot-reload code
  in `spawn.rs`/`resolved_scene.rs` is feature-independent and works for any
  `AssetLoader<Asset = ScenePatch>`, including third-party loaders and the test
  `FakeSceneLoader`.

---

## 5. Step-by-step implementation plan

Each step compiles and passes tests on its own.

1. **`crates/bevy_ecs/src/template.rs`** — add `SceneEntityReferences::iter` (§4.3.3) with
   a doc comment. Unit test `scene_entity_references_iter`.
2. **`crates/bevy_scene/src/resolved_scene.rs`** — add the private `SpawnRecorder`, thread
   it through `ResolvedScene::apply` / `apply_with` / `apply_related`, add
   `ResolvedSceneRoot::apply_recording`, and re-express `ResolvedSceneRoot::apply` in terms
   of it with `SpawnRecorder::None`. `ResolvedSceneListRoot::spawn_with` passes `None`.
   No public behavior change; existing tests must pass unchanged.
3. **`crates/bevy_scene/src/scene_patch.rs`** — add `SceneInstanceState`, add
   `#[require(SceneInstanceState)]` to `ScenePatchInstance`. Export from
   `crates/bevy_scene/src/lib.rs` (`pub use scene_patch::*` already covers it); add
   `SceneInstanceState` to the `prelude` (`lib.rs:900-906`).
4. **`crates/bevy_scene/src/spawn.rs`** — add `apply_to_instance`, route the two existing
   apply sites through it. Behavior unchanged; `spawned` is now recorded.
5. **`crates/bevy_scene/src/spawn.rs`** — D-7: `queue_spawn_scene` / `queue_apply_scene`
   insert `ScenePatchInstance` instead of pushing manually.
6. **`crates/bevy_scene/src/spawn.rs`** — D-6: `get_mut` → `get_mut_untracked` in
   `resolve_scene_patches`.
7. **`crates/bevy_scene/src/spawn.rs`** — the re-apply pass (§4.4), including the
   `QueryState` type change and the per-frame id dedup.
8. **`crates/bevy_scene/src/spawn.rs`** — `reload_dependents` (§4.5 Case A), called from
   `resolve_scene_patches`; plus the Case-B dependency filter in the re-apply pass.
9. **Tests** — §7.4 hot-reload tests against `FakeSceneLoader` (no SPEC-5 dependency), so
   steps 1-8 can land and be validated *before* the `.bsn` loader exists.
10. **Docs** — module docs on `SceneInstanceState`, a "Hot reload" section in
    `crates/bevy_scene/src/lib.rs`'s crate docs, and the release-content stubs (§8).
11. **After SPEC-5 lands** — the `.bsn` e2e suite (§7.2/§7.3), the bench (§7.5), and the
    example change (§7.6).

---

## 6. Edge cases & error handling

1. **Resolve fails after reload** (syntax error). `resolve_scene_patches` logs
   `Failed to resolve scene {id}: {err}` (`spawn.rs:624`) and leaves `resolved: None` on
   the *new* value; the re-apply pass's `.and_then(|p| p.resolved.clone())` yields `None`
   and `continue`s — **live instances keep the last good version**. Desired editor
   behavior; asserted by `hot_reload_parse_error_keeps_previous_scene`.
2. **Apply fails mid-way after reload.** The previous generation is already despawned, so
   the instance is partially applied. Log at `error!` with asset id, path and entity
   (mirroring `spawn.rs:814-817`) and keep whatever landed in `spawned` so the *next*
   reload still cleans up. Do **not** despawn the instance root — unlike
   `ResolvedSceneRoot::spawn` (`resolved_scene.rs:47-57`) we do not own it.
3. **Instance entity despawned between resolve and re-apply.** `get_entity_mut` fails;
   skip silently (its recorded entities died with it via linked spawn).
4. **Components the new `.bsn` no longer declares stay on the live root.** Apply is
   `get_or_insert` + bundle write (`resolved_scene.rs:266`, `:292`) and never removes.
   Known limitation; documented on `SceneInstanceState` and in the release note.
   Workaround: respawn the instance. See open question 1.
5. **External `Entity` references into scene content dangle after reload** — every scene
   entity is despawned and respawned with a new id. Documented; tested by
   `hot_reload_invalidates_external_entity_references`.
6. **Asset removed while instances live.** The `AssetEvent::Removed` arm
   (`spawn.rs:628-636`) is unchanged; instances keep their last applied content.
7. **Two reloads of one asset in one frame** — deduped (§4.8). **Reload with zero live
   instances** — re-resolves and invalidates dependents, applies to nothing.
8. **Self-dependent `ScenePatch`** — impossible via `include_cached`
   (`CachedSceneError::MultipleCached`/`LateCached`, `resolved_scene.rs:549-567`), plus
   `reload_dependents`'s `*dependent_id != changed` guard.
9. **UUID asset ids.** `AssetServer::get_path` returns `None`, so `reload_dependents`
   skips them; §4.5 Case B still covers their instances.

---

## 7. Validation plan for the whole series

### 7.1 Harness

Reuse the pattern already proven at `crates/bevy_scene/src/lib.rs:1113-1158` and
`benches/benches/bevy_scene/spawn.rs:399-428`:

```rust
fn bsn_test_app(dir: Dir) -> App {
    let mut app = App::new();
    app.register_asset_source(
        AssetSourceId::Default,
        AssetSourceBuilder::new(move || Box::new(MemoryAssetReader { root: dir.clone() })),
    );
    app.add_plugins((TaskPoolPlugin::default(), AssetPlugin::default(), ScenePlugin));
    app.finish();
    app.cleanup();
    app // DynamicBsnLoader is registered by ScenePlugin behind `bsn_asset` (SPEC-5)
}
```

- Files are written with `Dir::insert_asset_text(Path::new("x.bsn"), TEXT)`
  (`crates/bevy_asset/src/io/memory.rs:53-55`), which is a plain map insert and therefore
  **overwrites** on a second call — this is the file-edit primitive.
- Frame pumping uses the existing `run_app_until` helper (`lib.rs:2524-2534`).
- New file: **`crates/bevy_scene/src/bsn_asset/e2e_tests.rs`**, `#[cfg(all(test, feature =
  "bsn_asset"))]`, `mod e2e_tests;` from SPEC-5's `bsn_asset` module. Hot-reload tests that
  do **not** need the real loader (steps 1-8) go in
  `crates/bevy_scene/src/spawn.rs`'s existing `mod tests` (`spawn.rs:870`) using
  `FakeSceneLoader`.

**Simulating a file modification without a watcher** (the mechanism, cited):

```rust
dir.insert_asset_text(Path::new("player.bsn"), V2_TEXT);           // "edit the file"
app.world().resource::<AssetServer>().reload("player.bsn");        // server/mod.rs:958
run_app_until(&mut app, || /* predicate on world state */);
```

`AssetServer::reload` → `reload_internal` → `load_internal(handle, path, force = true, ..)`
re-runs the loader unconditionally and does **not** require `watching_for_changes`
(`server/mod.rs:958-984`). For `FakeSceneLoader`-based tests, the "edit" is swapping the
closure's backing state (e.g. an `Arc<Mutex<Box<dyn Fn() -> Box<dyn Scene>>>>`) rather
than the file text; the reload trigger is identical.

*Rejected alternative:* manually writing `AssetEvent::Modified` into
`Messages<AssetEvent<ScenePatch>>` and mutating `Assets<ScenePatch>` by hand. It cannot
reproduce the real event ordering (F2) or the whole-value replacement (F1), so it would
test a fiction.

### 7.2 Static → dynamic parity matrix

Each row: the existing static test in `crates/bevy_scene/src/lib.rs`, the new dynamic test
in `bsn_asset/e2e_tests.rs`, the fixture, and what it asserts. Naming convention:
`dyn_<static_test_name>`.

| # | Static test (`lib.rs:line`) | Dynamic test | Fixture `.bsn` | Asserts |
| --- | --- | --- | --- | --- |
| 1 | `supports_fully_qualified_component_paths` (:991) | `dyn_fully_qualified_component_paths` | `::bevy_ecs::hierarchy::Children []` | loads & spawns; registry lookup accepts leading `::` and full paths |
| 2 | `cached_patching` (:1003) | `dyn_cached_patching` | `a.bsn`: `Position { y: 2. }`; `b.bsn`: `:"a.bsn"` `Position { x: 1. }` | root has `Position { x: 1., y: 2., z: 0. }` — field-level merge across the include |
| 3 | `cached_patching_order` (:1050) | `dyn_cached_patching_order` | as above with both files setting `x` | dependent's value wins; base applied first (`resolved_scene.rs:254-266`) |
| 4 | `loaded_asset_cached_patching` (:1084) | `dyn_loaded_asset_cached_patching` | `a.bsn` + `b.bsn` with `Children [ #X ]` / `Children [ #Y ]` | 2 children in order `X`, `Y`; merged `Position` |
| 5 | `inline_scene_patching` (:1209) | `dyn_repeated_patch_same_entity` | `Position { x: 1. } Position { y: 2. }` | two patches on one entity merge into one component (same template slot) |
| 6 | `hierarchy` (:1256) | `dyn_hierarchy` | `#A Children [ (#B Children [#X]), (#C Children [#Y]) ]` | names & child counts at all 3 levels |
| 7 | `bsn_name_references` (:1341) | `dyn_name_references` | `#X Children [ (Reference(#X)) ]` | child's `Reference.0 == root` |
| 8 | `bsn_reverse_reference` (:1400) | `dyn_reverse_reference` | `Reference(#Last) Children [ #First, #Second, #Last ]` | forward reference resolves to `children[2]` |
| 9 | `bsn_list_name_references` (:1425) | *n/a* | — | `.bsn` has no scene-list root form in SPEC-3; N/A, note in matrix |
| 10 | `primitive_literals` (:1517) | `dyn_primitive_literals` | one component per numeric family, `bool`, `String`, `&'static str`, `Vec<u8>`, `[u8; 4]` | all parse & coerce; `i128`/`u128`/`f64` included; lossless widening path (SPEC-4) |
| 11 | `partial_tuple_struct` (:1678) | `dyn_partial_tuple_struct` | `Tup(1)` on a 3-field tuple struct | leading fields set, trailing fields defaulted |
| 12 | `enum_patching` (:1741) | `dyn_enum_patching` | unit / tuple / struct variants, incl. re-patching a variant | jbuehler23's `DynamicEnum`-wrapping fix; later patch replaces variant |
| 13 | `struct_patching` (:1803) | `dyn_struct_patching` | nested struct field patches | partial field application over `ReflectDefault` |
| 14 | `field_patching_with_default` (:1853) | `dyn_field_patching_with_default` | component with non-`Default`-ish fields | unmentioned fields take the template's default |
| 15 | `handle_template` (:1903) | `dyn_handle_template` | `Sprite { image: "img.png" }` | string→`Handle` via `ReflectConvert`; the handle is registered as a dependency and the patch does not resolve until it loads |
| 16 | `scene_list_children` (:1958) | `dyn_children_list` | `Children [ A, B, C ]` | 3 children, order preserved |
| 17 | `generic_patching` (:1991) | `dyn_generic_patching` | `Wrapper<u32> { value: 3 }` written as its registered `type_path` | generic components resolvable by full path |
| 18 | `comments_in_bsn` (:2103) | `dyn_comments` | `// line` and `/* block */` between patches | comments skipped by the lexer |
| 19 | `bsn_entry_can_surpass_tuple_limit` (:2124) | `dyn_many_patches_on_one_entity` | 20 distinct components on one entity | no tuple-arity limit in the dynamic path |
| 20 | `scene_without_explicit_component_still_spawns_component` (:2290) | `dyn_required_components` | component with `#[require(Other)]` | required component present after apply |
| 21 | `enum_variant_field_values_use_implicit_into` (:2740) | `dyn_value_coercion` | field typed `Val` given `3.0` / a string | `ReflectConvert` + numeric-widening coercion ladder (SPEC-0 Contract E) |
| 22 | `scene_with_optional_components` (:2876) | `dyn_optional_components` | `Option<T>` field given `Some(..)` / omitted | option field handling |
| 23 | `scene_nested_entity_references` (:2897) | `dyn_nested_entity_references` | 3-deep `#Name` graph | asset-based `SceneEntityReference` identity (Contract C.4) is stable & distinct |
| 24 | `repeated_call_entity_reference` (:2477) | `dyn_two_instances_get_distinct_entities` | two `ScenePatchInstance`s of the same file | the same `#Name` in two instances maps to two different entities (fresh `SceneEntityReferences` per apply, `resolved_scene.rs:70`) |
| 25 | `drop_is_called_for_uninserted_components` (:2487) | `dyn_drop_on_failed_apply` | file whose 2nd patch fails to build | `BundleScratch::manual_drop` path runs; no leak |
| 26 | `despawn_on_failed_spawn` (:2513) | `dyn_no_entity_leak_on_failed_load` | file with an unregistered type | loader error, entity count unchanged, error logged not panicked (SPEC-0 §6.6) |

Explicitly **N/A** — the implementer records each with "N/A — SPEC-0 §2 non-goal" so the
matrix stays auditable: `constant_values` (:1312), `on_template` (:1492),
`children_list_expr` (:1588), `children_single_expr` (:1614), `conditional_scene` (:1639),
`scene_expression_passing_pointless` (:1700), `child_of_template` (:1930),
`empty_scene_expressions` (:2040), `closures_in_bsn` (:2052), `component_scene`* (:2191,
:2220), the four `*_name_reference` scene-component tests (:2307-:2452),
`scene_with_blocks` (:2570), `scene_with_oneshot_system` (:2689),
`direct_macro_values_in_bsn` (:2721), `enum_variant_subexpressions_are_hoisted` (:2776),
`field_name_shorthand` (:2818), `scene_list_nested_entity_references` (:2928).

### 7.3 Cross-feature integration tests

In `bsn_asset/e2e_tests.rs`:

- **`bsn_macro_inherits_bsn_file`** — `world.spawn_scene(bsn! { :"base.bsn" Position { x: 1. } })`
  after `base.bsn` loads. Asserts field merge across the static/dynamic boundary and that
  both patches landed in the **same template slot** (SPEC-0 §6.3) — verified by asserting
  a single merged `Position`, not two writes.
- **`bsn_file_inherits_bsn_file`** — three-level chain `c.bsn : b.bsn : a.bsn`; asserts
  `include_cached` ordering rules (`resolved_scene.rs:549-567`) hold transitively.
- **`dynamic_and_static_patch_merge_on_one_component`** — `base.bsn` sets `Position.y`,
  the `bsn!` scene sets `Position.x`; asserts `{x, y}` both present and `z` defaulted, and
  that `CachedSceneInfo::duplicate_templates` copy-on-write produced exactly one
  `Position` insert (assert via component count / archetype).
- **`dynamic_children_merge_with_static_children`** — base `.bsn` declares
  `Children [ A ]`, the `bsn!` scene declares `Children [ B ]`; asserts **one** `Children`
  collection with `[A, B]` (Contract C.3: dynamic and static children of the same
  relationship must land in the same `related` entry).
- **`bsn_file_asset_dependency_gate`** — a `.bsn` referencing `"img.png"` does not resolve
  until the image loads; the instance stays in `WaitingScenes` and spawns on the right
  frame.
- **`entity_references_across_reload`** — see §7.4.

### 7.4 Hot-reload tests

Land with steps 1-8 in `crates/bevy_scene/src/spawn.rs`'s `mod tests`, using
`FakeSceneLoader`; mirrored with real `.bsn` text in `e2e_tests.rs` after SPEC-5.

| Test | Setup | Assert |
| --- | --- | --- |
| `hot_reload_replaces_root_components` | v1 `Position { x: 1. }` → reload v2 `Position { x: 5. }` | root's `Position.x == 5.` |
| `hot_reload_despawns_previous_children` | v1 `Children [A, B]` → reload v2 `Children [C]` | root has exactly 1 child; the two v1 child entities **no longer exist** (`world.get_entity(..).is_err()`) — the #24939 regression test |
| `hot_reload_no_orphaned_entities` | as above | total `world.entities().len()` returns to `pre + 1(root) + 1(child)`; no parentless leftovers |
| `hot_reload_preserves_instance_entity` | any reload | the `ScenePatchInstance` entity id is unchanged, and its `ChildOf` parent (if any) is intact |
| `hot_reload_applies_to_all_instances` | 3 instances of one file | all three updated in the same frame |
| `hot_reload_of_base_updates_dependent_file_instances` | `base.bsn`, `derived.bsn : base.bsn` overlapping on `Position`; edit `base.bsn` | derived instance's merged `Position` reflects the **new** base values — the §3.3 staleness regression test; requires §4.5 Case A |
| `hot_reload_of_base_updates_bsn_macro_instances` | `queue_spawn_scene(bsn! { :"base.bsn" Marker })`; edit `base.bsn`'s children | children updated (Case B) |
| `hot_reload_of_base_does_not_update_macro_overlap` | as above but the `bsn!` also patches `Position` | asserts the **documented** Case-C limitation explicitly, so the boundary is pinned and any future fix trips this test |
| `hot_reload_parse_error_keeps_previous_scene` | reload with invalid text | scene unchanged, error logged, `resolved` still the old `Arc` on the old value — instance untouched |
| `hot_reload_invalidates_external_entity_references` | external entity stores a scene child's `Entity`; reload | the stored id is dead; documented state loss |
| `hot_reload_during_pending_spawn` | queue an instance and reload the asset in the same frame | applied exactly once, with the new content (§4.8) |
| `hot_reload_twice_in_one_frame_applies_once` | two `AssetServer::reload` calls completing in the same frame | one despawn/apply cycle (dedup, §4.8) |
| `resolve_does_not_emit_modified` | first load of a `ScenePatch` | zero `AssetEvent::Modified` observed in `Messages` (D-6) |
| `queued_scene_gains_scene_patch_instance` | `queue_spawn_scene(bsn!{..})` | entity has `ScenePatchInstance` + `SceneInstanceState { applied: true }` (D-7) |
| `scene_instance_state_records_all_spawned` | nested scene with forward `#Name` refs | `spawned` contains every descendant **and** the forward-referenced entity; excludes the root; sorted & deduped |

Plus one `bevy_ecs` unit test, `scene_entity_references_iter`, for §4.3.3.

### 7.5 Performance guardrail

Extend `benches/benches/bevy_scene/spawn.rs` (which already has the in-memory source +
`FakeSceneLoader` scaffolding at `:399-428`):

1. `dynamic_ui_scene_spawn` — spawn the `ui()` scene from a real `.bsn` file, to be
   compared against the existing `ui_immediate_function_scene`.
   **Guardrail: dynamic spawn ≤ 1.25× static spawn.** Resolution is one-shot and cached in
   `ScenePatch::resolved`, so the *apply* cost must be within noise of static; anything
   above 1.25× means a dynamic template is not hitting `push_to_bundle_writer`
   (Contract A) and is causing per-component archetype moves.
2. `queued_scene_instance_spawn` — `ScenePatchInstance` spawn path, run **before and
   after** this spec's changes.
   **Guardrail: ≤ 3% regression** vs. the pre-SPEC-6 baseline (cost is the `SceneInstanceState`
   `Vec` push per related entity; `#[require]` keeps archetype moves flat, §4.3.1).
3. `ui_immediate_function_scene` / `named_entity_reference` / `ui_immediate_loaded_scene`
   — existing benches; **must show no regression**, since the immediate path uses
   `SpawnRecorder::None`. This is the concrete check for G4.
4. `scene_hot_reload` — a new bench measuring one despawn+re-apply cycle of a 100-node
   scene. No hard guardrail (nothing to compare to); recorded so future reconciliation work
   (`cart/bevy#36`) has a baseline to beat.

### 7.6 Manual QA checklist

Extend `examples/scene/bsn.rs` to load its UI from `assets/scenes/ui.bsn` via
`ScenePatchInstance` (keeping the `bsn!` version behind a comment), then run
`cargo run --example bsn --features bevy/file_watcher`:

1. UI appears.
2. Change a `BackgroundColor` in `ui.bsn`. → color updates without restart; button count
   unchanged; no flicker beyond one frame.
3. Add a child button. → appears; existing buttons are re-spawned (expected) and remain
   interactive (scene observers re-registered).
4. Delete a child. → disappears; `world.entities().len()` shows no orphan growth after 10
   edits (the #24939 check).
5. Introduce a syntax error, save. → error logged, last good UI still rendering, no panic.
   Fix it, save. → UI updates.
6. Split into `ui.bsn` + `button.bsn` (`ui.bsn` uses `:"button.bsn"`); edit `button.bsn`.
   → all buttons update (§4.5 Case A).
7. Save 5 times in quick succession. → one rebuild per save, no runaway entity growth,
   stable frame time.
8. Mutate a component at runtime (system sets `BackgroundColor` on click), then reload.
   → the mutation is **lost**: the documented, expected state loss.

---

## 8. Rollout

**Feature flags.** None added by SPEC-6. `.bsn` loading stays behind SPEC-5's default-on
`bevy_scene/bsn_asset` (plumbed through `bevy_internal`). Hot reload requires the user to
enable `bevy/file_watcher` (or `AssetPlugin { watch_for_changes_override: Some(true) }`),
exactly as for every other asset type.

**Release note stub** — `_release-content/release-notes/bsn_hot_reload.md`, following
`_release-content/release_notes_template.md`:

```markdown
---
title: Hot reloading `.bsn` scenes
authors: ["@<author>"]
pull_requests: [<spec-6 PR>]
---

Entities spawned from a `.bsn` file via `ScenePatchInstance` now update live when the file
changes on disk. Enable asset watching (the `file_watcher` feature, or
`AssetPlugin { watch_for_changes_override: Some(true) }`), edit a `.bsn` file, and every
live instance is rebuilt from the new definition — including instances of other `.bsn`
files that inherit it with `:"base.bsn"`.

Reloading is a rebuild, not a reconciliation: the scene's descendants are despawned and
respawned, so runtime state on scene-spawned entities (and `Entity` ids held elsewhere)
does not survive a reload. State-preserving reconciliation is planned as follow-up work.
Components that a `.bsn` file *stops* declaring are also not removed from the live root
entity; respawn the instance to pick that up.

The set of entities a scene created is now visible on each instance as
`SceneInstanceState::spawned`.
```

**Migration guide stub** — `_release-content/migration-guides/scene_patch_instance_state.md`,
following `_release-content/migration_guides_template.md`:

```markdown
---
title: `ScenePatchInstance` now requires `SceneInstanceState`
pull_requests: [<spec-6 PR>]
---

`ScenePatchInstance` gained `#[require(SceneInstanceState)]`, so entities carrying it also
carry a `SceneInstanceState` component recording the entities the scene spawned. This is
what makes hot reload able to clean up the previous generation of scene entities instead of
orphaning them.

`Commands::queue_spawn_scene`, `World::queue_spawn_scene` and
`EntityWorldMut::queue_apply_scene` now insert `ScenePatchInstance` on the target entity
rather than registering it out-of-band. Spawning and application behavior is unchanged,
but code that asserted on an exact archetype or component count for these entities must
account for the two additional components.

`ResolvedSceneRoot::apply` is unchanged. If you need to know which entities an application
created, use the new `ResolvedSceneRoot::apply_recording`.
```

No other migration guide is required: no public signature is removed or changed
incompatibly.

---

## 9. Acceptance criteria

1. Editing a `.bsn` file updates every live `ScenePatchInstance` of it within two frames,
   with no orphaned entities (`hot_reload_despawns_previous_children`,
   `hot_reload_no_orphaned_entities`).
2. Editing a base `.bsn` updates instances of every `.bsn` that inherits it, including the
   copy-on-write overlap set (`hot_reload_of_base_updates_dependent_file_instances`).
3. `ScenePatch` gains **no** new fields; `bevy_scene` gains no new resources and no new
   cargo features (`git diff` on `scene_patch.rs` shows only `SceneInstanceState` +
   `#[require]`).
4. A `.bsn` parse error during reload leaves live instances rendering the previous version
   and logs — never panics.
5. `ResolvedSceneRoot::apply`'s signature and behavior are unchanged; the three existing
   immediate-path benches show no regression, and the instance-path bench regresses ≤ 3%.
6. Every non-N/A row of §7.2 has a passing dynamic test; every N/A row cites the SPEC-0 §2
   non-goal that excludes it.
7. All five §7.3 cross-feature tests pass, proving static/dynamic template-slot and
   relationship-entry unification (Contract C.3, SPEC-0 §6.3).
8. `cargo run -p ci -- test` and `cargo run -p ci -- lints` pass; no new system-ordering
   ambiguities.
9. The §7.6 manual checklist has been walked end-to-end by the implementer and the result
   recorded in the PR description.

---

## 10. Open questions

1. **Should removed components be stripped from the live root?** (§6.4). Doing it right
   needs `BundleWriter` to report the `ComponentId`s it wrote, so `SceneInstanceState`
   could store the previous set and `remove_by_id` the difference. That is a `bevy_ecs`
   change outside SPEC-1/2's contracts. **Recommendation:** ship without it, file a
   follow-up alongside `cart/bevy#36`. Master-doc owners should confirm the deferral.
2. **Is deriving the reverse-dependency map by scanning `Assets<ScenePatch>` on every
   reload acceptable long-term?** It is O(#patches × #deps) per reload event. Fine for
   human-paced edits; a project with thousands of `.bsn` assets and an editor that
   auto-saves aggressively might want a cached index maintained in `resolve_scene_patches`.
   Deferred; noted with a `PERF:` comment at the call site.
3. **Should `SceneListPatch` hot reload at all?** Currently out of scope because scene
   lists have no owning instance entity. If `bsn_list!` from a `.bsn` file becomes a thing
   (SPEC-3 currently has no scene-list root form), this needs revisiting — likely by
   introducing a `SceneListPatchInstance` marker that owns the spawned roots.
4. **Should hot reload be opt-out?** A `SceneHotReload(bool)` resource would be trivial,
   but the pass is already inert without reload events (§4.9), so it would be config for
   config's sake. Flagging in case a reviewer wants an escape hatch for shipped builds
   that nonetheless call `AssetServer::reload` deliberately.
5. **Contract C.4 interaction:** if SPEC-2 makes the asset-based `SceneEntityReference`
   identity `(asset_path, node_id)`, then two `ScenePatchInstance`s of the *same file*
   would share reference identities. That is fine today because
   `ResolvedSceneRoot::apply` builds a **fresh** `SceneEntityReferences` per apply
   (`resolved_scene.rs:70`) — test #24 in §7.2 pins this. SPEC-2 must not "optimize" that
   map to be shared across applies; flagging so the invariant is recorded in both specs.
6. **Should `reload_dependents` also fire on `AssetEvent::Removed`?** A deleted base leaves
   dependents with a handle to a missing asset, which surfaces at apply time as
   `ApplySceneError::MissingCachedScene` (`resolved_scene.rs:236-241`). Current behavior
   is "keep the last applied content and log on the next apply", which seems right; not
   specced further.
