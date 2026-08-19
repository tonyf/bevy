# Assets as Entities — the Ideal-World Design

**Status: design essay / north star, 2026-08-19.** This is the no-legacy, no-migration
answer to "what should assets-as-entities be?" It is deliberately unconstrained by Bevy's
current code and by upstream's staged plan (#23094 / PR #22939) — see §8 for how the real
roadmap approximates this and where it stops short. Companion doc:
`assets-as-entities-and-dynamic-bsn.md` (upstream state + migration analysis for our
branch).

## 0. Premise

The ideal implementation of "assets as entities" is the one where **the asset system stops
existing as a system**. Today's `bevy_asset` is a parallel engine: bespoke storage
(`Assets<T>`), bespoke identity (`AssetId`, UUIDs, paths, labels), bespoke change
detection (`AssetEvent`), bespoke lifetime management (Arc-counted handles), bespoke
dependency tracking (`AssetInfos`). Each of these duplicates something the ECS already
does. Done fully, assets-as-entities dissolves every one of them into an existing ECS
concept, and what remains is not an asset system but a thin **I/O layer** plus a set of
conventions. The sections below are the six dissolutions.

## 1. "Asset" stops being a kind of type and becomes a kind of reference

There is no `Asset` trait and no `AssetData<A>` wrapper. A mesh asset is an entity with
`Mesh` on it — the same `Mesh` component a game entity could carry inline. The
asset/component type split disappears; what makes something an asset is not its type but
its **sharedness**: many entities reference one.

Consequences:

- An "inline asset" and a "shared asset" are the same data in the same component.
  Hoisting an inline value into a shared entity is a refactor (move the component, add an
  edge), not a type migration through `Assets::add`.
- Asset types need no dual identity (`Mesh` the asset vs `Mesh3d` the reference wrapper).
  The reference side is handled by §2; the data side is just the component.
- This is the fully-general form of what cart is circling with inline-assets-in-BSN and
  scene-owned entities (#23413 "Near Future", #23822): once assets are entities, the
  question "is this a mesh asset or just a mesh?" should not have an answer.

The one convention that survives is a `Shared` (or `AssetOf`-style) marker so tooling and
queries can distinguish "entity that exists to be referenced" from "entity that owns its
data" when they need to (§7 discusses the cost of not having a type-level split).

## 2. Handles dissolve into relationships; GC becomes graph reachability

Today `MeshMaterial3d(Handle<M>)` is a component holding an opaque refcounted token.
Semantically it is an edge, so ideally it **is** an edge: a relationship
(`UsesMaterial(Entity)`), with the usual relationship machinery behind it.

What this buys:

- **Reverse queries for free.** "Which entities use this material?" is the built-in
  reverse-relationship lookup — no `AssetEvent` broadcast, no side tables.
- **Change propagation along real edges.** A reloaded texture dirties its dependent
  materials by walking incoming edges, not by every consumer independently polling events.
  (This is the general fix for the "materials don't update when a dependency reloads"
  bug class upstream tracks, and the general form of our branch's hand-rolled
  `reload_dependents`.)
- **Reference counting disappears for in-world references.** The ECS already knows every
  edge. An asset entity is live iff reachable from a live root:
  - incoming-edge counts maintained incrementally (cheap, exact for the acyclic common
    case);
  - a `Retain` marker component for explicit roots — replacing `mem::forget` shader-library
    hacks with an inspectable, queryable statement of intent;
  - a rare mark-and-sweep pass over the asset subgraph for cycles — which Arc-based
    refcounting *cannot ever collect*, so this is strictly more powerful, not merely
    cleaner.
  GC runs batched, once per frame, under one lock.
- **Canonical state lives on the entity, not the handle.** This dissolves the
  cart-vs-andriyDev debate from PR #22261 (should handles carry canonical state?): neither
  side. Refcount, load state, generation, and path are components on the asset entity,
  where they are queryable and debuggable. The handle stays as dumb as possible.

The Arc-style `Handle` survives in exactly one place: the **world boundary**. References
held from resources, other threads, or mid-flight async loads are invisible to the ECS, so
an external-root token (an Arc whose drop enqueues a root-release) remains. It is one
small, honestly-scoped piece of non-ECS machinery — the *only* one in the design.

## 3. Loading is an ECS state machine; the AssetServer dissolves

To load something, you spawn an entity with a `Source` component —
`Source::Path("x.png")`, `Source::Bytes(..)`, `Source::Url(..)` — and the machinery is
ordinary systems and observers:

- An observer on `Source` insertion kicks off async I/O and inserts `Loading`.
- The loader's output lands as components on that same entity (§4), replacing `Loading`
  with `Ready`, or with `Failed(error)` — the error queryable, not just logged.
- Dependencies discovered during the load become `DependsOn` edges (§2), and a
  `DepsReady` marker is maintained incrementally by propagation along those edges.
  "Loaded with dependencies" is not an event type anyone defines; it is an observer on
  `DepsReady` insertion.
- Retry, timeout, prioritization, and throttling policies are user-replaceable systems
  over these components — not hardcoded server behavior.

What remains of the `AssetServer` is an **identity index** (path → entity, for dedup),
maintained like any other index with a despawn hook, plus the I/O sources themselves
(`AssetReader` backends). Every field of today's `AssetInfos` — load state, dep sets,
refcounts — is a component or an edge. The entire class of "the server and the world
disagree" bugs (the manual-despawn footguns found in PR #22939 review) becomes
*unrepresentable*: there is no second store to disagree with.

`asset_server.load("x.png")` survives as sugar: dedup-lookup in the index, else
`spawn((Source(path), …))`, returning the entity (plus an external-root handle if the
caller is outside the world).

## 4. Loaders produce entity subtrees; identity survives reload

A GLTF is not a value with labeled sub-assets bolted on — it is a graph. The ideal loader
output **is an entity subtree**: the file's root entity, with child entities for meshes,
materials, textures, animations, each carrying `Label` components. The entire labeled
sub-asset apparatus (`LoadedUntypedHandle`, `path#Label` special-casing) dissolves into
path resolution over children — `"model.gltf#Mesh0/Primitive0"` is a name lookup, not a
type-system feature.

Hot reload is **declarative re-materialization with an ownership rule**:

- The load machinery records which components (and child entities) the loader produced.
- On reload, that set is replaced declaratively — stale loader-owned components and
  children are removed, new ones inserted.
- Components *users* attached to the asset entity (gameplay metadata, computed AABBs,
  editor annotations) survive, because they are outside the loader-owned set.

And because the asset *is* the entity, **reload never changes identity**. Every reference
to it — in-world edge, external handle, serialized name — stays valid across any number of
reloads. No handle generations, no dangling ids, no re-resolution pass. This is the single
biggest quality-of-life win over any handle-generation scheme, and it falls out of the
architecture rather than being implemented.

The processing/import pipeline is the same shape one level up: processed assets are
entities with `DependsOn` edges to their source assets, `.meta` settings are components,
and "needs reprocessing" is change detection along the edges.

## 5. Identity unifies: one "stable name → entity" concept

Bevy today has at least four "stable name for a thing" mechanisms:

1. asset paths (`"textures/x.png"`),
2. UUID handles (`weak_handle!`, default handles),
3. scene entity references (`SceneEntityReference` callsite/asset identity),
4. BSN's `#Name` refs and `:"file.bsn"` includes.

In the ideal world these are **one concept**: a stable name that resolves to an entity,
with the identity index (§3) as its resolver. A handle-typed field in a `.bsn` file, a
`:"base.bsn"` include, a texture reference inside a material file, and a `#Name` ref are
all the same operation — *name → entity* — differing only in which components the resolver
expects to find there. UUID handles reduce to names in a reserved namespace; "default"
handles become template-resolved lookups at insert time (cart's stated plan in #19024)
rather than magic consts.

Seen this way, assets-as-entities is really the statement that **the asset system is the
ECS's sharing and persistent-identity layer, and the scene system (BSN) is its
serialization syntax**. cart's 5-step integration plan (#23822: merge AaE → shared
entities in BSN → handles to asset entities → inline syntax) converges on exactly this
point; the ideal world arrives there without the intermediate handle machinery.

## 6. The render world mirrors, rather than re-keys

GPU residency is components on render-world mirrors of asset entities — `GpuMesh`,
`Residency::Uploaded { .. }` — managed by ordinary systems: upload on
`AssetChanged`-equivalent ticks, evict by LRU query, prefetch by relationship walk from
visible entities. The `RenderAssets<A>` hashmaps disappear the same way `Assets<T>` did,
and for the same reason: they are bespoke keyed storage shadowing entity data. Extraction
becomes the generic main-world→render-world entity mirroring the renderer already needs,
applied to one more class of entity.

## 7. The honest tensions

Ideal does not mean free. Three costs are real and should be named:

- **Query bleed.** `Query<&Mesh>` now matches shared assets and inline meshes alike.
  Sometimes that is precisely the point (visualization tools, batch processing); sometimes
  it is a hazard (a system "fixing up" every mesh corrupts shared ones N times). The
  ecosystem needs `With<Shared>` / `Without<Shared>` discipline, and engine-provided
  queries must be exemplary about it. This is the price of dissolving the type split (§1),
  and it is a *convention* cost where the old design paid a *duplication* cost.
- **Deferred visibility.** Spawning is deferred, so "add an asset and read it back in the
  same system" is impossible. Every design in this space pays this (upstream v0 calls it
  a permanent limitation); the ideal world just stops pretending otherwise. Entity ids
  *are* available synchronously (remote allocation), so handles/edges can be created
  immediately — only the data read waits a sync point.
- **Async construction is the hard part.** Loaders building entity *subtrees* (§4) from
  async contexts need more than remote id allocation: either staging worlds merged at
  sync points, or first-class remote *construction* primitives (build a bundle graph
  off-thread, apply atomically). This is exactly why upstream's v0 is monolithic — the
  subtree contract is the piece with no merged primitive behind it. An ideal
  implementation invests here **first**, because §§1–5 all lean on subtree output.

Two second-order concerns, judged acceptable:

- **Archetype/table shape.** Assets with heterogeneous extension components fragment
  archetypes; huge blobs live in tables. Both are fine in practice — blobs are `Vec`s
  internally (the table stores pointers-sized headers), and asset-entity archetype count
  is bounded by loader output shapes, not asset count.
- **GC cost.** Incremental in-degree tracking is O(edge mutations); mark-and-sweep is
  confined to the asset subgraph and only needed for cyclic asset references, which are
  rare and currently *leak silently* — a scheduled sweep is an upgrade, not a regression.

## 8. Gap analysis: the ideal vs upstream's staged plan

Upstream's sequencing (#11266 staged plan → #23094 goal, PR #22939 as v0) is a
migration-shaped approximation of this design. Where each piece lands:

| Ideal-world piece | Upstream status |
| --- | --- |
| §1 no wrapper, sharedness not type | Not planned. v0 keeps `AssetData<A>` monolith; v1 goes multi-component but keeps the `Asset` concept |
| §2 references as relationships | Partially: "deps as a graph" and "subassets as relationships" are step-5 ideas; consumer-side handle fields stay `Handle<T>` |
| §2 GC by reachability | Not planned. v0 is Arc-refcount + despawn-on-zero; cycles remain uncollectable |
| §2 canonical state on entity | Directionally yes ("move more AssetServer metadata into the ECS"), unscheduled |
| §3 loading as ECS state machine | Not planned as such; load state stays in `AssetServer` through v0; `AssetEvent` → observers is step 5 |
| §4 loader subtrees + ownership rule | v1 ("non-monolithic assets"), design open — the reload-staleness question is exactly the ownership rule |
| §4 identity survives reload | **Already achieved by v0** (reload writes into the same entity) — the first ideal property to land |
| §5 unified identity | Emergent across #19024 (kill UUID handles), #23822 (BSN refs), no unifying doc |
| §6 render mirrors | Explicitly out of scope; v0 keeps `RenderAssets` re-keyed |

Reading the table: upstream is converging on §§2–5 piecewise, from the storage end inward;
the ideal design derives the same endpoints from the identity/reference end. The two
biggest things the staged plan risks locking in against the ideal are (a) `Handle<T>`
fields as the permanent consumer-side reference (vs relationships), which shapes every
component API, and (b) the ownership rule for loader output (§4) being retrofitted after
v1 ships rather than designed in — the retrofit cost falls on every loader author.

## 9. What this means for dynamic BSN

Our branch has accidentally rehearsed most of §3 with side tables and events:

- `resolve_scene_patches` (`bevy_scene/src/spawn.rs:633`) is a hand-rolled §3 state
  machine — `LoadedWithDependencies` standing in for a `DepsReady` observer, the
  `resolved_once` local standing in for a `Ready`-style marker, `get_mut_untracked`
  standing in for "resolver output is a different component than loader output".
- `reload_dependents` (`spawn.rs:734`) fakes §2's reverse edges with a full scan.
- `WaitingScenes` fakes the instance→asset edge that `ScenePatchInstance`'s handle would
  *be* under §2.
- The deferred ECS-backed AST (SPEC-0 Contract D, stable `BsnNodeId`s) is §4 applied to
  `.bsn` files: the document as a queryable entity subtree under the asset entity.
- `SceneEntityReference::Asset { path_hash }` and `#Name` refs are §5 waiting to happen.

So the ideal world is, concretely: what dynamic-bsn does with side tables and events,
done with edges and observers, engine-wide. Each upstream step that lands (v0 storage, dep
graph, observers) lets us delete one of the stand-ins — the migration map in
`assets-as-entities-and-dynamic-bsn.md` §4 is the per-step schedule for that.

## 10. Sources

Grounding for the upstream claims: goal issue #23094; original issue #11266 (staged plan);
PR #22939 (v0 + review threads — manual-despawn footguns, saver hack, AssetCommands
ordering); PR #8624 (cart's original assets-as-entities rationale in Asset V2);
PR #22261 (canonical-handle debate); PR #18670 (remote entity reservation); #19024
(UUID/weak handle removal, Construct-based default handles); #23822 / #24925 (cart's
BSN×assets 5-step plan); PR #23413 (scene-owned entities roadmap); #16041 / #21058
(AssetEvent → observers); alice's "*-as-entities" principles (HackMD @bevy/SypE1qZP1l).
