# SPEC-0: Dynamic BSN — Master Architecture & Interface Contracts

**Status: NORMATIVE.** This document is the source of truth for the spec series. Individual
specs (SPEC-1..6) MUST conform to the interface contracts here. If a spec author believes a
contract is wrong, they record it under "Open Questions" in their spec — they do NOT change
the contract unilaterally.

Target codebase: `/home/tony/workspace/bevy` (Bevy main, 0.20.0-dev, post-0.19).

## 1. Goal

Implement "dynamic BSN": a first-party `.bsn` asset loader and the runtime, reflection-driven
scene representation behind it, so that `asset_server.load::<ScenePatch>("player.bsn")` works,
`bsn! { :"player.bsn" }` inheritance works, and the result is fully interoperable with
statically-defined `bsn!` scenes (patches on the same component merge field-by-field).

The design deliberately follows the upstream direction already endorsed by cart:
pcwalton's draft PR bevyengine/bevy#23576 (custom lexer + parser, ECS-backed AST,
reflection-driven resolution producing the existing `ScenePatch` asset), adapted to
current `main` (post crate-renames, by-value `Scene::resolve`, `BundleWriter` apply path).

## 2. Non-goals (explicitly out of scope for the whole series)

- `use` imports, import versioning, schema migrations (deferred upstream; #14437 post-MVP).
- Calling registered functions from `.bsn` files.
- Closures, observers (`on(...)`), `{ expr }` blocks, Rust consts — not expressible in assets.
- Scene components (`@Widget`) invocation from `.bsn` — `SceneComponent::Props` is not
  `Reflect`; a `.bsn` file naming a `@Type` template patch IS in scope (see SPEC-3 grammar),
  but invoking `SceneComponent::scene(props)` is not.
- World→BSN write-back (jackdaw #23639 territory), asset catalogs (#23648), binary format.
- State-preserving reconciliation (villor cart/bevy#36). Hot reload in this series is
  re-resolve + re-apply (despawn/respawn of scene-owned descendants), documented as such.
- Editor `SceneDocument` APIs.

## 3. Spec series and dependency order

| Spec | Title | Crate(s) | Depends on |
|---|---|---|---|
| SPEC-1 | Reflection type data for Templates & Relationships | `bevy_ecs` | — |
| SPEC-2 | Erased-API extensions to the scene core | `bevy_scene` (+ `bevy_ecs` for `SceneEntityReference`) | SPEC-1 (types only) |
| SPEC-3 | `.bsn` text format, lexer, parser, and AST | **new workspace crate `crates/bevy_bsn`** (zero bevy deps) | — |
| SPEC-4 | Reflection-driven resolution (AST → `Scene` → `ResolvedScene`) | `bevy_scene` | SPEC-1, SPEC-2, SPEC-3 |
| SPEC-5 | The `DynamicBsnLoader` asset loader & plugin wiring | `bevy_scene` | SPEC-3, SPEC-4 |
| SPEC-6 | Hot reload & end-to-end validation | `bevy_scene` | SPEC-5 |

SPEC-1 and SPEC-2 are standalone, upstreamable PRs. SPEC-3 is a **standalone workspace
crate, `crates/bevy_bsn`**, with zero dependencies on other bevy crates (ratified per
Kneelawk's request in #23576, comment 4375795637: an "official parsing library that does
not bring in the rest of Bevy's infrastructure as a dependency", enabling external tools —
Blender/Unity plugins — that *read and write* BSN). Consequences: the canonical printer
ships in the crate (write side); the crate is `no_std + alloc` with a default `std`
feature; no `World`, `Entity`, `bevy_reflect`, or `bevy_asset` types anywhere in it; the
ECS-backed AST projection (deferred, editor track) sits above it in `bevy_scene`.
`bevy_scene` consumes it as an optional dependency activated by the `bsn_asset` feature.

## 4. Grounding: key existing code (all specs must cite and conform to these)

- `Scene` trait (by-value `resolve(self, ...)`, `SceneBox` object-safety workaround):
  `crates/bevy_scene/src/scene.rs:48-122`.
- `ScenePatch` asset (`scene: Option<Box<dyn Scene>>`, `#[dependency] dependencies`,
  `resolved: Option<Arc<ResolvedSceneRoot>>`): `crates/bevy_scene/src/scene_patch.rs:20-30`;
  `ScenePatch::load_with(&mut impl LoadFromPath, scene)`: `scene_patch.rs:41`;
  one-shot `resolve` does `self.scene.take()`: `scene_patch.rs:57-67`.
- `ResolvedScene` internals, erased template APIs, cached copy-on-write:
  `crates/bevy_scene/src/resolved_scene.rs:165-567`.
- `RelatedResolvedScenes` with public `unsafe fn` pointers + `new::<R: Relationship>()`:
  `resolved_scene.rs:650-692`.
- `ErasedComponentTemplate` (apply via `BundleWriter`): `resolved_scene.rs:696-760`.
- `Template` / `FromTemplate` / blanket impls (`Clone+Unpin` ⇒ `Template`,
  `Clone+Default+Unpin` ⇒ `FromTemplate` with `Template = Self`):
  `crates/bevy_ecs/src/template.rs:32-41, 348-351, 390-406`.
- `SceneEntityReference` (macro-callsite identity): `bevy_ecs/src/template.rs:141-167`.
- `CachedSceneAsset` (the `:"x.bsn"` include): `crates/bevy_scene/src/scene.rs:411-442`;
  `ResolvedScene::include_cached` ordering rules: `resolved_scene.rs:549-567`.
- Spawn pipeline & asset-event handling (`resolve_scene_patches` handles only
  `LoadedWithDependencies`/`Removed`): `crates/bevy_scene/src/spawn.rs:607-788`.
- Type-data pattern reference: `crates/bevy_ecs/src/reflect/component.rs:1-120`
  (`ReflectComponent(ReflectComponentFns)`, built via `CreateTypeData<C>`).
- `ReflectConvert` (merged, for string→Handle): `crates/bevy_reflect/src/convert.rs`.
- `from_reflect_with_fallback` ladder: `crates/bevy_ecs/src/reflect/mod.rs:108-164`.
- Handle-from-path deserialization: `HandleDeserializeProcessor`, `LoadFromPath`
  (`load_from_path_erased`): `crates/bevy_asset/src/reflect.rs:398-500`.
- Loader shape proven by `FakeSceneLoader`: `crates/bevy_scene/src/lib.rs:1133-1151`,
  `benches/benches/bevy_scene/spawn.rs:406-428`.
- Prior art (MUST read, in scratchpad): `../reflect_template.rs` (pcwalton's
  `ReflectTemplate`/`ReflectFromTemplate`, 60 lines) and `../dynamic_bsn.rs` (pcwalton's
  full loader, 1050 lines, written against the OLD `&self` resolve API — port, don't copy).
  jbuehler23's enum fix (pcwalton/bevy#21, merged into that file): wrap
  `DynamicStruct`/`DynamicTupleStruct` in `DynamicEnum` with the correct variant name.

## 5. Interface contracts (NORMATIVE)

### Contract A — `bevy_ecs::reflect::template` (SPEC-1 provides; SPEC-4 consumes)

```rust
/// Type data registered on a COMPONENT type C that has a custom FromTemplate impl.
/// Absence means `<C as FromTemplate>::Template == C` (the Clone+Default blanket).
#[derive(Clone)]
pub struct ReflectFromTemplate { pub template_type_id: TypeId }
// registered via #[reflect(FromTemplate)] on the component; CreateTypeData<C: FromTemplate>
// where <C::Template as Template>::Output: Reflect

/// Type data registered on a TEMPLATE type T (e.g. HandleTemplate<A>) whose Output differs
/// from T. Absence means output == template (apply clones the template value).
#[derive(Clone)]
pub struct ReflectTemplate {
    pub build_template:
        fn(&dyn Reflect, &mut TemplateContext) -> Result<Box<dyn Reflect>, BevyError>,
}
// registered via #[reflect(Template)] on the template type; CreateTypeData<T: Template>
// where T::Output: Reflect
```

Additionally, **extend `ReflectComponentFns`** (registered for free by every
`#[reflect(Component)]`) with one new field — this is the erased insertion path that keeps
the single-archetype-move guarantee:

```rust
/// Pushes a built component value into a BundleWriter.
/// Tries downcast to C first (values from ReflectTemplate::build_template are concrete);
/// falls back to from_reflect_with_fallback for dynamic values.
/// SAFETY: bundle_writer and registrator must belong to the same World.
pub push_to_bundle_writer: unsafe fn(
    Box<dyn PartialReflect>,
    &TypeRegistry,
    &mut ComponentsRegistrator,
    &mut BundleWriter,
) -> Result<(), BevyError>,
```

### Contract B — `bevy_ecs::reflect::relationship` (SPEC-1 provides; SPEC-2/4 consume)

```rust
/// Type data registered on a RelationshipTarget type (e.g. `Children`), because `.bsn`
/// syntax names the target: `Children [ ... ]`.
#[derive(Clone)]
pub struct ReflectRelationshipTarget { // name per §7 ratification (registered on the target type)
    pub relationship_type_id: TypeId,        // TypeId::of::<ChildOf>()
    pub relationship_target_type_id: TypeId, // TypeId::of::<Children>()
    pub relationship_name: &'static str,     // type_name::<ChildOf>()
    /// Same bodies as RelatedResolvedScenes::new::<R>() (resolved_scene.rs:670-692).
    pub insert_relationship:
        unsafe fn(&mut BundleWriter, &mut ComponentsRegistrator, Entity),
    pub insert_relationship_target:
        unsafe fn(&mut BundleWriter, &mut ComponentsRegistrator, usize),
}
// CreateTypeData<T: RelationshipTarget>; registered on Children via reflect attribute.
```

### Contract C — `bevy_scene` erased-API extensions (SPEC-2 provides; SPEC-4 consumes)

1. `ResolvedScene::get_or_insert_erased_template` `default` parameter changes from
   `fn() -> Box<dyn ErasedComponentTemplate>` to
   `impl FnOnce() -> Box<dyn ErasedComponentTemplate>`. (Callers all pass closures already
   compatible; this is a non-breaking generalization.)
2. New provided method on `ErasedComponentTemplate`:
   `fn try_as_partial_reflect_mut(&mut self) -> Option<&mut dyn PartialReflect> { None }`
   (default `None`; the generic impl for `T: Template` returns `Some` when
   `T: PartialReflect` is not expressible — so the generic impl keeps the default, and
   SPEC-4's dynamic template overrides it. Additionally add
   `fn try_as_partial_reflect(&self) -> Option<&dyn PartialReflect> { None }`.)
3. New method:
   `ResolvedScene::get_or_insert_related_resolved_scenes_erased(&mut self, data: &ReflectRelationshipTarget) -> &mut RelatedResolvedScenes`
   — keyed by `data.relationship_type_id` in the same `related` map the generic method uses
   (dynamic and static children of the same relationship MUST land in the same entry).
4. `SceneEntityReference` gains an asset-based identity. Current identity is
   `(&'static str file, u32 line, u32 column, name_id, call_id)`. Add a constructor for
   `(source: Arc<str> /* asset path */, node_id: u32 /* stable AST node index */)` — exact
   representation decided in SPEC-2, constraints: no `&'static str` leaking, `Eq + Hash`
   consistent with existing usage, stable across re-parses of the same unchanged file,
   distinct across files.
5. New `ResolveSceneError` variants needed by SPEC-4 (names normative):
   `TypeNotRegistered { type_path: String }`, `TypeNotReflectable { type_path: String }`,
   `MissingReflectDefault { type_path: String }`, `MissingReflectComponent { type_path: String }`,
   `UnsupportedRelationship { type_path: String }`, `ApplyFailed { type_path: String, error: ... }`.
   All errors, never panics, in the dynamic path (match `DynamicWorld`'s error style,
   NOT `from_reflect_with_fallback`'s panic style).

### Contract D — parser & AST (SPEC-3 provides; SPEC-4/5 consume)

Layer 1 (bevy-independent): `Lexer` + recursive-descent or LALRPOP parser producing a plain
`BsnAst` value tree with **stable node IDs** (`BsnNodeId(u32)`, assigned in document order):

```rust
pub struct BsnDocument { pub roots: Vec<BsnNodeId>, pub nodes: Vec<BsnNode>, /* spans */ }
pub struct BsnNode { pub id: BsnNodeId, pub span: Span, pub kind: BsnNodeKind }
pub enum BsnNodeKind {
    Entity { name: Option<String>, base: Option<String> /* ":path.bsn" */,
             patches: Vec<BsnNodeId>, relations: Vec<BsnNodeId> },
    Patch { symbol: BsnPath, is_template: bool /* @-prefixed */, value: BsnValueId },
    Relation { target_symbol: BsnPath /* e.g. "Children", full path allowed */,
               entities: Vec<BsnNodeId> },
}
pub enum BsnValue { Unit, Bool(bool), Int(i128), Float(f64), String(String),
    Path(BsnPath) /* enum unit variant or const-like */, Tuple(Vec<BsnValueId>),
    Struct(Vec<(String, BsnValueId)>), List(Vec<BsnValueId>),
    NamedTuple(BsnPath, Vec<BsnValueId>), EntityRef(String /* #Name */) }
```

Grammar: syntactic subset of `bsn!` (see `crates/bevy_scene/macros/src/lib.rs:30-110`
table): fully-qualified or registry-resolvable type paths, struct patches with partial
fields, tuple patches with partial leading fields, enum unit/struct/tuple variants,
`~`/`@` template patches, `#Name`, `:"other.bsn"` first-entry inheritance,
`Children [ ... ]` (any registered RelationshipTarget path), string literals for asset
paths, numeric/bool literals, nested tuples of entities for multi-root.

Layer 2 (`bevy_scene`): ECS projection `BsnAst(World)` mirroring pcwalton
(`BsnPatch`/`BsnExpr`/`BsnPatches` components, one entity per `BsnNodeId`) is **deferred to
the editor track and OUT OF SCOPE** — SPEC-4/5 consume the plain `BsnDocument` directly.
SPEC-3 must state this and keep `BsnNodeId` stable so the projection can be added later.

### Contract E — dynamic scene types (SPEC-4 provides; SPEC-5/6 consume)

```rust
/// The runtime Scene built from a parsed BsnDocument + resolved symbols.
/// Implements Scene (BY-VALUE resolve; wrap self-consumption accordingly) and Clone-able
/// via internal Arc so ScenePatch can retain it for hot reload (SPEC-6).
pub struct DynamicScene { /* Arc<DynamicSceneInner> */ }
```

Resolution rules (normative):
- Symbol → `TypeRegistration` via `TypeRegistry::get_with_type_path`, falling back to
  parent-path lookup for enum variants (port `resolve_type_or_enum_variant_to_template`
  from `dynamic_bsn.rs:857-903`).
- Component TypeId → template TypeId via `ReflectFromTemplate`, defaulting to same TypeId.
- Template construction: `ReflectDefault` on the template type (error `MissingReflectDefault`
  otherwise). Field patch: build partial `DynamicStruct`/`DynamicTupleStruct`/`DynamicEnum`
  (enum variants wrapped per jbuehler23's fix) and `try_apply` onto the stored template via
  `try_as_partial_reflect_mut`. Value coercion order: exact type → `ReflectConvert`
  (string→Handle etc.) → numeric lossless widening → error.
- Template slot keying: the REAL template TypeId (so dynamic and `bsn!` patches merge, and
  cached copy-on-write `clone_template` interop works).
- Apply path: dynamic erased template's `apply` uses `ReflectTemplate::build_template` when
  present, else `PartialReflect::reflect_clone` of the stored template; insertion via
  Contract A's `push_to_bundle_writer` (NEVER `EntityWorldMut::insert` per-component; the
  single-bundle-write property must hold).
- Children: `get_or_insert_related_resolved_scenes_erased` with `ReflectRelationshipTarget`
  looked up from the relation symbol's registration.
- `:"path.bsn"` base: emit `CachedSceneAsset` include as the FIRST resolve step (mirrors
  macro; `include_cached` ordering rules apply). Dependencies: `register_dependencies`
  walks the document and registers every asset-path string typed as a Handle plus every
  base include, using `SceneDependencies::register_erased`.

### Contract F — loader & plugin (SPEC-5)

`DynamicBsnLoader: AssetLoader<Asset = ScenePatch>`, `extensions() == &["bsn"]`, holds
`TypeRegistryArc` via `FromWorld` (mirror `bevy_world_serialization/src/world_asset_loader.rs:19-34`),
`load()` = read text → parse (SPEC-3) → build `DynamicScene` (SPEC-4) →
`ScenePatch::load_with(load_context, scene)`. Registered by `ScenePlugin` behind a new
default-on cargo feature `bsn_asset` in `bevy_scene` (+ plumbed through `bevy_internal`).
Parse/resolve errors are returned as loader errors with file/span context (never panic).

### Contract G — hot reload (SPEC-6)

`ScenePatch` gains `pub source: Option<Box<dyn Scene>>`-style retention WITHOUT breaking
one-shot semantics: concretely, `DynamicScene` is `Clone` (Arc inner), the loader stores a
clone in a new field `ScenePatch::reload_source: Option<Box<dyn Scene>>` (name final in
SPEC-6), and `resolve_scene_patches` handles `AssetEvent::Modified` by re-resolving from
`reload_source` (or by the freshly reloaded asset — note the AssetServer re-runs the loader
on file change, producing a new ScenePatch value; SPEC-6 must reconcile these two paths and
pick the minimal correct one) and re-applying to all `ScenePatchInstance` entities
(despawn scene-spawned descendants, re-apply; document state loss; reconciliation is a
non-goal). `bsn!`-defined patches without a retained source keep current behavior.

## 6. Decision log (with rationale; do not relitigate in specs)

1. **Custom lexer/parser, not the syn-based macro parser** — cart's stated preference in
   #23576 (differences manifest; less runtime code).
2. **Plain-value AST with stable node IDs now; ECS-backed AST later** — pcwalton's
   `BsnAst(World)` exists for editor bidirectional links (proven by jackdaw), but the
   loading path doesn't need it; keeping the parser bevy-independent honors the ecosystem
   request. Stable `BsnNodeId` preserves the upgrade path and feeds the asset-based
   `SceneEntityReference` identity.
3. **Real template TypeId keying** — required for merge interop with `bsn!` and cached
   copy-on-write; single-wrapper-TypeId designs collide (all dynamic components in one
   slot) or panic typed callers (`resolved_scene.rs:420` downcast unwrap).
4. **Insertion via `push_to_bundle_writer` on `ReflectComponentFns`** — preserves
   `ResolvedScene`'s one-archetype-move apply; avoids a per-type registration burden
   (every `#[reflect(Component)]` gets it for free); dynamic patches must be component
   templates (keyed slots) not bundle templates (push-only, no merging).
5. **Only custom-template types need annotations** (`#[reflect(FromTemplate)]` on the
   component, `#[reflect(Template)]` on the template type). The `Clone+Default` blanket
   makes `Template == Component` for the common case — absence of type data encodes that.
6. **Errors, never panics** in the dynamic path — `.bsn` files are user data;
   follow `DynamicWorld`'s error-variant style.
7. **Hot reload = re-resolve + re-apply** — reconciliation deferred (cart/bevy#36).
8. **`ReflectConvert` for coercions** — merged upstream (#23742) exactly for this.

## 7. Ratified amendments (review pass — these override §5/§6 where they conflict)

After review of SPEC-1..6 against the codebase, the following are RATIFIED:

**Contract A (SPEC-1):**
- A-1: `ReflectTemplate` gains `output_type_id: TypeId` (enables locating `ReflectComponent`
  on the output type at load time and handle-dependency discovery, SPEC-4 §4.10).
- `ReflectFromTemplate` gains `template_type_path: &'static str` (error messages).
- `ReflectRelationship` is renamed **`ReflectRelationshipTarget`** (it is registered on the
  target type; Contract B updated accordingly).
- The strengthened bound `T::Template: Reflect` on `#[reflect(FromTemplate)]` stands.
- **SPEC-1 Phase 5 is a series prerequisite for SPEC-4**: the `FromTemplate` derive gains an
  opt-in `#[template(reflect)]` container attribute (generated template gets
  `#[derive(Reflect)] #[reflect(Default)]` + registration); seed set of in-repo components
  annotated; `OptionTemplate`/`VecTemplate` and `EntityTemplate` become `Reflect`
  (`SceneEntityReference` as `#[reflect(opaque)]` — covers SPEC-4 P3). Components without
  the attribute are not `.bsn`-usable and fail at load with `TypeNotRegistered`.

**Contract C (SPEC-2):**
- C-6: `ErasedComponentTemplate` gains
  `fn template_type_id(&self) -> TypeId { Any::type_id(self) }`; the duplicate-skip at
  `resolved_scene.rs:325` switches to it. (Fixes double-insertion of dynamic templates
  through the cached copy-on-write path.)
- C-7 **primary fix (supersedes SPEC-4 §4.9.2's interim-only ruling)**:
  `ResolveContext` gains `pub type_registry: Option<&'a TypeRegistry>`, populated by the
  resolve entry points (`ScenePatch::resolve` callers, `resolve_scene_patches`,
  `World::spawn_scene`) from `AppTypeRegistry` where a `World` is available.
  `get_or_insert_template::<T>`'s `downcast_mut().unwrap()` becomes: try downcast; on
  failure, attempt **typed recovery** — occupant's `try_as_partial_reflect()` +
  the registry's `ReflectFromReflect` for `TypeId::of::<T>()` → `from_reflect` →
  `Box<dyn Reflect>::downcast::<T>()` (concrete type, succeeds) → replace the slot with the
  typed box and return `&mut T`. This preserves the dynamic base's field values under a
  typed patch — correct merge semantics for `bsn! { :"player.bsn" Transform { … } }`.
  Fallback when the registry is absent or `T` lacks `ReflectFromReflect`: reset the slot to
  `T::default()` with `error!` (SPEC-4's interim), never panic. Moving
  `ErasedComponentTemplate` to `bevy_ecs` + an `erase` fn on `ReflectTemplate` is recorded
  as an upstream-alignment option, NOT required for v1.
- `ResolveSceneError::UnpatchableTemplate` is ratified as the 8th variant.
- `SceneEntityReferenceSource` as a `Copy` enum (`CallSite{..} | Asset{path_hash: u64}`) is
  ratified over `Arc<str>` (preserves `Copy` on `EntityTemplate`; collision risk documented).
- Record in C4: `ResolvedSceneRoot::apply` builds a fresh `SceneEntityReferences` per apply
  (`resolved_scene.rs:70`); asset-based identity is only sound under this invariant
  (SPEC-6 test #24 pins it). Must not be shared across applies.

**Contract D (SPEC-3):** `Struct`/`NamedTuple` values carry their `BsnPath`;
`is_template: bool` becomes `BsnPatchPrefix { FromTemplate, Template, SceneComponent }`;
spans on names/bases; `[…]` list values as a deliberate superset of `bsn!`; recursive
descent instead of LALRPOP (upstream-alignment open question). All ratified.

**Contract E (SPEC-4):** `DynamicScene::from_document(document, source, registry)` is the
build entry point (no `LoadFromPath` parameter — dependencies accumulate internally and
flow through `register_dependencies` → `ScenePatch::load_with`). `DynamicComponentTemplate`
must NOT be `Clone` (would collide with the blanket `ErasedComponentTemplate` impl; it
cannot derive `Clone` anyway — state it explicitly).

**Contract G (SPEC-6):** `reload_source` is **removed** — the AssetServer replaces the whole
`ScenePatch` value on reload, so the reloaded asset carries a fresh `scene: Some(_)`;
retention would snapshot stale content. `LoadedWithDependencies` re-fires on reload and is
the trigger; `Modified` is not used (frame-late + self-emission via `get_mut`, which
switches to `get_mut_untracked`). SPEC-6's dependent re-resolution (runtime deps scanned
from `ScenePatch::dependencies`) and despawn-first re-apply with `SceneInstanceState`
bookkeeping are ratified as specced.

**SPEC-5:** associated consts (`Color::WHITE`) are unsupported in v1 (consistent with §2) —
the example must not use them; the `AssetLoadFailedEvent<ScenePatch>` warning system (Q5)
is ratified in scope; self-include (`a.bsn` whose base is itself) is a load-time error,
general cross-file cycles deferred (Q3).

**Deferred (recorded, not in v1):** component-removal-on-reload stripping; `SceneListPatch`
hot reload; cached reverse-dependency index; multi-error parser recovery; labeled
sub-assets & multi-root `.bsn`. *(The separate parser crate is no longer deferred — it is
ratified as `crates/bevy_bsn`, see §3.)*

**Conformance-review additions (upstream-alignment risks, see SPEC-3 OQ 11-13):** an
optional `bsn <semver>;` version pragma should be discussed upstream before v1 files exist
(#14437 predicates migrations on version anchors; deferring imports leaves files with
none); descendant-patching syntax (#23413 roadmap) is not reserved in the grammar; the
`@`/`~` prefix semantics deliberately follow the `bsn!` macro and diverge from pcwalton's
draft format, which must be reconciled before/when #23576 lands.

## 8. Cross-cutting conventions for all specs

- Rust API style per Bevy: docs on every public item, `#[derive(Debug)]` where possible,
  error enums via `thiserror`, no `unwrap` outside tests, `unsafe` only with SAFETY comments.
- Every spec MUST contain: Goals; Non-goals; Background (with repo `file:line` citations);
  Detailed design (exact signatures, exact file paths to create/modify); Step-by-step
  implementation plan (ordered, each step compilable); Edge cases & error handling;
  Test plan (concrete test names + what each asserts, placed in which file); Acceptance
  criteria; Open questions.
- A junior engineer with Rust experience but no Bevy-internals background must be able to
  implement from the spec alone: never say "handle X appropriately" — say exactly how.
- Cite pcwalton's code when porting; call out every place where main's API differs from
  his branch (by-value resolve, `ErasedComponentTemplate` + `BundleWriter` vs his
  `ErasedTemplate` + `insert_reflect`, `CreateTypeData` vs `FromType`, private `related` map).
