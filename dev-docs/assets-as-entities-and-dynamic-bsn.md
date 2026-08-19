# Assets as Entities × Dynamic BSN

**Status: analysis / working notes, 2026-08-19.** How the upstream "Assets as Entities"
initiative interacts with our `dynamic-bsn` branch, what breaks, what gets better, and what
we should do now vs. after it lands. No code changes are proposed for the current branch;
see §6 for the action list.

## 1. Upstream state (as of 2026-08-19)

- **Goal issue [#23094](https://github.com/bevyengine/bevy/issues/23094)** (andriyDev,
  2026-02): "Assets as Entities" is an official Bevy Project Goal. SMEs: **andriyDev and
  cart**. Design docs not yet written; working group lives on Discord. The original feature
  issue [#11266](https://github.com/bevyengine/bevy/issues/11266) (2024) was closed in favor
  of it and contains the staged plan.
- **PR [#22939](https://github.com/bevyengine/bevy/pull/22939) "Assets as entities v0"**
  (andriyDev) is **open, approved by 3+ reviewers, milestoned 0.20, S-Waiting-on-SME (cart)**,
  and currently has merge conflicts with main. We are on 0.20.0-dev — this is aimed at *our*
  release window.
- The ECS blocker (allocating `Entity` ids from async asset loaders) was solved by merged
  PR [#18670](https://github.com/bevyengine/bevy/pull/18670) "Remote entity reservation"
  (ElliottjPierce). Handle-drop semantics were unified ahead of time in merged PR
  [#22261](https://github.com/bevyengine/bevy/pull/22261).
- The idea is cart's (Asset V2, [#8624](https://github.com/bevyengine/bevy/pull/8624), which
  *started* entity-backed and ported off). His first listed motivation in #8624: *"Easier to
  inline assets in Bevy Scenes (as they are 'just' normal entities + components)"* — i.e.
  the BSN use case has been the point from the beginning.

### What v0 (#22939) actually does

- **`Assets<T>` the resource is removed.** Each asset is an entity carrying an
  `AssetData<A>` component (monolithic on purpose in v0 — one component per asset type).
- System-facing compatibility layer: `Assets<A>` / `AssetsMut<A>` **SystemParams** backed by
  queries replicate the old `Res<Assets<A>>` API (`Res<Assets<Mesh>>` → `Assets<Mesh>` in
  signatures). `AssetMut` derefs to the inner `A` with change detection.
- `Handle<A>` becomes a refcounted reference to the asset **entity**; handles are
  effectively always strong; last-handle-drop despawns the asset entity. UUID handles go
  through an `AssetUuidMap` (UUID → entity). `AssetServer::load` remote-allocates the entity
  immediately; the actual spawn is deferred to `PreUpdate`.
- New `AssetCommands` SystemParam (`spawn_asset::<A>(value)`); **known footgun:** `Commands`
  and `AssetCommands` are separate queues that apply in param order
  ([#22885](https://github.com/bevyengine/bevy/issues/22885) tracks the fix).
- **`AssetEvent` survives v0** as explicit duct tape (re-emitted from ECS-side machinery),
  slated for deletion in favor of change detection / `AssetChanged` / observers later.
- Hot reload still writes into `AssetData<A>` on the same entity; `AssetChanged` keeps
  working. Load-state / dependency tracking stays in the `AssetServer` in v0.
- Acknowledged regressions: no same-frame asset access after `spawn_asset` (spawn is
  deferred; "permanent limitation"); loss of `impl Into<A>` in `add`
  (`meshes.add(Cuboid::default())` → explicit `Mesh::from(...)`); savers mint ids via
  throwaway entities.

### The staged roadmap after v0 (andriyDev, #11266 2026-01-24)

1. ~~generic entity refcounting~~ (folded into v0), 2. ~~UUID map~~, 3. **v0 = #22939**,
4. **v1: non-monolithic assets** — loaders produce *multiple components* on the asset entity
(design open: is `Asset` a marker? what happens to stale components on reload?), 5. delete
`LoadedUntypedHandle`; replace `AssetEvent` with observers/entity events; **track asset
dependencies as an entity graph**; maybe subassets as relationships.

### cart's explicit BSN integration plan

From cart's comment on [#23822](https://github.com/bevyengine/bevy/issues/23822) (creating
handles from runtime values in `bsn!`):

> 1. Merge assets-as-entities, which will enable spawning new assets in BSN.
> 2. Add support for "shared entities" in BSN: entities that are spawned once and reused
>    across instances.
> 3. Add support to BSN for getting a handle to an asset entity.
> 4. (optional) perhaps add syntax for doing this all inline.
> 5. Profit.

"Shared entities" is the **Scene-owned-entities** item on #23413's Near Future roadmap
("defining assets *inside* of scenes (this would pair nicely with Assets as Entities)").
And in [#24925](https://github.com/bevyengine/bevy/issues/24925) (July 2026) cart says:
*"currently `asset_value` doesn't support templates, as assets aren't entities (and
templates run on entities). Our plan for this is Assets as Entities (#23094), **which we
are working on now**."* The interim `HandleTemplate::Value(Arc<Mutex<...>>)` behind
`asset_value` is explicitly a stopgap he dislikes (mutex lock per spawn).

So: `bsn!`'s asset story is *deliberately blocked on* #23094. Every asset-related BSN issue
(#23822, #24925, #24965) is deferred to it.

## 2. Our coupling surface (inventory)

Dynamic BSN touches `bevy_asset` in a deliberately narrow set of places. Everything below
is the complete list of seams that #22939 and its follow-ups can move:

| # | Surface | Where | Coupled to |
|---|---------|-------|------------|
| 1 | `ScenePatch` asset shape (`scene`, `#[dependency] dependencies: Vec<UntypedHandle>`, `resolved: Option<Arc<ResolvedSceneRoot>>`) | `crates/bevy_scene/src/scene_patch.rs:19-30` | `Asset` derive, `UntypedHandle`, dependency-driven load state |
| 2 | Resolve pipeline trigger: `AssetEvent::LoadedWithDependencies` / `Removed`; `get_mut_untracked` to avoid self-emitted `Modified` | `crates/bevy_scene/src/spawn.rs:633-716` | `AssetEvent`, `Assets<T>` mutation semantics |
| 3 | Hot-reload dependent invalidation: manual reverse-dependency scan + `assets.reload(path)` | `spawn.rs:734-760` (`reload_dependents`), `patch_depends_on` | `Assets<T>` iteration, `AssetId` comparison |
| 4 | Instance bookkeeping: `WaitingScenes` (AssetId → waiting entities), `resolved_once: Local<HashSet<AssetId>>`, `ScenePatchInstance(Handle<ScenePatch>)` + `SceneInstanceState` | `spawn.rs` | `AssetId` keying |
| 5 | `HandleTemplate<T>` (`Handle<T>::Template`), `ReflectConvert` string→`HandleTemplate::Path` | `crates/bevy_asset/src/handle.rs:243-284`; `crates/bevy_bsn_asset/src/value.rs:130-148` | `Handle` internals hidden behind the template — good |
| 6 | Dependency registration at load: `Scene::register_dependencies` → `LoadFromPath::load_from_path_erased` → `ScenePatch::load_with` | `scene_patch.rs:41-53`; `crates/bevy_bsn_asset/src/loader.rs:119` | `LoadContext`, `UntypedHandle` |
| 7 | `CachedSceneAsset` (`:"base.bsn"`) — resolve/spawn-time lookups into `Assets<ScenePatch>`; the `Arc` on `resolved` exists so nested apply can borrow cached patches | `crates/bevy_scene/src/scene.rs:445-460`; `resolved_scene.rs` `include_cached` | `Assets<ScenePatch>` reads during resolve/apply |
| 8 | `DynamicBsnLoader` (async loader → `ScenePatch`), `report_scene_patch_load_failures` (`AssetLoadFailedEvent`) | `crates/bevy_bsn_asset/src/loader.rs` | `AssetLoader` contract, failure events |
| 9 | `SceneEntityReference::Asset { path_hash }` identity | SPEC-2/SPEC-0 §7 | asset *paths* only — unaffected |

Notably, `scene_patch.rs:27` already carries cart's TODO: *"consider breaking this out to
prevent mutating asset events when resolved. **Assets as Entities will enable this!**"* —
upstream anticipated exactly this interaction.

## 3. Impact under v0 (#22939 as written)

Mostly **mechanical churn, no design breakage**:

- `ResMut<Assets<ScenePatch>>` / `Res<Assets<...>>` system params → the new query-backed
  `Assets` / `AssetsMut` SystemParams. `ScenePatch::resolve(&self, assets, patches, ...)`
  and `spawn_queued`'s resource_scope choreography need signature ports.
- `AssetEvent<ScenePatch>` still exists in v0 (duct tape), so `resolve_scene_patches`'s
  `LoadedWithDependencies` trigger and our pinned reload semantics (commit e5dabde65)
  survive v0 unchanged. The trigger is isolated in one system, which is exactly where we
  want the migration cost concentrated.
- `get_mut_untracked`: needs an equivalent on the query-backed param (bypass change
  detection on `AssetData<ScenePatch>`). If v0 doesn't expose one, `DetectChangesMut::
  bypass_change_detection` on `AssetMut` should serve. This hack **stops being needed at
  all** once we do the resolved-split (§4.1).
- `Handle<ScenePatch>` / `UntypedHandle` in `dependencies` keep working (handles remain the
  currency; only their internals change). `dependency.id() == changed_untyped` comparisons
  keep working while `AssetId` exists, but ids are on a deprecation path
  ([#19024](https://github.com/bevyengine/bevy/issues/19024), `AssetId::invalid` removal) —
  prefer comparing handles/entities where we touch this code next.
- `HandleTemplate` is untouched by v0 (it's cart's own indirection and the reason templates
  will keep working); our `ReflectConvert` string→`HandleTemplate::Path` coercion is
  future-proof as-is.
- `DynamicBsnLoader` is unaffected: loaders still return the asset value; the server owns
  entity spawning (via remote allocation). `LoadContext`-based dependency registration
  (`LoadFromPath`) survives because dependency/load-state tracking stays in `AssetServer`
  in v0.
- Aliasing caution: `resolve_scene_patches` mutates one `ScenePatch` while reading others
  of the same type (`get_mut_untracked(id)` + `&patches` into resolve, for cached
  includes). Same-component queries can't split that borrow the way `Assets<T>`'s interior
  indexing does; v0's `AssetsMut` presumably mediates this, but it's the one spot where a
  query-backed storage is semantically tighter than a resource. The resolved-split (§4.1)
  dissolves the problem (read `AssetData<ScenePatch>` of deps, write a *different*
  component on self).

**Conclusion: nothing in our dynamic-bsn design fights v0.** The port is signatures plus
one borrow-shape review in `resolve_scene_patches`.

## 4. What assets-as-entities makes *better* for dynamic BSN

These are the post-merge opportunities, in rough order of value. Each one deletes bespoke
bookkeeping we currently hand-roll — which is the whole thesis of #23094.

### 4.1 `resolved` becomes a component on the asset entity (the scene_patch.rs:27 TODO)

Today `ScenePatch.resolved` is a cache field *inside* the asset, which is why
`resolve_scene_patches` needs `get_mut_untracked` (writing the cache must not look like the
asset changed) and why hot-reload trigger selection was delicate (`Modified` is
self-emitted by our own bookkeeping). With the asset as an entity:

- The resolver *inserts* a `ResolvedSceneRoot`-holding component on the asset entity
  instead of mutating `AssetData<ScenePatch>`. Loader writes and resolver writes are
  different components → change detection is honest for free; `get_mut_untracked` and the
  `Modified`-vs-`LoadedWithDependencies` reasoning both evaporate.
- Reload = loader rewrites `AssetData<ScenePatch>` → `AssetChanged<AssetData<ScenePatch>>`
  (or an observer) is a *clean* trigger with none of today's caveats; the stale resolved
  component is removed/replaced by the resolver.
- `resolved_once: Local<HashSet<AssetId>>` becomes "does the asset entity already have the
  resolved component" — a marker query, no side table.
- This is v0-compatible: v0 is monolithic about *loader output*, but asset entities are
  ordinary entities — runtime systems may attach components today. It's also exactly the
  shape non-monolithic v1 will want.

### 4.2 Dependencies become relationships; `reload_dependents` becomes a reverse query

`ScenePatch.dependencies: Vec<UntypedHandle>` plus `reload_dependents`'s full-scan reverse
lookup (`spawn.rs:734`, with its own PERF comment) plus `patch_depends_on` are a hand-rolled
dependency graph — precisely the "bespoke duplicate of the ECS" #23094 exists to delete.
Once deps are edges between asset entities (andriyDev's step 5 explicitly plans "track asset
dependencies as a graph"), `:"base.bsn"` runtime includes become relationship edges and
"who depends on the changed asset" is the built-in reverse-relationship lookup. Our current
scan is fine (human-paced reloads) but should be first in line for deletion. Bonus: upstream
notes this graph also fixes the general "dependents don't update when a dependency
reloads" class of bugs — our custom `reload_dependents` exists *because* the asset server
only walks loader deps; the upstream fix generalizes our workaround.

### 4.3 Instance→scene tracking via the ECS

`WaitingScenes` (AssetId → Vec<Entity> side tables) and the re-apply pass's "which
instances point at this patch" logic can become: `ScenePatchInstance` holds a handle, the
handle *is* an entity reference, so instance→asset is a traversable edge and asset→instances
is a reverse query (or an explicit relationship). Same deal for `SceneListPatch` waiting
maps. This removes the `Removed`-event cleanup dance in `resolve_scene_patches` too:
despawn of the asset entity can be observed directly.

### 4.4 The trigger surface migrates from events to observers

When upstream deletes `AssetEvent` (post-v0), `resolve_scene_patches` moves from
`MessageReader<AssetEvent<ScenePatch>>` to `AssetChanged` filters / observers. Our design
already concentrates every trigger decision in that one system and documents the semantics
(spawn.rs:623-632), so this is a contained rewrite. One thing to preserve: our pinned
"re-fire on every (re)load, exactly once" property (SPEC-6 / e5dabde65 tests) — those tests
are the safety net for the migration and are worth keeping green through it.

### 4.5 The endgame: `.bsn` assets as entity subtrees

Two deferred pieces of our own design line up eerily well:

- **The ECS-backed AST** (pcwalton's `BsnAst(World)`, deferred to the editor track in
  SPEC-0 Contract D) — under assets-as-entities, the natural home for it is *children of
  the `.bsn` asset entity*: one entity per `BsnNodeId`, queryable and observable, giving the
  editor a live, reflectable document. We kept `BsnNodeId` stable in document order exactly
  so this projection can be added later; assets-as-entities gives it an obvious anchor.
- **`SceneEntityReference::Asset { path_hash }`** identity could eventually become an
  entity-based identity (the asset entity + node id), removing the documented hash-collision
  caveat.

Combined with cart's step 2/3 (shared entities + handles to asset entities), the full
picture is: a `.bsn` file is an asset entity; its parsed nodes are entities; assets it
defines inline are shared asset entities; its instances and dependencies are relationships;
hot reload is change detection propagating along those edges. Dynamic BSN stops having any
bespoke storage at all.

## 5. What dynamic `.bsn` needs that upstream hasn't specced yet (grammar exposure)

cart's step 2–4 imply `bsn!` will grow syntax for (a) defining an asset/shared entity
inline, (b) referencing it as a `Handle`. Whatever that syntax is, **the `.bsn` text format
must be able to express it too**, and we are the ones defining `.bsn`'s v1 grammar.

- Our AST already has `EntityRef(String)` (`#Name`) and our builder has `build_entity_ref`
  targeting `EntityTemplate` fields. The natural extension: `#Name` in a `Handle<T>`-typed
  field position resolves to a handle to the named (shared/asset) entity — cart's step 3,
  expressed in existing syntax. No grammar change needed for the *reference* side.
- The *definition* side (marking an entity node as a shared/asset entity, or an inline
  asset value in a handle position à la `asset_value`) has no reserved syntax in our
  grammar. SPEC-0 §7 already records grammar-reservation risks (descendant patching,
  version pragma); **inline-asset/shared-entity syntax should be added to that list** and
  raised in the upstream BSN threads (#23576 / #23822) before v1 `.bsn` files proliferate.
- `asset_value(...)` (runtime asset values in `bsn!`) is out of scope for `.bsn` files by
  construction (SPEC-0 §2: no expressions in assets) — for text files, an inline asset is
  a *literal*, which is exactly cart's step 4. Nothing to do now beyond the reservation.

## 6. Recommended actions

**Now (on dynamic-bsn, before our upstream PR):**

1. **No structural changes.** Our coupling surface (§2) is narrow and sits behind
   `ScenePatch` + one system; pre-splitting `resolved` out of the asset *before* AaE would
   require a side resource keyed by AssetId — recreating the bespoke-table pattern AaE
   deletes. Not worth it; keep the seam documented instead.
2. **Add the grammar open question** (§5): reserve/discuss syntax space for shared-entity
   definitions and handle-to-entity references in `.bsn`, referencing cart's 5-step plan in
   #23822. Cheap now, expensive after v1 files exist. (Fold into SPEC-3's open questions
   11–13 alongside the version-pragma item.)
3. **Mention alignment in our upstream PR description**: our `.bsn` loader deliberately
   keys everything on `ScenePatch` + `HandleTemplate` + `LoadedWithDependencies`, all of
   which have direct v0 equivalents; and the scene_patch.rs:27 TODO's resolved-split is the
   first follow-up once #22939 merges. Reviewers (cart, andriyDev) will care.
4. **Avoid growing new `AssetId`-keyed side tables** in any further dynamic-bsn work;
   prefer handle/entity-shaped bookkeeping that ports cleanly.

**When #22939 merges (it's milestoned for our 0.20 window — expect it under us):**

5. Mechanical port: `Assets`/`AssetsMut` SystemParams, borrow-shape review of
   `resolve_scene_patches` (§3 aliasing note), keep SPEC-6's reload-semantics tests green.
6. First follow-up: resolved-as-component split (§4.1) — kills `get_mut_untracked`, the
   `resolved_once` local, and the `Modified` caveats in one move; realizes cart's TODO.
7. Opportunistic: `WaitingScenes` → queries/relationships (§4.3); `reload_dependents` →
   dependency-graph query when upstream's asset dep graph lands (§4.2).

**Watch list:** #22939 (v0 merge + conflict resolution), #23094 (design docs when written —
none exist yet; the working group is on Discord), #19024 (UUID/weak handle removal — touches
our `dependency.id()` comparisons), #22885 (Commands/AssetCommands ordering), #23576
(pcwalton's `.bsn` draft — grammar reconciliation), #23822 (the 5-step plan thread — where
to raise §5).

## 7. Key sources

- #23094 goal issue; #11266 original issue (staged plan, monolithic-vs-async debate);
  PR #22939 (v0 implementation + review threads); PR #8624 (cart's original rationale);
  PR #18670 (remote entity reservation); PR #22261 (handle unification; cart's
  canonical-handle design comments); #19024 (always-strong handles); #23822 / #24925 /
  #24965 (BSN×assets, cart's integration plan); PR #23413 (scene system, Scene-owned
  entities roadmap); alice's "*-as-entities" principles (HackMD @bevy/SypE1qZP1l).
