# Loom — an Incremental Kernel for Bevy

**Status: design essay / north star, 2026-08-19.** Capstone of the ideal-world series.
Companion docs: `assets-as-entities-and-dynamic-bsn.md` (upstream state + migration
analysis) and `assets-as-entities-ideal-design.md` (assets dissolved into ECS concepts).
This doc goes one level up: assets, scenes, reactivity, hot reload, and render extraction
are not five features — they are one problem, **incremental recomputation over a
dependency graph** — and Bevy already owns the two primitives an incremental engine
needs: change ticks (dirtiness for free) and relationships (edges for free). "Loom" is
the working name for the small kernel that unifies them.

## 0. The five commitments

1. **One incremental kernel (~3k lines); everything else is registrations.** No
   subsystem gets its own invalidation scheme ever again.
2. **Entity = identity, dense index = performance.** Graph nodes are entities for
   identity and tooling; propagation never chases pointers through archetypes.
3. **Mark-and-sweep with stratified levels and early cutoff at every level.** Never
   eager observer cascades.
4. **Scenes are compiled, not interpreted.** The hot path applies a baked,
   archetype-ready plan.
5. **Reactivity uses static read-sets extracted from BSN, not runtime dependency
   tracking.** Possible because BSN templates are analyzable data, not opaque closures.

The design is salsa/adapton (incremental computation with memoization and early cutoff)
fused with React (retained instances + keyed reconciliation), implemented natively on an
ECS instead of alongside one.

## 1. The kernel

### 1.1 Nodes, edges, and the side index

Every participating thing — a `.bsn` source file, a parsed AST, a typed IR, a resolved
scene subtree, a material, a live instance, a GPU buffer — is a **node**, and a node is
an entity. Dependencies are relationship edges. But the kernel's *working
representation* is a dense side index it owns, maintained incrementally by hooks —
exactly the pattern Bevy already uses for the archetype graph and query caches:

- **CSR adjacency** (dependents-of), compact `u32` node ids mapped ↔ entities,
  incrementally patched on edge insert/remove.
- **Per node:** a *level*, a *version*, and a *fingerprint*.
  - Level = `max(level of inputs) + 1`, maintained incrementally. An edge insertion that
    would violate levels is a **cycle → hard error at insertion time**, never a livelock
    (the kernel-wide generalization of our `.bsn` self-include check).
  - Version = monotonic `u64`, bumped **only when the node's value actually changes**
    (not on every recompute).
  - Fingerprint = hash of input versions observed at last recompute.
- **Dirty state:** one bitset (dedup) + one `Vec<u32>` queue per level. No hashmaps on
  the hot path.

### 1.2 The sweep

One new schedule phase, **Propagate**, drains dirty queues level 0 → N. Within a level,
dirty nodes are grouped **by kind** (parse nodes, bind nodes, resolve nodes, bake nodes,
upload nodes, ...), and each kind's recompute runs as an ordinary Bevy system over its
batch. Consequences:

- **Parallelism for free**: within a level, kinds run under the existing parallel
  scheduler; recomputes of one kind are cache-coherent (same component types).
- **Glitch-freedom**: level order guarantees each node recomputes at most once per sweep
  regardless of how many of its inputs changed (no diamond-problem double updates).
- **Determinism**: level order + stable within-level ordering (by node id) + explicit
  kind registration order. No emergent observer-firing order.

The recompute protocol applies salsa's crucial trick twice:

1. **Skip check**: if current input versions equal the fingerprint, don't run at all.
2. **Early cutoff**: if the recomputed value hashes equal to the previous value, don't
   bump the version — propagation stops dead at this node.

Edit a comment in a `.bsn` file: one parse runs, produces a hash-identical AST, version
doesn't bump, and nothing downstream — bind, resolve, bake, fifty live instances — even
wakes up.

### 1.3 Marking, and the division of labor

- **Observers/hooks mark, and only mark**: O(1) enqueue into a dirty queue. No user code
  runs at mark time, so there are no cascades and no reentrancy.
- **Change ticks mark sources**: a bounded per-frame scan over *registered source
  components only* (never a world-wide poll) turns tick changes into dirty marks.
  Interior nodes are marked exclusively by edge propagation.
- **Messages are transport, not notification**: batched carriage across boundaries —
  between levels when queuing is needed, between worlds (§5), to disk (§6). Ticks and
  marks do all signaling.
- **Writes during the sweep** that target lower levels queue for the *next* frame:
  bounded latency, provably no infinite propagation. A genuine feedback loop is a level
  violation and surfaces as an error, not a hang.
- **Errors are values**: a failed recompute installs `Failed(error)` on the node and
  keeps serving its last good value downstream — the degrade-to-last-good behavior our
  `.bsn` resolve implements by hand today (`resolve_scene_patches` leaving stale
  `resolved` intact), generalized kernel-wide.

### 1.4 Cost model

| Situation | Cost |
| --- | --- |
| Nothing changed | **Zero.** No polling, no per-frame scans beyond registered source ticks (bounded), no allocation |
| One value changed | O(affected subgraph ∩ actually-changed), with early cutoff pruning at every node whose recomputed value is identical |
| N changes to one node in a frame | One recompute (coalesced by the bitset) |
| Spawning the 1000th instance of an unchanged scene | Baked-plan replay: one archetype move + near-memcpy per entity; zero reflection, zero registry lookups, zero merging (§2) |
| Hot reload of a large scene with a one-line edit | One parse + one bind delta + one subtree re-resolve + one plan diff + writes to only the touched entities (§2, §4) |
| Memory overhead | CSR (u32s per edge) + 3 u64-class words per node (level/version/fingerprint) + interned Arc'd values with structural sharing |
| Worst case | Never O(world), never O(scene size) — always proportional to the changed frontier |

## 2. Scenes as a compiler pipeline

The scene path is five memoized nodes, each independently subject to early cutoff:

```text
bytes ─parse→ AST ─bind→ typed IR ─resolve→ resolved subtree ─bake→ spawn plan ─reconcile→ live instance
```

- **Parse** — text → AST with stable `BsnNodeId`s (our `bevy_bsn` crate, as-is).
- **Bind** — resolve symbols against the type registry **once** (dynamic-bsn's SPEC-4
  already does all registry work at load, none at spawn), producing a typed IR. Bind
  also extracts each template's **read-set** (§3) — the prop and asset fields it reads —
  as static graph edges.
- **Resolve** — merge bases field-by-field into **immutable, interned, Arc'd subtrees**,
  memoized per subtree. A `:"base.bsn"` shared by fifty scenes exists once in memory;
  a reload changing one entity of the base re-resolves one subtree, and forty-nine
  dependents cut off at the fingerprint check. (Our copy-on-write cached `ScenePatch`
  templates are the seed of this.)
- **Bake** — the new stage and the performance centerpiece: lower a resolved subtree to
  an **archetype-ready spawn plan** — per scene entity, the final sorted component-id
  set and one contiguous value blob, precomputed so application is a single archetype
  move and a near-memcpy per entity. This promotes dynamic-bsn's
  single-archetype-move invariant (`push_to_bundle_writer`) from "property defended in
  review" to "shape of the data."
- **Reconcile** — diff old plan vs new plan **keyed by stable `BsnNodeId`**, with
  subtree-hash skipping (unchanged-hash subtrees are not visited), emitting minimal
  batched writes to live entities linked via `SpawnedFrom` edges.

Reconciliation collapses construction and dynamics into one operation:

- **First spawn = reconcile against empty.** There is no separate spawn path.
- **Hot reload = reconcile against previous.** Runtime state, entity ids, and
  user-added children survive because the diff never mentions them. Our current
  despawn-and-respawn (`SceneInstanceState::spawned`) is the degenerate reconciler
  ("diff = everything").
- **Prop change = reconcile the affected subtree** (§3).
- **Two-way editing** (jackdaw/world→BSN territory) is the same edges walked backwards:
  every live entity is `SpawnedFrom`-linked to its AST node.

The ownership question — which state is scene-owned vs runtime-owned — is *shared* with
the asset loader ownership rule (`assets-as-entities-ideal-design.md` §4) rather than
being a second hard problem. One rule, two clients.

## 3. Reactivity: static read-sets, because BSN is data

Signals-style frameworks (SolidJS, MobX, fine-grained reactivity generally) pay for
runtime dependency *recording* on every execution because their templates are opaque
closures. **BSN templates are analyzable data.** At bind time we know exactly which prop
components and asset fields each template reads, so the read-set becomes **static edges
registered once**, with zero per-execution tracking.

The loop: prop component write → tick marks the prop's source node → sweep re-runs
precisely the template nodes that declared that read → reconciler patches the affected
entities. That single loop *is* UI reactivity (feathers widgets), *is* prefab overrides,
*is* editor live-edit. Event-shaped logic stays with `on(...)` observers; genuinely
dynamic derivations (closures) opt into recorded tracking and pay for what they use. The
95% case pays nothing.

This is a strictly better deal than any signals runtime, and it is only available
because the scene language is a value format instead of code — the design bet cart made
with BSN, cashed. Dynamic-bsn's `register_dependencies` (static dep declaration, no
runtime discovery) is the same principle already shipping for asset deps.

## 4. Assets and GC on the kernel

Per `assets-as-entities-ideal-design.md`, restated as node kinds:

- An asset is a source node (`Source::Path/Bytes/Url`) plus loader-produced derived
  nodes (the entity subtree), under the loader-owned/user-owned ownership rule. Reload
  re-materializes loader-owned components on the **same entity** — identity survives
  forever; references never dangle.
- "Loaded with dependencies" is not an event anyone emits: it is "all my level-k inputs
  have versions," a property the kernel already tracks. Dynamic-bsn's
  `resolve_scene_patches` trigger becomes a bind/resolve node kind.
- **GC = in-degree counts on the CSR** — the edges are already there. Batched sweep at
  frame end, incremental cycle collection amortized across frames (collects cyclic asset
  references that Arc counting leaks silently today), external handles as pinned roots.
  No Arc traffic on in-world references at all.

## 5. The render world: extraction as a cross-world edge

Render-world mirror nodes depend on main-world node versions. The per-frame extract
ships **one message batch per kind** containing only version-bumped nodes — this is the
message system's true role: batched transport across boundaries, not notification.
`RenderAssets`, render-world `AssetEvents`, and per-plugin extract bookkeeping collapse
into it. GPU upload is a recompute kind; residency and eviction are ordinary systems
over mirror nodes; visibility-driven prefetch is edges from camera nodes.

## 6. Simplicity, measured in deletions

What one kernel replaces: `AssetServer` internals and `AssetInfos`, `AssetEvent` (both
worlds), the scene spawner's queues and `WaitingScenes` tables, dynamic-bsn's
`reload_dependents` reverse scan and `resolved_once` bookkeeping, `RenderAssets` +
hand-rolled extraction, per-feature hot-reload plumbing, and the future category of
third-party reactivity crates. Each becomes a node-kind registration of roughly a page.

Because nodes are entities, **the debugger is a query**: "why did this recompute?" is a
provenance walk renderable in the editor; per-kind-per-level timings come out of the
profiler labeled; the dependency structure is visible in the inspector that already
exists. And the version stream is a byproduct with two free applications: an incremental
save format (persist bumped versions) and network replication feeds.

## 7. Honest tensions

- **Frame latency**: batched propagation lands next sweep, not instantly. Right default
  for an engine; an opt-in immediate mode covers the rare same-frame need at the cost of
  batching.
- **Edge churn**: UI that restructures every frame must not thrash level maintenance.
  Mitigations: levels are per-subgraph; UI subgraphs get a reserved dense level band;
  edge sets for template read-sets are static (§3), so churn is confined to structural
  reconciliation.
- **Hash costs**: early cutoff requires hashing recomputed values. Mitigations: shallow
  hashes over interned Arc ids (structural sharing makes deep hashing unnecessary);
  kinds can opt out of cutoff where hashing exceeds recompute cost.
- **Ownership semantics** (scene-owned vs runtime-owned vs loader-owned) remains the one
  genuinely hard *semantic* design. It is deliberately a single shared rule (§2, §4);
  villor's reconciliation work (cart/bevy#36) is the prior art to build on.

## 8. Validation order

1. **Bake**: prove the archetype-ready plan format against relationship spawning and
   `EntityTemplate` cross-references — it carries the headline perf claim.
2. **Reconciliation ownership**: nail the scene-owned/runtime-owned rule with
   state-preservation tests (dynamic-bsn's SPEC-6 parity suites are the seed corpus).
3. **Level maintenance under churn**: benchmark stratification against a
   restructure-every-frame UI workload before committing to the incremental-level
   algorithm.

## 9. Gap analysis: what exists today

| Loom piece | Upstream today | Our dynamic-bsn branch today |
| --- | --- | --- |
| Dirty marking | change ticks (shipped), `AssetChanged` filter | `LoadedWithDependencies` as a hand-rolled mark |
| Edges | relationships (shipped); asset dep graph is #11266 step 5 | `ScenePatch::dependencies` Vec + `reload_dependents` full scan |
| Stratified sweep | — (observers are eager; no level scheduler) | miniature 2-level version: loader → `resolve_scene_patches` ordering |
| Memoization + early cutoff | — | COW cached `ScenePatch` templates; `resolved` cache (subtree-level seed) |
| Stable reconciliation keys | — (scene reload is respawn) | `BsnNodeId` stable in document order; `SceneEntityReference::Asset` identity |
| Baked spawn plans | `ResolvedScene` (merged, but reflection-adjacent at spawn) | single-archetype-move invariant via `push_to_bundle_writer` |
| Reconciliation | villor's reconciliation exploration (cart/bevy#36, not merged) | despawn-and-respawn with `SceneInstanceState` bookkeeping (degenerate diff) |
| Static read-sets | — (`on(...)` observers only) | `register_dependencies` (asset deps only, same principle) |
| Assets as nodes | PR #22939 v0 (open, milestoned 0.20) | analyzed in companion docs |
| Cross-world edges | bespoke extraction + `RenderAssets` | n/a |
| Last-good error values | — | `.bsn` resolve failure keeps last good `resolved` |

Reading the table: the kernel's contracts have all been field-tested somewhere — five of
them in miniature on our own branch (static dep registration, COW memoization, stable
keys, last-good errors, mark-then-sweep). Loom is those conventions moved from
`bevy_scene` into the engine's core loop and given the data structures they deserve.

## 10. Crate structure

The organizing rules are the ones the workspace already lives by: one concept per crate;
a strict dependency DAG; the algorithmically hard, bevy-independent parts extracted into
standalone leaf crates (the `bevy_bsn` precedent — fuzzable, miri-able, reusable by
external tooling); and the ECS core gains only true primitives, never subsystems.

```text
bevy_loom_graph          (no_std + alloc, ZERO bevy deps: CSR, levels, versions,
    │                     fingerprints, dirty queues, cycle detection, cutoff protocol)
    ▼
bevy_ecs                 (unchanged core + ONE addition: the spawn-plan / baked-bundle
    │                     primitive, beside templates & BundleWriter where it belongs)
    ▼
bevy_loom                (the kernel binding: entities ↔ node ids via hooks, node kinds
    │                     as systems, the Propagate phase plugin, provenance API)
    ▼
bevy_asset               (rebuilt thin: io backends, identity index, Source/loader
    │                     node kinds, CSR-based GC — the AssetServer god-object is gone)
    ▼
bevy_scene               (the scene compiler: typed IR, bind + read-set extraction,
    │  ▲                  resolve, bake, reconcile — all as node kinds; spawner deleted)
    │  └── bevy_bsn      (unchanged: standalone parser/printer, zero bevy deps)
    │  └── bevy_bsn_asset (the `.bsn` frontend: parse + lower to IR; `bsn!` is the
    ▼                     macro frontend to the same IR)
bevy_render / bevy_ui    (mirror + extract + upload node kinds; reactive widget kinds;
                          RenderAssets and bespoke extraction deleted)
```

Placement calls worth defending:

- **`bevy_loom_graph` is standalone and bevy-free.** Incremental level maintenance,
  cycle detection, and the cutoff protocol are the kernel's riskiest algorithms (§8);
  they should be property-tested and fuzzed against plain `u32` ids with no ECS in the
  loop. Same playbook that made `bevy_bsn` tractable.
- **`bevy_loom` is a separate crate, not a `bevy_ecs` module.** The ECS core is already
  large, the kernel will iterate fast in its early life, and nothing in `bevy_ecs`
  depends on it — ticks, hooks, and relationships are used *by* the kernel, not the
  reverse. Keeping it out preserves `bevy_ecs`'s review surface and lets the kernel
  version independently while it stabilizes.
- **The spawn-plan format is the exception: it goes *in* `bevy_ecs`.** Baked plans are
  archetype-shaped (sorted component-id sets, contiguous blobs, single archetype move) —
  that is bundle machinery, sits naturally beside `template.rs` and `BundleWriter`, and
  will be wanted by consumers other than scenes (command batching, network snapshot
  application).
- **`bevy_asset` keeps its name and shrinks.** Its remaining jobs — io backends,
  path→entity identity index, loader orchestration kinds, GC policy — are real and
  cohesive. An optional further split of `bevy_asset::io` into `bevy_asset_io` would
  serve headless/server tooling that wants readers and watchers without the kernel.
- **There is deliberately no `bevy_reactive` and no hot-reload crate.** Reactivity is
  bind's static read-sets plus the kernel; hot reload is a source-node version bump plus
  reconcile. Both are *emergent from the DAG*, and giving either a crate would recreate
  the parallel-subsystem pattern this design exists to delete. The absence is the
  architecture.
- **Frontends stay plural and thin.** `bevy_bsn_asset` (text) and the `bsn!` macro
  (Rust) both lower to `bevy_scene`'s one typed IR — mirroring how our branch already
  splits parser / lowering / scene core, which is why the current crate boundaries
  survive this redesign almost untouched.
- **Diagnostics ride in `bevy_dev_tools`**: graph inspector, "why did this recompute"
  provenance walks, per-kind-per-level timings — all queries over kernel state, no
  privileged access needed.

## 11. Sources

Bevy grounding: change ticks / `AssetChanged`; relationships; observers/hooks;
`Messages`; PR #23413 (BSN core, templates, `register_dependencies`); PR #22939 /
#23094 (assets as entities); cart/bevy#36 (villor's reconciliation); #23639 (jackdaw,
world→BSN); dynamic-bsn SPEC-0..6 (this repo, `dev-docs/dynamic-bsn-specs/`). Prior art:
salsa / adapton (memoized incremental computation, early cutoff, durability levels);
React (retained instances, keyed reconciliation); Incremental (Jane Street; stratified
stabilization); build systems à la carte (Mokhov et al.; fingerprints and cutoff
taxonomy).
