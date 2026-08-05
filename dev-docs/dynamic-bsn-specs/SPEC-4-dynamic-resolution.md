# SPEC-4: Reflection-Driven Resolution (`BsnDocument` → `DynamicScene` → `ResolvedScene`)

**Status:** Proposed. Conforms to SPEC-0 (NORMATIVE master). Provides **Contract E**.
Consumes **Contract A/B** (SPEC-1 type data), **Contract C** (SPEC-2 erased APIs),
**Contract D** (SPEC-3 AST).

Target: `/home/tony/workspace/bevy`, `main`, `0.20.0-dev`.

---

## 1. Goals

1. Define `DynamicScene`: a `Clone`-able (`Arc` inner), `Send + Sync + 'static` value that
   implements `bevy_scene::Scene` and is produced from a parsed `BsnDocument` (Contract D)
   plus a `TypeRegistry`.
2. Define the **load-time** lowering: symbols → `TypeRegistration`s, literals/values →
   `Box<dyn PartialReflect>` patch values, relations → `ReflectRelationshipTarget`, `#Name` →
   `SceneEntityReference`, `:"base.bsn"` → `CachedSceneAsset`. All registry lookups and all
   value construction happen **once**, at load, with file/span context in every error.
3. Define `DynamicComponentTemplate`: the `ErasedComponentTemplate` implementation that
   carries a reflected template value and inserts its built output through Contract A's
   `push_to_bundle_writer` (preserving `ResolvedScene`'s single-archetype-move apply).
4. Define `Scene::resolve` for `DynamicScene` such that its merge semantics are
   **observationally identical** to the equivalent `bsn!` macro expansion
   (`crates/bevy_scene/macros/src/bsn/codegen.rs`), including partial struct patches,
   partial *leading* tuple-struct fields, nested partial patches, enum "match-or-reset",
   copy-on-write over a cached base, and implicit `.into()` conversions.
5. Define `Scene::register_dependencies` so that `:"base.bsn"` includes and every
   `Handle`-typed string literal become tracked `ScenePatch::dependencies`.
6. **No panics.** Every failure is a `DynamicSceneBuildError` (load time) or a
   `ResolveSceneError` (resolve time), each carrying a type path and, at load time, a span.

## 2. Non-goals

- Parsing/lexing (SPEC-3), the `AssetLoader` and plugin wiring (SPEC-5), hot reload (SPEC-6).
- Invoking `SceneComponent::scene(props)` from `.bsn` (SPEC-0 §2). A `@Type` entry parses
  (SPEC-3 grammar) but **fails at build time** with `SceneComponentUnsupported`.
- Observers (`on(...)`), closures, `{ expr }` blocks, function calls, Rust consts.
- Writing `.bsn` back out; editor `SceneDocument` APIs; the ECS-backed AST projection.
- State-preserving reconciliation.
- `&'static str` fields (see §6.7).

## 3. Background

### 3.1 What `main` actually does (all citations are `file:line` in this repo)

- **`Scene` is by-value.** `fn resolve(self, &mut ResolveContext, &mut ResolvedScene)`
  — `crates/bevy_scene/src/scene.rs:54-58`. Object safety is provided by `SceneBox`
  (`scene.rs:82-108`) and `impl<T: ?Sized + SceneBox> Scene for Box<T>` (`scene.rs:110-122`).
- **`ResolveContext`** carries only `assets`, `patches`, `cached` — **no `TypeRegistry`**
  (`scene.rs:170-177`). A `Scene` that needs the registry must carry it itself.
- **`TemplatePatch::resolve`** is the canonical "patch a template" scene:
  `let template = scene.get_or_insert_template::<T>(context); (self.0)(template, context);`
  — `scene.rs:333-347`. This is what `bsn!` field patches lower to
  (`macros/src/bsn/codegen.rs:215-247`).
- **`CachedSceneAsset::resolve`** looks up the handle, calls `scene.include_cached(handle)`
  and sets `context.cached = Some(scene_patch)` — `scene.rs:422-442`.
  `include_cached` errors with `CachedSceneError::LateCached` if the `ResolvedScene`
  already has templates or related scenes — `resolved_scene.rs:549-567`. Therefore a base
  include **must be the first thing resolved on an entity**.
- **`get_or_insert_erased_template`** — `resolved_scene.rs:461-498`. Copy-on-write: if the
  slot is vacant *and* `context.cached`'s resolved scene has a template with that `TypeId`,
  it `clone_template()`s it into the local scene and records the `TypeId` in
  `CachedSceneInfo::duplicate_templates`. The `default` parameter is `fn() -> Box<dyn
  ErasedComponentTemplate>` — a bare fn pointer (Contract C item 1 widens it to
  `impl FnOnce`, which SPEC-4 *requires*: our default closure captures cloned type data).
- **`duplicate_templates` skip** — `resolved_scene.rs:254-261` passes it to
  `apply_templates_without_bundle_write`, which skips via
  `skip_templates.should_skip((**template).type_id())` — `resolved_scene.rs:325`.
  `(**template).type_id()` is `Any::type_id` of the **concrete boxed template type**.
  For the generic impl (`resolved_scene.rs:714`) that equals the template `TypeId`.
  **It does not for a single dynamic wrapper type.** See §4.2, addendum C-6.
- **`ErasedComponentTemplate`** — `resolved_scene.rs:696-732`. `apply` receives
  `&mut TemplateContext` + `&mut BundleWriter` and pushes the built component with
  `bundle_writer.push_component(&mut components, component)`. The whole entity is written
  in **one** `bundle_writer.write` (`resolved_scene.rs:282, 306`) — one archetype move.
- **`RelatedResolvedScenes`** — `resolved_scene.rs:650-692`; the `related` map on
  `ResolvedScene` is **private** (`resolved_scene.rs:174`); only the generic
  `get_or_insert_related_resolved_scenes::<R>()` exists (`resolved_scene.rs:537-543`).
  Contract C item 3 adds the erased variant.
- **`get_or_insert_template::<T>`** downcasts the erased slot and `unwrap()`s
  — `resolved_scene.rs:409-422`. This is a **panic** if the slot holds a foreign box. See §4.9.
- **`ScenePatch::load_with(&mut impl LoadFromPath, scene)`** runs
  `Scene::register_dependencies` and turns each `(type_id, path)` into an `UntypedHandle`
  — `scene_patch.rs:41-53`. `ScenePatch::resolve` `take()`s the scene (one-shot)
  — `scene_patch.rs:57-67`.
- **`resolve_scene_patches`** resolves only on `AssetEvent::LoadedWithDependencies`
  — `spawn.rs:616-627`.
- **Reflect apply semantics**: struct `try_apply` iterates the *incoming* value's fields
  and silently ignores fields the target does not have (`structs.rs:499-509`); tuple structs
  behave the same (`tuple_struct.rs:108-120`). We therefore validate field names/arities
  **ourselves at load time** instead of relying on `try_apply` (which would silently drop
  typos). Enum `try_apply` (generated, `bevy_reflect/derive/src/impls/enums.rs:192-243`)
  has two branches: same variant name ⇒ update only the incoming fields; different variant
  ⇒ **construct the whole variant**, which requires *every* non-ignored field of the target
  variant to be present, else `ApplyError::MissingEnumField`
  (`bevy_reflect/derive/src/enum_utility.rs:245-270`). Ignored fields and
  `#[reflect(default)]` fields are auto-defaulted (`enum_utility.rs:82-131`).
- **`ReflectConvert`** — `bevy_reflect/src/convert.rs`. Type data on the *destination*
  type; `try_convert_from(Box<dyn Reflect>) -> Result<Box<dyn Reflect>, Box<dyn Reflect>>`,
  keyed by the **source concrete `TypeId`**. `String → HandleTemplate<A>` is already
  registered for every reflected asset: `crates/bevy_asset/src/lib.rs:703-704`.
  This replaces pcwalton's `starts_with("bevy_asset::handle::HandleTemplate<")` hack
  (`dynamic_bsn.rs:627-642`).
- **`ReflectFromPtr`** is inserted automatically by every `#[derive(Reflect)]`
  (`bevy_reflect/derive/src/registration.rs:68-70`); `as_reflect_mut(PtrMut) -> &mut dyn Reflect`
  (`type_registry.rs:979-982`). `PtrMut: From<&mut T>` for `T: ?Sized`
  (`bevy_ptr/src/lib.rs:965-972`). Used in §4.9.
- **`from_reflect_with_fallback`** (`bevy_ecs/src/reflect/mod.rs:108-164`) **panics** when
  no strategy applies. SPEC-4 never calls it directly; Contract A's
  `push_to_bundle_writer` is specified to downcast first (values we produce are concrete),
  so the panic path is not reachable for well-formed dynamic scenes. See §6.10.
- **`VariantDefaults`** (`bevy_ecs/macros/src/variant_defaults.rs`) and the `FromTemplate`
  derive (`bevy_ecs/macros/src/template.rs:186-262`) generate inherent
  `fn default_<variant>() -> Self` whose body is `Self::Variant { field: Default::default(), .. }`.
  There is **no type data** for these, so the dynamic path reconstructs them field-by-field
  from `ReflectDefault` on each field type — semantically identical (§4.6.4).
- **`bsn!` value semantics** (`macros/src/bsn/codegen.rs`):
  - every value gets an implicit `.into()` (`codegen.rs:726-751`);
  - a nested `BsnValue::Type` with **no** enum variant becomes a *nested path assignment*,
    i.e. a recursive **partial** patch (`codegen.rs:586-600`) — this is why
    `Foo { nested: Bar(2) }` over `Bar(1,1,0)` yields `Bar(2,1,0)`
    (test `struct_patching`, `lib.rs:1803-1850`);
  - an enum-variant patch emits *match-or-reset*:
    `if !matches!(_node, T::V{..}) { *_node = T::default_v(); } if let T::V{a,..} = _node { *a = ...; }`
    (`codegen.rs:440-450`);
  - tuple fields are assigned by index `0..n`, so trailing fields keep their previous
    value (test `partial_tuple_struct`, `lib.rs:1678-1697`).

### 3.2 Catalogue of deltas vs. pcwalton's `dynamic_bsn.rs` (prior art)

Every item below is a place where the port **must** differ from the scratchpad file.

| # | pcwalton (`dynamic_bsn.rs`) | current `main` / this spec |
|---|---|---|
| D1 | `Scene::resolve(&self, …)` (`:795, :821, :921`) | by-value `resolve(self, …)`; `DynamicScene` holds an `Arc` so `self` consumption is free (§4.3) |
| D2 | `ErasedTemplate` + `context.entity.insert_reflect(...)` (`:985`) — one archetype move **per component** | `ErasedComponentTemplate::apply(&self, ctx, &mut BundleWriter)` + Contract A `push_to_bundle_writer` — a **single** archetype move for the whole entity (§4.7) |
| D3 | `ErasedTemplate::as_any_mut` + `try_as_partial_reflect_mut` already on the trait (`:995-1001`) | `main` has neither; Contract C item 2 adds `try_as_partial_reflect{,_mut}` with `None` defaults; §4.9 adds the `ReflectFromPtr` fallback for typed slots |
| D4 | `scene.related.entry(...)` — direct field access (`:829`) | `related` is private; use Contract C item 3 `get_or_insert_related_resolved_scenes_erased(&ReflectRelationshipTarget)` (§4.8) |
| D5 | Hard-codes `ChildOf`/`Children`; errors otherwise (`:426, :825`) | any registered `RelationshipTarget` via Contract B `ReflectRelationshipTarget` (§4.8) |
| D6 | `get_or_insert_erased_template(…, move \|\| …)` — needs a closure (`:942`) | `main` takes `fn()`; **requires** Contract C item 1 |
| D7 | `starts_with("bevy_asset::handle::HandleTemplate<")` string sniff (`:629-642`) + `panic!` on `:637` | `ReflectConvert` (`convert.rs`), registered at `bevy_asset/src/lib.rs:704`; no panics (§4.6.2) |
| D8 | `as` casts for int literals — silently wraps (`:695-738`) | range-checked `TryFrom` with `IntegerOutOfRange` (§4.6.1) |
| D9 | `unwrap()` on registry lookups (`:284, :358, :470, :617, :774`) | all fallible, all produce errors with span + type path (§5) |
| D10 | `error!()` + silent `return` inside patch closures (`:328, :398, :416`) | typed errors returned from `resolve` (§6) |
| D11 | Symbol resolution at load, patch value construction at load, but the **apply** re-derives everything from the registry each time (`:962-972`) | all type data (`ReflectDefault`, `ReflectComponent`, `ReflectTemplate`, `ReflectFromReflect`) is **cloned into** the patch at load time; resolve/apply do zero registry *lookups* (§4.3) |
| D12 | Enum handling: `DynamicEnum` wrapping with the correct variant name (`:336-346`, `:406-419`) — **jbuehler23's fix, PRESERVED** | preserved *and* extended with match-or-reset + defaulted variant fill (§4.6.4), which pcwalton's `apply` does not implement (his `enum_reflect.apply(&dynamic_enum)` panics on a variant switch with missing fields) |
| D13 | Throws the AST away; `MultiPatch` wrapper for multi-root (`:215`) | single-root `DynamicScene`; multi-root is SPEC-5's `SceneListPatch` decision |
| D14 | `context.cached` leaks into related entity scenes | explicitly saved/cleared/restored around child resolution (§4.8) |

---

## 4. Detailed design

### 4.1 Files

| Path | Action |
|---|---|
| `crates/bevy_scene/src/dynamic/mod.rs` | **new** — `pub use` of the items below; module doc |
| `crates/bevy_scene/src/dynamic/scene.rs` | **new** — `DynamicScene`, `DynamicSceneInner`, `DynamicSceneEntity`, `DynamicRelation`, `Scene` impl |
| `crates/bevy_scene/src/dynamic/build.rs` | **new** — `DynamicSceneBuilder`, symbol resolution, `DynamicSceneBuildError` |
| `crates/bevy_scene/src/dynamic/value.rs` | **new** — the `BsnValue × TypeInfo → Box<dyn PartialReflect>` algorithm |
| `crates/bevy_scene/src/dynamic/template.rs` | **new** — `DynamicComponentTemplate`, `erased_template_reflect_mut` |
| `crates/bevy_scene/src/lib.rs` | modify — `#[cfg(feature = "bsn_asset")] mod dynamic; pub use dynamic::*;` |
| `crates/bevy_scene/Cargo.toml` | modify — feature `bsn_asset` (default-on) pulling `bevy_reflect`, `bevy_ptr` |

All new modules are behind the `bsn_asset` feature introduced by SPEC-5 (Contract F).

### 4.2 Prerequisites and required contract addenda

These are **preconditions for SPEC-4 to compile/work**. Items marked *(addendum)* are
changes to SPEC-1/SPEC-2 that SPEC-4 needs and that SPEC-0 §5 does not yet spell out; they
are re-raised in §10.

- **P1 — Template types must be `Reflect` + registered. RATIFIED into SPEC-1 Phase 5**
  (SPEC-0 §7, Contract A). `bevy_ecs/macros/src/template.rs` currently emits the generated
  `…Template` struct/enum **without** `#[derive(Reflect)]` (see `template.rs:100-121,
  265-280`). SPEC-1 Phase 5 adds an **opt-in `#[template(reflect)]` container attribute**:
  when present, the generated template type gets `#[derive(Reflect)] #[reflect(Default)]`
  plus registration. A seed set of in-repo components is annotated. Components *without*
  the attribute are not `.bsn`-usable and fail at load with `TypeNotRegistered` — SPEC-5's
  user documentation must say so, and §6.1's `TypeNotRegistered` message should name the
  attribute.
- **P2 — Components used from `.bsn` must be registered with `#[reflect(Component, Default)]`**
  (`ReflectComponent` for insertion, `ReflectDefault` for template construction). This is a
  documented user requirement, not an engine change.
- **P3 — `EntityTemplate` must be `Reflect`. RATIFIED into SPEC-1 Phase 5** (SPEC-0 §7):
  `EntityTemplate`, `OptionTemplate` and `VecTemplate` become `Reflect`, with
  `SceneEntityReference` as `#[reflect(opaque)]`. This covers both `#Name` values (§4.6.8)
  and the `#[template(built_in)]` fields that §6.8 previously listed as unsupported.
  Identity is carried by the ratified `SceneEntityReferenceSource` `Copy` enum
  (`CallSite { .. } | Asset { path_hash: u64 }`, Contract C), which preserves `Copy` on
  `EntityTemplate`.
- **A-1 *(addendum to Contract A)* — RATIFIED (SPEC-0 §7). `ReflectTemplate` gains
  `output_type_id: TypeId`.**
  ```rust
  pub struct ReflectTemplate {
      pub build_template: fn(&dyn Reflect, &mut TemplateContext) -> Result<Box<dyn Reflect>, BevyError>,
      /// `TypeId::of::<<T as Template>::Output>()`.
      pub output_type_id: TypeId,
  }
  ```
  Needed for (a) locating `ReflectComponent` on the *output* type at load time and
  (b) discovering that `HandleTemplate<A>` produces `Handle<A>` and thence `A`'s `TypeId`
  for dependency registration (§4.10). Cost: one field.
  SPEC-1 additionally ratified `ReflectFromTemplate::template_type_path: &'static str`
  (better error messages) and the strengthened `T::Template: Reflect` bound on
  `#[reflect(FromTemplate)]`; §4.4.2 uses the former in its error paths.
- **C-6 *(addendum to Contract C)* — RATIFIED (SPEC-0 §7).
  `ErasedComponentTemplate::template_type_id`.**
  ```rust
  /// The `TypeId` of the `Template` this erased template represents. This is the key used by
  /// `ResolvedScene::template_indices`; it is *not* necessarily the `TypeId` of `Self`.
  fn template_type_id(&self) -> TypeId { Any::type_id(self) }
  ```
  and `resolved_scene.rs:325` changes from `(**template).type_id()` to
  `template.template_type_id()`. **Required for correctness**: without it the
  `duplicate_templates` copy-on-write skip (`resolved_scene.rs:254-261`) never matches a
  dynamic template, and a component patched by both a cached dynamic base *and* the local
  scene is pushed to the `BundleWriter` **twice** with the same `ComponentId`.
- **C-7 *(Contract C, RATIFIED — SPEC-2 owns it)* — typed recovery in
  `get_or_insert_template::<T>`.** `resolved_scene.rs:416-421` `unwrap()`s a downcast that
  fails when the slot was created by a dynamic scene (a `bsn!` scene inheriting a `.bsn`
  base and patching the same component). SPEC-2 replaces it with:
  1. `ResolveContext` gains `pub type_registry: Option<&'a TypeRegistry>`, populated by the
     resolve entry points (`ScenePatch::resolve` callers, `resolve_scene_patches`,
     `World::spawn_scene`) from `AppTypeRegistry` wherever a `World` is in reach.
  2. Try the downcast. On failure, attempt **typed recovery**: take the occupant's
     `try_as_partial_reflect()` (Contract C item 2 — `DynamicComponentTemplate` returns
     `Some`, §4.7.4), look up `ReflectFromReflect` for `TypeId::of::<T>()` in
     `context.type_registry`, call `from_reflect`, `Box::<dyn Reflect>::downcast::<T>()`
     (concrete by construction, so it succeeds), replace the slot with the typed box and
     return `&mut T`.
  3. Fallback when the registry is absent or `T` lacks `ReflectFromReflect`: reset the slot
     to `T::default()` with an `error!`. Never panic.

  Step 2 **preserves the dynamic base's field values underneath the typed patch**, which is
  what makes `bsn! { :"player.bsn" Transform { … } }` merge correctly. It works whenever the
  template type is reflect-registered, which SPEC-1 Phase 5 guarantees for the seed set (P1).
  Nothing on SPEC-4's side is conditional on it beyond `try_as_partial_reflect` returning
  `Some` — see §4.9.2.
- **`ResolveSceneError::UnpatchableTemplate { type_path }`** is Contract C's ratified 8th
  variant; SPEC-4 uses it for §4.9.1's unrecoverable branch.

### 4.3 Data model (Contract E)

```rust
// crates/bevy_scene/src/dynamic/scene.rs

/// A [`Scene`] built from a parsed `.bsn` document. Cheap to clone (`Arc` inner), which lets
/// `ScenePatch` retain a copy for hot reload (SPEC-6).
#[derive(Clone)]
pub struct DynamicScene(Arc<DynamicSceneInner>);

struct DynamicSceneInner {
    /// The root entity of the document.
    root: DynamicSceneEntity,
    /// Every asset dependency discovered while building, flattened over the whole document.
    /// (asset `TypeId`, path). Consumed by `register_dependencies`.
    dependencies: Vec<(TypeId, AssetPath<'static>)>,
    /// The asset path this document was parsed from. Used for error messages and for
    /// `SceneEntityReference` identity (Contract C item 4).
    source: Arc<str>,
}

/// One entity's worth of pre-resolved instructions, in document order.
struct DynamicSceneEntity {
    /// `:"base.bsn"`. Resolved FIRST (see `include_cached` ordering, resolved_scene.rs:549).
    base: Option<AssetPath<'static>>,
    /// `#Name`.
    name: Option<DynamicName>,
    /// Component/template patches, in document order.
    patches: Vec<DynamicPatch>,
    /// Relations (`Children [...]`, `MyRel [...]`), in document order.
    relations: Vec<DynamicRelation>,
}

struct DynamicName {
    name: Name,
    reference: SceneEntityReference,
}

struct DynamicRelation {
    /// Cloned from the relationship-target type's registration at build time (Contract B).
    /// Named `ReflectRelationshipTarget` per SPEC-0 §7's ratified rename — it is registered
    /// on the *target* type (`Children`), which is what `.bsn` syntax names.
    data: ReflectRelationshipTarget,
    /// The related entity scenes, in document order.
    children: Vec<DynamicSceneEntity>,
}
```

```rust
/// A single `Type { … }` / `Type(…)` / `Type::Variant { … }` / `~Type { … }` entry.
struct DynamicPatch {
    /// The REAL template `TypeId` — the slot key in `ResolvedScene::template_indices`.
    /// This is what makes dynamic and `bsn!` patches merge (SPEC-0 decision #3).
    template_type_id: TypeId,
    /// `TypePath` of the template type. Errors only.
    template_type_path: Arc<str>,
    /// `TypePath` of the component (`Template::Output`) type. Errors only.
    component_type_path: Arc<str>,
    /// Byte span of this entry in the source document. Errors only.
    span: Span,

    // --- type data, cloned out of the registry at BUILD time (see §4.4 rationale) ---
    /// Constructs a fresh template value. From the template type's registration.
    reflect_default: ReflectDefault,
    /// Inserts the built output into a `BundleWriter`. From the OUTPUT type's registration.
    reflect_component: ReflectComponent,
    /// Present iff `Template::Output != Template` (Contract A).
    reflect_template: Option<ReflectTemplate>,
    /// Used by the `clone_template` ladder (§4.7.3). From the template type's registration.
    reflect_from_reflect: Option<ReflectFromReflect>,

    /// What to do to the template value at resolve time.
    value: DynamicPatchValue,
}

enum DynamicPatchValue {
    /// `Foo` / `~FooTemplate` with no fields: ensure the slot exists, change nothing.
    /// (Mirrors `codegen.rs:215-219`: `let _ = _scene.get_or_insert_template::<Foo>(_context);`)
    Ensure,
    /// `Foo { a: 1 }` / `Foo(1)`: `try_apply` this partial `DynamicStruct`/`DynamicTupleStruct`
    /// onto the current template value.
    Partial(Box<dyn PartialReflect>),
    /// `Foo::Bar { x: 1 }` / `Foo::Qux`: match-or-reset (mirrors `codegen.rs:440-450`).
    EnumVariant {
        /// The target variant name.
        variant: Arc<str>,
        /// `DynamicEnum(variant, <every non-ignored field defaulted, then the supplied
        /// fields overlaid>)`. Applied when the current variant differs — this is the
        /// reflection equivalent of `T::default_<variant>()` followed by the assignments.
        full: Box<dyn PartialReflect>,
        /// `DynamicEnum(variant, <only the supplied fields>)`. Applied when the current
        /// variant already matches, so untouched fields survive.
        partial: Box<dyn PartialReflect>,
    },
}
```

**Why `DynamicPatchValue::EnumVariant` stores two values.** The derived enum `try_apply`
(`impls/enums.rs:192-243`) *only* supports partial updates when the variant name matches;
switching variants demands a complete field set. Storing both forms at build time makes
`resolve` a pure `variant_name()` comparison plus one `try_apply`, with no registry access
and no allocation.

**Why all type data is cloned into `DynamicPatch` at build time.** `ReflectDefault`,
`ReflectComponent`, `ReflectTemplate`, `ReflectFromReflect` and `ReflectRelationshipTarget` are
all `Clone` and are (or contain only) function pointers
(`std_traits.rs:11-14`, `reflect/component.rs:80-81`). Cloning them at build time means:
(a) `resolve` and `apply` perform **zero** `TypeRegistry` lookups, so no
`TypeNotRegistered` can appear after a successful load; (b) behaviour is pinned to the
registry state at load, which is deterministic; (c) `apply` does not need to hold a read
guard across `build_template`, avoiding the re-entrant `RwLock` read hazard.
A live `AppTypeRegistry` handle is still stored on `DynamicComponentTemplate` because
Contract A's `push_to_bundle_writer` takes `&TypeRegistry`, and §4.9's `ReflectFromPtr`
fallback needs it.

**`Send + Sync`.** `Box<dyn PartialReflect>` and `Box<dyn Reflect>` are `Send + Sync`
(`PartialReflect: DynamicTypePath + Send + Sync`, `reflect.rs:106`), `ReflectX` type data are
`Send + Sync`, `AssetPath<'static>` and `Arc<str>` are. So `DynamicScene: Send + Sync +
'static`, satisfying `SceneBox` (`scene.rs:82`).

### 4.4 Build entry point and symbol resolution (load time)

```rust
// crates/bevy_scene/src/dynamic/build.rs

impl DynamicScene {
    /// Lowers a parsed document into a resolvable `Scene`.
    ///
    /// `source` is the asset path of the document; it is used for error messages and for
    /// `SceneEntityReference` identity. `registry` is read once, up front.
    pub fn from_document(
        document: &BsnDocument,
        source: impl Into<Arc<str>>,
        registry: &AppTypeRegistry,
    ) -> Result<Self, DynamicSceneBuildError>;
}
```

The builder holds `&TypeRegistry` (one read guard for the whole build), the `source`, an
accumulating `Vec<(TypeId, AssetPath<'static>)>` for dependencies, and a
`HashMap<&str, u32>` mapping `#Name` strings to stable node ids for entity references.

**Rationale for load-time symbol resolution.** (a) Errors are reported with file + span by
the asset loader instead of surfacing later as a generic `error!` from
`resolve_scene_patches` (`spawn.rs:624`); (b) `resolve` runs once per spawn-site and
possibly many times, so lookups should not be repeated; (c) it makes `resolve`'s failure
set small and auditable (§6). It does **not** make `resolve` infallible: `include_cached`,
`try_apply` and the typed-slot interop path (§4.9) can still fail, and the registry can
change between load and resolve — which is precisely why the type data is *cloned* rather
than looked up lazily (§4.3).

#### 4.4.1 `resolve_symbol` — port of `dynamic_bsn.rs:857-903`

```rust
struct ResolvedSymbol<'a> {
    /// Registration of the *named* type (the enum, if this is a variant).
    registration: &'a TypeRegistration,
    /// `Some(variant_name)` if the last path segment named an enum variant.
    variant: Option<&'a str>,
}

fn resolve_symbol<'a>(
    registry: &'a TypeRegistry,
    path: &BsnPath,
    span: Span,
) -> Result<ResolvedSymbol<'a>, DynamicSceneBuildError>;
```

Algorithm (`BsnPath::as_path()` joins segments with `::`):
1. `registry.get_with_type_path(&path.as_path())` → `Some(r)` ⇒ `ResolvedSymbol { r, None }`.
2. Otherwise, if the path has ≥ 2 segments, look up the parent path
   (`path.as_path_skip_last()`), i.e. `a::b::MyEnum` for `a::b::MyEnum::Variant`.
   If found **and** its `TypeInfo` is `TypeInfo::Enum(e)` **and**
   `e.variant(last_segment).is_some()`, return `ResolvedSymbol { r, Some(last_segment) }`.
   (pcwalton omitted the `is_enum` / `variant exists` checks — `dynamic_bsn.rs:875-880`
   — which turns a typo into a confusing downstream failure.)
3. Otherwise `Err(UnknownType { type_path, span })`.

Short (non-fully-qualified) names are supported exactly as far as
`TypeRegistry::get_with_type_path` supports them; SPEC-3's grammar allows both
(`lib.rs:991` `supports_fully_qualified_component_paths` is the macro-side analogue).

#### 4.4.2 Component `TypeId` → template `TypeId`

Port of `dynamic_bsn.rs:1010-1034`, restated:

```rust
fn template_registration<'a>(
    registry: &'a TypeRegistry,
    named: &'a TypeRegistration,
    // Contract D's ratified `BsnPatchPrefix { FromTemplate, Template, SceneComponent }`
    // (SPEC-0 §7) replaces the earlier `is_template: bool`.
    prefix: BsnPatchPrefix,
) -> Result<(&'a TypeRegistration /*template*/, &'a TypeRegistration /*output*/), _>
```
- `BsnPatchPrefix::Template` (`~Type`) ⇒ the named type *is* the template. Its output is
  `named.data::<ReflectTemplate>().map(|t| t.output_type_id)` (addendum A-1), defaulting to
  `named.type_id()`.
- `BsnPatchPrefix::FromTemplate` (bare `Type`) ⇒ template id is
  `named.data::<ReflectFromTemplate>().map(|d| d.template_type_id)` (Contract A), defaulting
  to `named.type_id()`; output is `named` itself. `ReflectFromTemplate::template_type_path`
  (ratified) supplies the type path used in the errors below without a second registry hop.
- `BsnPatchPrefix::SceneComponent` (`@Type`) ⇒ `SceneComponentUnsupported` (§2, §6.1).
- Both ids must resolve in the registry (`TypeNotRegistered` — its message names the
  `#[template(reflect)]` attribute from P1, since that is the usual cause), the template
  registration must have `ReflectDefault` (`MissingReflectDefault`), and the output
  registration must have `ReflectComponent` (`MissingReflectComponent`).

Note for enum variants: the *named* type is the enum (`Foo`), so the template type is
`FooTemplate` and the variant name is carried separately. This is exactly pcwalton's
`template_is_enum` flag (`dynamic_bsn.rs:1005-1008`), reified as `Option<&str>`.

### 4.5 Value construction: signature and dispatch

```rust
// crates/bevy_scene/src/dynamic/value.rs

/// Builds a reflected value for `value` such that it can be applied to (or stored in) a
/// field of type `expected`.
///
/// `expected` is the *template-side* type of the destination field (e.g. `HandleTemplate<Image>`,
/// not `Handle<Image>`), because we are always building values for a template.
fn build_value(
    cx: &mut BuildCx,           // registry, source, span stack, dependency sink
    value: &BsnValue,
    expected: &TypeRegistration,
) -> Result<Box<dyn PartialReflect>, DynamicSceneBuildError>;
```

`build_value` is the only entry point; it dispatches on `BsnValue` (Contract D) and is
recursive. Every arm ends with the shared **coercion tail**:

```
COERCE(produced: Box<dyn Reflect>, produced_type_id, expected) →
  1. if produced_type_id == expected.type_id()               → Ok(produced)
  2. if let Some(c) = expected.data::<ReflectConvert>()
         && let Ok(v) = c.try_convert_from(produced)         → Ok(v)
  3. if expected is an "optionish" enum (§4.6.6)
         → recurse: wrap `produced` into the `Some` variant  → Ok(dynamic_enum)
  4. Err(ValueTypeMismatch { found, expected, span })
```

Step 2 is what makes `"image.png"` become `HandleTemplate::Path` and `TextSize::Large`
become `FontSize(24)` (macro parity: `lib.rs:2740-2773`,
`macros/.../codegen.rs:744` `(#ty).into()`).

### 4.6 Value construction: the cases

#### 4.6.1 `BsnValue::Int(i128)` / `BsnValue::Float(f64)` / `BsnValue::Bool(bool)`

Integer literal, in order:

1. **Exact:** if `expected.type_id()` is `i128` (resp. the literal's own natural type) → done.
2. **`ReflectConvert`:** boxed as its *natural* type — `i64` if the value fits in `i64`,
   else `i128` — try `try_convert_from`.
3. **Range-checked numeric coercion**, by `expected.type_id()`:

   | expected | rule |
   |---|---|
   | `i8 i16 i32 i64 i128 isize` | `TryFrom<i128>`; on failure `IntegerOutOfRange` |
   | `u8 u16 u32 u64 u128 usize` | `TryFrom<i128>`; negative or too large ⇒ `IntegerOutOfRange` |
   | `f32` | allowed iff `v.unsigned_abs() <= 1 << 24` (exactly representable), else `LiteralNotRepresentable` |
   | `f64` | allowed iff `v.unsigned_abs() <= 1 << 53`, else `LiteralNotRepresentable` |
   | `bool`, `char`, anything else | fall through to step 4 |

4. Optionish wrap (§4.6.6), else `ValueTypeMismatch`.

**Float-from-int policy (decision).** *Allowed, with an exact-representability check.*
`bsn!` rejects it (`self.x = 1;` where `x: f32` is a Rust type error, which is why
`primitive_literals` (`lib.rs:1561-1568`) writes `-1.0, 0.0, 1.0, 1.`), but `.bsn` is a
data format edited by hand and by tools, and `x: 1` vs `x: 1.0` is a pure-punctuation
footgun. The exactness check means no silent precision loss ever occurs. This is the one
deliberate, documented divergence from macro strictness; it is strictly *more* permissive,
so no `bsn!` program changes meaning.
Never coerce the other direction: `Float` → integer is always `ValueTypeMismatch`.

Float literal: exact `f64` → `ReflectConvert` from `f64` → `f32` (lossy rounding is
allowed, matching Rust literal typing; reject only if a finite input rounds to non-finite)
→ optionish → error.

Bool literal: exact `bool` → `ReflectConvert` from `bool` → optionish → error.

#### 4.6.2 `BsnValue::String(String)`

1. `expected == String` → `Box::new(s.clone())`.
2. `expected == Cow<'static, str>` → `Cow::Owned(s.clone())`.
3. `expected == AssetPath<'static>` → `AssetPath::parse(s).into_owned()`.
   (Covers a field literally typed `AssetPath`, and is reused by 4.)
4. `expected.data::<ReflectConvert>()` with `Box::new(s.clone()) as Box<dyn Reflect>`.
   For any `Handle<A>` field this hits the registration at `bevy_asset/src/lib.rs:704` and
   yields `HandleTemplate::<A>::Path(AssetPath)`. **This replaces the type-path `starts_with`
   sniff at `dynamic_bsn.rs:627-642` and its `panic!`.** On success, run the dependency
   probe of §4.10.
5. Optionish wrap (§4.6.6).
6. `Err(ValueTypeMismatch)`. In particular `&'static str` fields fail here — see §6.7.

#### 4.6.3 `BsnValue::Path(BsnPath)` — unit struct or unit enum variant

Resolve the symbol (§4.4.1).
- **Unit enum variant:** produce a value of the *named enum* type:
  `let mut v = reg.data::<ReflectDefault>()?.default();`
  `v.try_apply(&DynamicEnum::new(variant, DynamicVariant::Unit))?`.
  (jbuehler23's `DynamicEnum` wrapping, `dynamic_bsn.rs:492-493`, preserved.)
  Note `try_apply` here can only fail with `UnknownVariant`, which §4.4.1 already excluded.
- **Unit struct:** `reg.data::<ReflectDefault>()?.default()`.

Then COERCE. Coercion step 2 is what makes `TextFont { font_size: TextSize::Large }` work
(`ReflectConvert` registered from `TextSize` to `FontSize`) — the exact scenario of
`enum_variant_field_values_use_implicit_into` (`lib.rs:2740-2773`).

#### 4.6.4 `BsnValue::Struct` / `BsnValue::NamedTuple` — named composite values

Let `sym = resolve_symbol(path)`.

**Case A — same type as expected, non-enum ⇒ NESTED PARTIAL.**
If `sym.variant.is_none()` and `sym.registration.type_id() == expected.type_id()`, return a
**partial** `DynamicStruct`/`DynamicTupleStruct` (built exactly as in §4.6.5), *not* a fully
constructed value. This is the reflection equivalent of the macro's nested path assignment
(`codegen.rs:586-600`) and is what makes `Foo { nested: Bar(2) }` over `Bar(1,1,0)` produce
`Bar(2,1,0)` (`struct_patching`, `lib.rs:1803-1850`). The stock RON/serde deserializer
cannot express this; our algorithm can, because the partial value is only ever fed to
`try_apply`, which visits *incoming* fields only (`structs.rs:499-509`).

**Case B — enum variant.** Build a `DynamicEnum` with the correct variant name:
- Look up `VariantInfo` for `sym.variant` on the enum's `TypeInfo`.
- **Fill every non-ignored field of that variant** with `ReflectDefault::default()` of the
  field's own type (`NamedField::ty().id()` / `UnnamedField::ty().id()` → registration →
  `ReflectDefault`); a field type without `ReflectDefault` is
  `MissingReflectDefault { type_path, span }`.
  This reproduces `T::default_<variant>()` (`variant_defaults.rs:24-53`) exactly: that
  generated function is `Self::V { f: Default::default(), … }`. Fields marked
  `#[reflect(ignore)]` do not appear in `VariantInfo` at all and are auto-defaulted by the
  generated `try_apply` (`enum_utility.rs:126-131`), so omitting them is correct.
- Overlay the supplied fields (recursively `build_value`'d against each field's declared type).
- Wrap: `DynamicEnum::new(variant, DynamicVariant::Struct(ds) | ::Tuple(dt) | ::Unit)`.

  For tuple variants build a `DynamicTuple` from the supplied leading fields *after*
  the defaults fill, so the result is dense (jbuehler23's shape from
  `dynamic_bsn.rs:406-414`, but complete rather than partial).
- COERCE.

**Case C — different type from expected, non-enum.** Fully construct: `ReflectDefault` of
the named type, `try_apply` the partial struct/tuple built as in §4.6.5, then COERCE. This
mirrors `(#ty).into()` (`codegen.rs:744`).

#### 4.6.5 Building a partial struct / tuple-struct value

Named fields (`BsnValue::Struct`):
```
let info = expected_or_named.type_info().as_struct()?;   // else TypeNotStruct
let mut ds = DynamicStruct::default();
ds.set_represented_type(Some(type_info));                 // preserves TypePath in errors
for (name, v) in fields {
    let field = info.field(name).ok_or(UnknownField { type_path, field: name, span })?;
    if !seen.insert(name) { return Err(DuplicateField { .. }); }   // macro parity: codegen.rs:484-490
    let field_reg = registry.get(field.ty().id()).ok_or(TypeNotRegistered)?;
    ds.insert_boxed(name.clone(), build_value(cx, v, field_reg)?);
}
```
**Unknown fields are a hard error**, matching the reflect *deserializer*'s strictness and
the macro's compile error, and deliberately *not* `try_apply`'s silent skip
(`structs.rs:503-506`). Duplicate fields are a hard error, matching `codegen.rs:484-490`.

Tuple fields (`BsnValue::NamedTuple`), the **partial leading fields** rule:
```
let info = <TupleStructInfo | TupleVariantInfo>;
if fields.len() > info.field_len() { return Err(TooManyTupleFields { .. }); }
let mut dts = DynamicTupleStruct::default();
for (i, v) in fields.iter().enumerate() {
    let field_reg = registry.get(info.field_at(i).unwrap().ty().id())?;
    dts.insert_boxed(build_value(cx, v, field_reg)?);
}
```
Fields `fields.len()..info.field_len()` are simply absent; `try_apply` on a tuple struct
iterates the incoming value's indices only (`tuple_struct.rs:108-120`), so trailing fields
keep their prior value — the `partial_tuple_struct` semantics (`lib.rs:1678-1697`) and
`Bar(2)` over `Bar(1,1,0)` → `Bar(2,1,0)` (`lib.rs:1803`).

#### 4.6.6 `Option` and the "optionish" rule

`bsn!` relies on `impl<T> From<T> for Option<T>` (std) and
`impl<T> From<T> for OptionTemplate<T>` (`bevy_ecs/src/template.rs:539-543`) plus the
implicit `.into()`; so `field: 5` sets `Some(5)` for both `Option<u32>` and
`OptionTemplate<u32>` fields.

Reflection equivalent (coercion step 3): `expected` is **optionish** iff its `TypeInfo` is
`TypeInfo::Enum(e)` with exactly two variants, one named `None` (unit) and one named `Some`
(tuple, 1 field). If so, and the produced value is not already of the expected type, then:
```
let inner_reg = registry.get(e.variant("Some")?.as_tuple_variant()?.field_at(0)?.ty().id())?;
let inner = COERCE(produced, inner_reg)?;             // recurse the tail on the payload type
DynamicEnum::new("Some", DynamicVariant::Tuple(DynamicTuple::from_iter([inner])))
```
Explicit `None` / `Some(x)` in the document are just `BsnValue::Path`/`NamedTuple` and go
through §4.6.3/§4.6.4 (the enum-variant path) and match by name, so both spellings work.

#### 4.6.7 `BsnValue::List`

Port of `dynamic_bsn.rs:671-693` with error handling:
`expected.type_info().as_list()` (else `ValueTypeMismatch`), item type registration,
recurse per element, `DynamicList::push_box`, `set_represented_type`.
Works for `Vec<T>` fields whose template is `Vec<T>` (the `Clone + Default` blanket), and —
per §6.8 — for `#[template(built_in)]` fields whose template is `VecTemplate<T>`, by
unwrapping a single-field tuple struct whose field is a list before matching.

#### 4.6.8 `BsnValue::EntityRef(String)` — `#Name` as a value

```
if expected.type_id() != TypeId::of::<EntityTemplate>() {
    // still allow a user conversion into e.g. a newtype
    → COERCE(Box::new(entity_template), expected)
}
let reference = SceneEntityReference::from_asset(cx.source_path_hash, cx.name_node_id(name));
Box::new(EntityTemplate::SceneEntityReference(reference)) as Box<dyn Reflect>
```
`SceneEntityReference::from_asset` is Contract C item 4, built on the ratified
`SceneEntityReferenceSource` `Copy` enum (`CallSite { .. } | Asset { path_hash: u64 }`), so
`EntityTemplate` stays `Copy`. `cx.source_path_hash` is the hash of `DynamicSceneInner::source`,
computed once per build. The `name → node_id` map is per-document and stable across re-parses
of an unchanged file, which is what makes `ChildOf(#Root)` (`child_of_template`,
`lib.rs:1930-1956`) resolve to the same entity as the `#Root` declaration.
Requires prerequisite P3. Two invariants come with the asset-based identity:
(a) `path_hash` collisions across files are possible and documented as accepted risk in
Contract C; (b) `ResolvedSceneRoot::apply` must keep building a fresh
`SceneEntityReferences` per apply (`resolved_scene.rs:70`) — sharing one across applies
would alias references between spawns of the same `.bsn` (Contract C item 4 note; pinned by
SPEC-6 test #24).

#### 4.6.9 `BsnValue::Unit` / `BsnValue::Tuple`

`Unit` ⇒ `expected` must be `()`; else `ValueTypeMismatch`.
`Tuple` ⇒ build a `DynamicTuple` against `TypeInfo::Tuple`, element-wise. (Rarely used;
included because `BsnValue::Tuple` exists in Contract D.)

### 4.7 `DynamicComponentTemplate`

```rust
// crates/bevy_scene/src/dynamic/template.rs

/// An `ErasedComponentTemplate` whose value is held reflectively.
///
/// # This type MUST NOT implement `Clone`
///
/// `ErasedComponentTemplate` has a blanket impl for `T: Template<Output: Component>`
/// (`resolved_scene.rs:714`), and `Template` itself is blanket-implemented for
/// `T: Clone + Unpin` (`bevy_ecs/src/template.rs:390`). A `Clone` (and therefore `Unpin`)
/// `DynamicComponentTemplate` would pick up both blankets and **collide** with the manual
/// impl below — a coherence error, and SPEC-2's stated constraint (SPEC-0 §7, Contract E).
/// It cannot derive `Clone` anyway (`Box<dyn Reflect>` is not `Clone`). Duplication goes
/// through `ErasedComponentTemplate::clone_template` (§4.7.3) and nothing else.
/// Do not "simplify" this by adding `#[derive(Clone)]`.
pub struct DynamicComponentTemplate {
    /// Slot key. Equals `TypeId` of the concrete type inside `value`.
    template_type_id: TypeId,
    /// The template value. Always a *concrete* value (never a `Dynamic*`), constructed by
    /// `ReflectDefault` and mutated by `try_apply`.
    value: Box<dyn Reflect>,
    /// From the OUTPUT type's registration; provides `push_to_bundle_writer` (Contract A).
    reflect_component: ReflectComponent,
    /// `Some` iff `Output != Self`.
    reflect_template: Option<ReflectTemplate>,
    /// Fallbacks for `clone_template`.
    reflect_default: ReflectDefault,
    reflect_from_reflect: Option<ReflectFromReflect>,
    /// Live registry handle. NOTE: this is *not* made redundant by Contract C's new
    /// `ResolveContext::type_registry` (C-7). `ResolveContext` exists only during
    /// `Scene::resolve`; `ErasedComponentTemplate::apply` runs later and receives a
    /// `TemplateContext`, which has no registry field — yet `push_to_bundle_writer`
    /// (Contract A) takes `&TypeRegistry`, and taking it from
    /// `TemplateContext::resource::<AppTypeRegistry>()` would hold an immutable borrow of
    /// `context.entity` across the `world_mut()` call in §4.7.2. Keep the `Arc`.
    type_registry: AppTypeRegistry,
}
```

#### 4.7.1 Construction

Only ever from a `DynamicPatch`, inside the `get_or_insert_erased_template` default
closure (§4.8): `value = patch.reflect_default.default()`, the four type-data fields cloned
from the patch, `type_registry` cloned from the `DynamicScene`. `ReflectDefault` returns
`Box<dyn Reflect>` of the concrete template type (`std_traits.rs:26`), so
`Any::type_id(&*value) == template_type_id` holds by construction.

#### 4.7.2 `apply` — the ordering is load-bearing

```rust
unsafe fn apply(&self, context: &mut TemplateContext, bundle_writer: &mut BundleWriter)
    -> Result<(), BevyError>
{
    // 1. Build the output. MUST happen before acquiring the registry read guard: a user
    //    `build_template` may itself lock the registry, and a same-thread re-entrant read
    //    can deadlock if a writer is queued.
    let output: Box<dyn Reflect> = match &self.reflect_template {
        Some(t) => (t.build_template)(&*self.value, context)?,
        // No ReflectTemplate ⇒ Output == Self ⇒ the "template" is the component.
        None => self.value.reflect_clone()?,
    };

    // 2. Registry guard from our OWN Arc — independent of any borrow of `context`.
    let registry = self.type_registry.read();

    // 3. Components registrator. SAFETY: `world_mut` is used only to register components,
    //    which does not change the entity's location (same argument as resolved_scene.rs:722).
    let mut components = unsafe { context.entity.world_mut().components_registrator() };

    // 4. Single-archetype-move insertion (Contract A). SAFETY: `bundle_writer` and
    //    `components` come from the same World, per this method's own safety contract.
    unsafe {
        (self.reflect_component.push_to_bundle_writer)(
            output.into_partial_reflect(), &registry, &mut components, bundle_writer,
        )?;
    }
    Ok(())
}
```

This is the `dynamic_bsn.rs:960-987` body, ported: pcwalton's step 4 was
`context.entity.insert_reflect(output)`, i.e. an immediate insert and therefore an archetype
move **per dynamic component** (delta D2). Contract A's `push_to_bundle_writer` keeps the
whole entity to one move.

**Borrow choreography (the reason step 2 uses `self.type_registry`).**
`TemplateContext::resource::<AppTypeRegistry>()` borrows `context.entity` immutably
(`template.rs:74-77`), which conflicts with the `&mut` needed by `world_mut()` in step 3.
pcwalton dodged this by scoping the resource borrow into a block that ends before the insert
(`dynamic_bsn.rs:962-972`) — that works only because he did not need the registry *during*
the insert. We do (Contract A takes `&TypeRegistry`), so the handle must be owned by the
template. `AppTypeRegistry` is `AppTypeRegistry(pub TypeRegistryArc)`
(`bevy_ecs/src/reflect/mod.rs:36`), i.e. an `Arc` clone.

#### 4.7.3 `clone_template` — the reflect-clone ladder

`clone_template` returns `Box<dyn ErasedComponentTemplate>` with **no error channel**, so
the ladder must be total. Modelled on
`crates/bevy_world_serialization/src/reflect_utils.rs:11-24`, adapted to keep a *concrete*
`Box<dyn Reflect>` (a `to_dynamic()` result is only `PartialReflect` and would break
`build_template`, which takes `&dyn Reflect`):

```
1. self.value.reflect_clone()                                    → Box<dyn Reflect>
2. self.reflect_from_reflect?.from_reflect(self.value.as_partial_reflect())
3. let mut v = self.reflect_default.default();
   if let Ok(d) = self.value.to_dynamic() { let _ = v.try_apply(&*d); }   // best effort
   v
```
Step 3 always yields a value of the right concrete type, so `clone_template` is infallible.
Step 1 succeeds for every `#[derive(Reflect)]` type without `#[reflect(opaque)]` non-`Clone`
fields, so 2 and 3 are cold paths.

#### 4.7.4 The remaining trait methods

```rust
fn template_type_id(&self) -> TypeId { self.template_type_id }     // addendum C-6
fn try_as_partial_reflect(&self) -> Option<&dyn PartialReflect> {
    Some(self.value.as_partial_reflect())
}
fn try_as_partial_reflect_mut(&mut self) -> Option<&mut dyn PartialReflect> {
    Some(self.value.as_partial_reflect_mut())
}
```

### 4.8 `Scene::resolve` for `DynamicScene`

```rust
impl Scene for DynamicScene {
    fn resolve(self, context: &mut ResolveContext, scene: &mut ResolvedScene)
        -> Result<(), ResolveSceneError>
    {
        let inner = &*self.0;          // by-value self, Arc deref — delta D1
        resolve_entity(&inner.root, &inner.registry, context, scene)
    }

    fn register_dependencies(&self, dependencies: &mut SceneDependencies) {
        if let Some(base) = &self.0.root.base {
            dependencies.register::<ScenePatch>(base.clone());
        }
        for (type_id, path) in &self.0.dependencies {
            dependencies.register_erased(*type_id, path.clone());
        }
    }
}
```
(`self.0.dependencies` already contains the base includes of *nested* entities plus every
handle path; see §4.10. The root base is registered separately so the flattening step in
§4.10 does not need to special-case it.)

```rust
fn resolve_entity(
    entity: &DynamicSceneEntity,
    registry: &AppTypeRegistry,
    context: &mut ResolveContext,
    scene: &mut ResolvedScene,
) -> Result<(), ResolveSceneError> {
    // (1) BASE INCLUDE FIRST. `include_cached` rejects a late include
    //     (resolved_scene.rs:556-561), and everything below depends on `context.cached`
    //     being set so copy-on-write can find the base's templates.
    if let Some(base) = &entity.base {
        CachedSceneAsset(base.clone()).resolve(context, scene)?;
    }

    // (2) NAME. Uses the same inline path the macro uses (codegen.rs:289-295 →
    //     scene.rs:471-478): pushes the entity reference and patches `Name` through
    //     `get_or_insert_template`, so it participates in copy-on-write.
    if let Some(n) = &entity.name {
        NameEntityReference { name: n.name.clone(), reference: n.reference }
            .resolve_inline(context, scene);
    }

    // (3) PATCHES, in document order.
    for patch in &entity.patches {
        resolve_patch(patch, registry, context, scene)?;
    }

    // (4) RELATIONS, in document order.
    for relation in &entity.relations {
        // Children of a cached-base entity are appended by the cached ResolvedScene itself
        // at apply time (resolved_scene.rs:284-287); they must NOT be merged into ours, and
        // a child MUST NOT consult the parent's cached patch for copy-on-write — doing so
        // would clone the base ROOT's templates into a CHILD and then panic on
        // `self.cached.as_mut().unwrap()` (resolved_scene.rs:490-494), because the child
        // has no cached scene of its own. See delta D14 / §6.9.
        let saved = context.cached.take();
        let related = scene.get_or_insert_related_resolved_scenes_erased(&relation.data);
        for child in &relation.children {
            let mut child_scene = ResolvedScene::default();
            resolve_entity(child, registry, context, &mut child_scene)?;
            related.scenes.push(child_scene);
        }
        context.cached = saved;
    }
    Ok(())
}
```

Note the borrow shape in (4): `related` borrows `scene` mutably for the whole loop while
`resolve_entity` writes into a *separate* `child_scene` and mutates only `context`. This is
exactly what `RelatedScenes::resolve` does (`scene.rs:387-396`), so it borrow-checks.
Using Contract C item 3's erased accessor (keyed by `data.relationship_type_id`) guarantees
that dynamic `Children [...]` and a `bsn!` `Children [...]` on the same entity land in the
**same** `RelatedResolvedScenes` entry and therefore in one `Children` collection.

```rust
fn resolve_patch(
    patch: &DynamicPatch,
    registry: &AppTypeRegistry,
    context: &mut ResolveContext,
    scene: &mut ResolvedScene,
) -> Result<(), ResolveSceneError> {
    // The default closure captures cloned type data — hence Contract C item 1
    // (`fn()` → `impl FnOnce`). Delta D6.
    let (rd, rc, rt, rfr, reg) = (
        patch.reflect_default.clone(), patch.reflect_component.clone(),
        patch.reflect_template.clone(), patch.reflect_from_reflect.clone(),
        registry.clone(),
    );
    let template_type_id = patch.template_type_id;
    let erased = scene.get_or_insert_erased_template(context, template_type_id, move || {
        Box::new(DynamicComponentTemplate {
            template_type_id,
            value: rd.default(),
            reflect_component: rc,
            reflect_template: rt,
            reflect_default: rd,
            reflect_from_reflect: rfr,
            type_registry: reg,
        })
    });

    let value = erased_template_reflect_mut(
        erased, template_type_id, &patch.template_type_path, registry,
    )?;

    match &patch.value {
        DynamicPatchValue::Ensure => {}
        DynamicPatchValue::Partial(v) => {
            value.try_apply(&**v).map_err(|error| ResolveSceneError::ApplyFailed {
                type_path: patch.template_type_path.to_string(), error,
            })?;
        }
        DynamicPatchValue::EnumVariant { variant, full, partial } => {
            let ReflectRef::Enum(current) = value.reflect_ref() else {
                return Err(ResolveSceneError::ApplyFailed {
                    type_path: patch.template_type_path.to_string(),
                    error: ApplyError::MismatchedKinds {
                        from_kind: ReflectKind::Enum, to_kind: value.reflect_kind(),
                    },
                });
            };
            // Reflection equivalent of codegen.rs:443-449.
            let source = if current.variant_name() == &**variant { partial } else { full };
            value.try_apply(&**source).map_err(|error| ResolveSceneError::ApplyFailed {
                type_path: patch.template_type_path.to_string(), error,
            })?;
        }
    }
    Ok(())
}
```

### 4.9 Interop with typed (`bsn!`-created) templates

Two directions. Both merge correctly, but by **different mechanisms**, because the concrete
type in the slot differs:

| Direction | Slot's concrete type | Mechanism | Owner |
|---|---|---|---|
| dynamic patch → typed slot (§4.9.1) | the real template type `T` | `ReflectFromPtr` on `T` recovers `&mut dyn Reflect` in place | SPEC-4 |
| typed patch → dynamic slot (§4.9.2) | `DynamicComponentTemplate` | C-7 typed recovery: `try_as_partial_reflect` + `ReflectFromReflect` → typed box replaces the slot | SPEC-2 |

#### 4.9.1 Dynamic patch over a typed slot

Happens when a `bsn!` scene patches `Transform` and then includes a dynamic `.bsn` (via a
scene function or `{expr}`) that also patches `Transform`, or when SPEC-5 applies a
`.bsn` `ScenePatch` onto an entity whose scene already has a typed template. The slot holds
a `TransformTemplate` boxed through the generic impl (`resolved_scene.rs:714-732`), whose
`try_as_partial_reflect_mut` is the Contract C default, i.e. `None`.

```rust
fn erased_template_reflect_mut<'a>(
    erased: &'a mut dyn ErasedComponentTemplate,
    template_type_id: TypeId,
    type_path: &str,
    registry: &AppTypeRegistry,
) -> Result<&'a mut dyn PartialReflect, ResolveSceneError> {
    // Two-step probe: a direct `if let Some(v) = erased.try_as_partial_reflect_mut()`
    // followed by further use of `erased` is rejected by NLL. Contract C item 2's
    // immutable variant exists precisely to make this shape work.
    if erased.try_as_partial_reflect().is_some() {
        return Ok(erased.try_as_partial_reflect_mut().unwrap());
    }

    // Typed template. Recover `&mut dyn Reflect` through `ReflectFromPtr`, which every
    // `#[derive(Reflect)]` type registers automatically
    // (bevy_reflect/derive/src/registration.rs:68-70).
    let any: &mut dyn Any = erased;              // trait upcast; `Any` is a supertrait of
                                                 // ErasedComponentTemplate (resolved_scene.rs:696)
    if (*any).type_id() != template_type_id {
        return Err(ResolveSceneError::UnpatchableTemplate { type_path: type_path.into() });
    }
    let registry = registry.read();
    let registration = registry.get(template_type_id)
        .ok_or_else(|| ResolveSceneError::TypeNotRegistered { type_path: type_path.into() })?;
    let from_ptr = registration.data::<ReflectFromPtr>()
        .ok_or_else(|| ResolveSceneError::UnpatchableTemplate { type_path: type_path.into() })?;
    debug_assert_eq!(from_ptr.type_id(), template_type_id);

    // SAFETY: `any` is an exclusive reference to a value whose concrete type id was just
    // checked to equal `template_type_id`, which is the type `from_ptr` was created for.
    // `PtrMut::from(&mut T)` for `T: ?Sized` keeps the data address of the trait object
    // (bevy_ptr/src/lib.rs:965-972). The returned reference inherits `any`'s exclusive 'a.
    let reflect = unsafe { from_ptr.as_reflect_mut(PtrMut::from(any)) };
    Ok(reflect.as_partial_reflect_mut())
}
```

The error is kept, but only for the genuinely unrecoverable case (slot occupied by a
template of a *different* type, or a type registered without `ReflectFromPtr`), and it uses
Contract C's ratified `UnpatchableTemplate` variant. Rationale for recovering rather than
erroring outright: the recoverable case is the common one, the recovery is 15 lines, and the
alternative is that `bsn! { Transform { … } { load_bsn("x.bsn") } }` fails at runtime.
(Approved as a deviation from the original brief's "None → error" wording; OQ-4 closed.)

#### 4.9.2 Typed patch over a dynamic slot — RESOLVED by C-7 typed recovery

This is `bsn! { :"player.bsn" Transform { x: 1. } }`, the headline use case of the whole
series (SPEC-0 §1). Sequence:

1. `CachedSceneAsset::resolve` sets `context.cached` to the resolved `player.bsn` patch
   (`scene.rs:428-433`).
2. `get_or_insert_template::<TransformTemplate>` → `get_or_insert_erased_template`
   (`resolved_scene.rs:416`) finds the base's slot for `TypeId::of::<TransformTemplate>()`
   (our slots use the **real** template `TypeId`, SPEC-0 decision #3) and calls
   `clone_template()` (`resolved_scene.rs:476`) — producing another
   `DynamicComponentTemplate` holding the base's *populated* `TransformTemplate` value.
3. `(… as &mut dyn Any).downcast_mut::<TransformTemplate>()` (`resolved_scene.rs:420-421`)
   returns `None` — the box's concrete type genuinely *is* `DynamicComponentTemplate`.
   Before C-7 this `unwrap()`ed and panicked.

**Ratified fix (SPEC-2 owns the code; SPEC-0 §7, Contract C item C-7):** the `None` arm
performs **typed recovery** rather than panicking or discarding:

```rust
// bevy_scene/src/resolved_scene.rs, inside get_or_insert_template::<T>
let erased = self.get_or_insert_erased_template(context, TypeId::of::<T>(), || Box::new(T::default()));
if let Some(typed) = (erased as &mut dyn Any).downcast_mut::<T>() { return typed; }

// The slot holds a foreign (reflected) representation of the same template type.
let recovered: Option<Box<T>> = context.type_registry
    .and_then(|registry| {
        let value = erased.try_as_partial_reflect()?;                 // Contract C item 2
        let from_reflect = registry.get_type_data::<ReflectFromReflect>(TypeId::of::<T>())?;
        from_reflect.from_reflect(value)                              // Box<dyn Reflect>, concrete T
    })
    .and_then(|boxed| boxed.downcast::<T>().ok());

match recovered {
    Some(typed) => { self.insert_erased_template(TypeId::of::<T>(), typed); /* re-fetch &mut T */ }
    None => {
        error!("template slot for {} holds a representation that could not be converted \
                (no ResolveContext type registry, or no ReflectFromReflect); resetting to \
                default", core::any::type_name::<T>());
        self.insert_erased_template(TypeId::of::<T>(), Box::new(T::default()));
    }
}
```

Why this works and why it is enough:

- `DynamicComponentTemplate::try_as_partial_reflect` returns `Some` (§4.7.4) and the value
  behind it is the **concrete** template type, never a `Dynamic*` (§4.7.1, §4.7.3). So
  `ReflectFromReflect::from_reflect` takes the fast `downcast` path and reproduces the
  base's field values exactly.
- `ReflectFromReflect` is present for every `#[derive(Reflect)]` type that is not
  `#[reflect(from_reflect = false)]`, which SPEC-1 Phase 5's `#[template(reflect)]` seed set
  guarantees (P1). So the merge path — not the fallback — is the normal outcome.
- Result: **`bsn! { :"player.bsn" Transform { x: 1. } }` merges correctly.** `player.bsn`'s
  `Transform` fields survive, the `bsn!` patch is applied on top, and everything downstream
  (`clone_template`, `duplicate_templates` with C-6, `apply`) sees an ordinary typed
  template.
- `ResolveContext::type_registry` is `Option<&TypeRegistry>` because a handful of test-only
  and `World`-less resolve paths have no registry; those degrade to the reset+`error!`
  fallback (§7 row 9b), which is a diagnostic, not UB.

`ReflectFromPtr` (§4.9.1) is *not* usable in this direction — it needs the box's concrete
type to be `T`, and here it is not. That asymmetry is why the two directions use different
mechanisms.

**Recorded upstream-alignment option (NOT required for v1, not part of this series' scope):**
move `ErasedComponentTemplate`/`ErasedBundleTemplate` from `bevy_scene::resolved_scene` into
`bevy_ecs::template` (they reference only `bevy_ecs` types: `BundleWriter`,
`ComponentsRegistrator`, `TemplateContext`, `Component`, `BevyError`) and give
`ReflectTemplate` an
`erase: fn(Box<dyn Reflect>) -> Result<Box<dyn ErasedComponentTemplate>, Box<dyn Reflect>>`
field, so `DynamicComponentTemplate::clone_template` could hand back a *typed* box and the
downcast at step 3 would simply succeed. That would remove the recovery step entirely, at
the cost of a crate move and a wider registration requirement. C-7's typed recovery achieves
the same observable semantics without either.

### 4.10 `register_dependencies` and how a string is known to be a handle

Dependencies are computed **at build time**, not at dependency-walk time, and stored flat in
`DynamicSceneInner::dependencies`.

- **Base includes.** Every `DynamicSceneEntity::base`, at any depth, contributes
  `(TypeId::of::<ScenePatch>(), path)` — matching `CachedSceneAsset::register_dependencies`
  (`scene.rs:439-441`). The root's base is emitted by `register_dependencies` directly and
  nested ones are pushed into the flat list during the build walk.
- **Handle strings.** In §4.6.2 step 4, immediately after a successful `ReflectConvert`,
  run:
  ```
  let asset_type_id = expected.data::<ReflectTemplate>()          // addendum A-1
      .and_then(|t| registry.get(t.output_type_id))               // e.g. Handle<Image>
      .and_then(|r| r.data::<ReflectHandle>())                    // bevy_asset/src/reflect.rs
      .map(|h| h.asset_type_id);
  if let Some(id) = asset_type_id {
      cx.dependencies.push((id, AssetPath::parse(s).into_owned()));
  }
  ```
  That is: *"the expected field type is a template whose output is a `Handle<A>`"*. This is
  the only structural test needed, it is generic over `A`, and it also covers any future
  template type that produces a handle.
  If `ReflectTemplate` is absent (addendum A-1 not implemented, or a bespoke conversion that
  is not a handle), nothing is registered and the asset loads lazily at spawn time via
  `HandleTemplate::build_template` (`bevy_asset/src/handle.rs:343`) — correct, just later.

`ScenePatch::load_with(load_context, dynamic_scene)` (`scene_patch.rs:41-53`) then turns each
pair into an `UntypedHandle` through `LoadContext::load_from_path_erased`
(`bevy_asset/src/reflect.rs:405-411`), so the `.bsn`'s assets become real load-context
dependencies and `LoadedWithDependencies` (`spawn.rs:618`) waits for them.

---

## 5. Step-by-step implementation plan

Each step compiles and is independently testable.

1. **Skeleton.** Create `dynamic/` module + `bsn_asset` feature. Add `DynamicScene` with an
   empty `DynamicSceneEntity` and a `Scene` impl that does nothing. Assert
   `fn assert_scene<S: Scene>() {}` on it.
2. **Prerequisites (all ratified; SPEC-4 cannot start step 3 without them).**
   SPEC-1 Phase 5 lands P1 + P3 (`#[template(reflect)]`, seed set,
   `EntityTemplate`/`OptionTemplate`/`VecTemplate` `Reflect`) and Contract A with A-1.
   SPEC-2 lands Contract C items 1–5, C-6, C-7 (incl. `ResolveContext::type_registry`) and
   the `UnpatchableTemplate` variant. Two `bevy_scene` unit tests gate this step:
   `TypeId::of::<TransformTemplate>()`-keyed slots skip correctly under
   `duplicate_templates` (C-6), and `get_or_insert_template::<T>` recovers a typed value
   from a foreign reflect-exposing occupant instead of panicking (C-7).
3. **Symbol resolution.** `resolve_symbol` + `template_registration` + the build-error enum.
   Unit tests: unit struct, enum variant, unknown type, non-enum parent path.
4. **Value construction — scalars.** `Int`/`Float`/`Bool`/`String`, the coercion tail,
   `ReflectConvert`. Unit tests per row of the tables in §4.6.1–2.
5. **Value construction — composites.** `Path`, `Struct`, `NamedTuple`, `List`, optionish,
   `EntityRef`. Unit tests per §8.1.
6. **`DynamicComponentTemplate`.** Construction, `apply`, `clone_template` ladder,
   `try_as_partial_reflect{,_mut}`, `template_type_id`. Test by hand-building a
   `ResolvedScene` and applying it.
7. **`DynamicPatch` + `resolve_patch`.** Wire steps 3–6 together. Test the enum
   match-or-reset in isolation.
8. **`resolve_entity`.** Base include, name, patches, relations, `context.cached`
   save/restore.
9. **`register_dependencies`** and the handle probe.
10. **`erased_template_reflect_mut`** including the `ReflectFromPtr` fallback and its test.
11. **Integration tests** (§8.2), built from hand-constructed `BsnDocument` fixtures so
    SPEC-4 can be merged before SPEC-5's loader.

---

## 6. Edge cases and error handling

### 6.1 Build-time errors (`DynamicSceneBuildError`)

`thiserror`, every variant carries `span: Span` and the source path is added by the loader
(SPEC-5).

| Variant | Cause |
|---|---|
| `UnknownType { type_path }` | §4.4.1 exhausted |
| `TypeNotRegistered { type_path }` | a field/template/output `TypeId` has no registration |
| `MissingReflectDefault { type_path }` | template type, or an enum-variant field type, lacks `ReflectDefault` |
| `MissingReflectComponent { type_path }` | output type lacks `ReflectComponent` |
| `UnknownVariant { type_path, variant }` | `resolve_symbol` matched a parent enum without that variant |
| `TypeNotStruct { type_path }` / `TypeNotTupleStruct { type_path }` | named-field syntax on a tuple type, or vice versa |
| `UnknownField { type_path, field }` | field not in `StructInfo`/`StructVariantInfo` |
| `DuplicateField { type_path, field }` | macro parity, `codegen.rs:484-490` |
| `TooManyTupleFields { type_path, given, expected }` | more positional values than the tuple has |
| `IntegerOutOfRange { value, type_path }` | §4.6.1 |
| `LiteralNotRepresentable { type_path }` | int→float exactness check |
| `ValueTypeMismatch { found, expected }` | coercion tail exhausted |
| `UnsupportedRelationship { type_path }` | relation symbol has no `ReflectRelationshipTarget` (Contract B) |
| `SceneComponentUnsupported { type_path }` | `@Type` entry (SPEC-0 §2) |
| `UnsupportedValueKind { kind }` | a `BsnValue` variant with no reflection encoding |

### 6.2 Resolve-time errors (`ResolveSceneError`, Contract C item 5)

| Variant | When |
|---|---|
| `MissingSceneDependency(path)` | base `.bsn` not loaded — raised by `CachedSceneAsset::resolve` (`scene.rs:435`) |
| `CachedSceneError(_)` | `MultipleCached` (two bases) or `LateCached` (base not first) — `resolved_scene.rs:551-561` |
| `ApplyFailed { type_path, error }` | any `try_apply` failure in `resolve_patch` |
| `UnpatchableTemplate { type_path }` | §4.9.1: the slot occupant is neither reflect-exposing nor recoverable via `ReflectFromPtr` (Contract C's ratified 8th variant) |
| `TypeNotRegistered { type_path }` | registry changed between load and resolve, only reachable from §4.9.1 |

`MissingReflectDefault` / `MissingReflectComponent` / `UnsupportedRelationship` /
`TypeNotReflectable` exist on `ResolveSceneError` per Contract C but are **not reachable**
from `DynamicScene` (the type data is captured at build time); they remain for other `Scene`
implementations. C-7's typed-recovery fallback (§4.9.2) is a logged `error!`, not a
`ResolveSceneError` — `get_or_insert_template` has no error channel.

### 6.3 Registry changed between load and resolve
Type data is cloned into `DynamicPatch`, so unregistering a type after load does not break
resolution; only §4.9.1's `ReflectFromPtr` lookup can observe the change and it returns an
error. Registering a type *after* load does not retroactively fix a failed load — the load
error already happened.

### 6.4 Two bases / late base
SPEC-3's grammar allows at most one `:"…"` and requires it first; if a malformed document
reaches us, `include_cached` produces `MultipleCached`/`LateCached`
(`resolved_scene.rs:550-561`). We do not duplicate the check.

### 6.5 Empty document / entity with no entries
`resolve` succeeds and contributes nothing; the spawned entity is empty. Matches
`bsn! {}` (`empty_scene_expressions`, `lib.rs:2040`).

### 6.6 `~Type` on a type that is not a component template
The output registration will lack `ReflectComponent` ⇒ `MissingReflectComponent` at build.
Bundle templates (`ErasedBundleTemplate`) are not reachable from `.bsn`; SPEC-0 decision #4
requires component templates so slots merge.

### 6.7 `&'static str` fields
Rejected (`ValueTypeMismatch`). Producing a `&'static str` from document text requires
`Box::leak`, which would leak unboundedly on every hot reload (SPEC-6). Users should use
`String` or `Cow<'static, str>`. Documented in the `.bsn` reference (SPEC-5).

### 6.8 `#[template(built_in)]` fields — supported
`OptionTemplate<T>` and `VecTemplate<T>` (`bevy_ecs/src/template.rs:520-586`) become
`Reflect` in SPEC-1 Phase 5 (ratified, P1/P3), so `#[template(built_in)]` fields work:
`OptionTemplate<T>` is an enum with `Some`/`None` variants and is therefore "optionish"
under §4.6.6 (the rule is deliberately structural, not `Option`-specific, precisely so it
covers both); `VecTemplate<T>` is a tuple struct over `Vec<T>`, so `[a, b]` is built by
§4.6.7 against its field-0 list type rather than against the template directly — the
`build_value` list arm must therefore unwrap a single-field tuple struct whose field is a
list before reporting `ValueTypeMismatch`. Blanket `Option<T>`/`Vec<T>` fields (the common
case) continue to work directly via §4.6.6/§4.6.7.

### 6.9 `context.cached` and child entities
Cleared and restored around each relation (§4.8 step 4). Without this, a child would clone
the *base root's* template into itself and then hit
`self.cached.as_mut().unwrap()` (`resolved_scene.rs:490-494`) on a scene with no cached
info — a panic. This is a latent bug in `main` reachable from `bsn!` too (base root and a
local child both patching the same component); SPEC-4 does not fix the macro path but must
not walk into it.

### 6.10 No panics anywhere
- No `unwrap`/`expect` outside `debug_assert!`s and the two provably-safe `unwrap()`s
  (`try_as_partial_reflect_mut` right after `try_as_partial_reflect().is_some()`;
  `info.field_at(i)` right after a bounds check).
- `from_reflect_with_fallback`'s panic (`bevy_ecs/src/reflect/mod.rs:141`) is not inherited:
  the values we hand to `push_to_bundle_writer` are always concrete (§4.7.1, §4.7.3), so
  Contract A's "downcast first" path always hits.
- `push_to_bundle_writer` returns `Result`; `apply` propagates it as `BevyError`, which
  `ResolvedScene::apply` maps to `ApplySceneError::TemplateBuildError`
  (`resolved_scene.rs:333`).

---

## 7. Merge-semantics conformance table

`D` = dynamic (`.bsn`), `S` = static (`bsn!`). "Reference" cites the existing macro-side
test that pins the expected outcome.

| # | Scenario | Expected outcome | Mechanism | Reference test |
|---|---|---|---|---|
| 1 | D `Position { y: 2. }` then D `Position { x: 1. }` on one entity | `x=1, y=2, z=0` | one slot keyed by `TypeId::of::<PositionTemplate>()`; two `try_apply`s of disjoint `DynamicStruct`s | `cached_patching_order` `lib.rs:1050-1081` |
| 2 | D `Foo { x: 1, nested: Bar(1,1) }` then D `Foo { y: 2, nested: Bar(2) }` | `Foo { x:1, y:2, z:0, nested: Bar(2,1,0) }` | §4.6.4 case A (nested partial) + §4.6.5 leading-tuple rule | `struct_patching` `lib.rs:1803-1850`; `field_patching_with_default` `lib.rs:1853-1899` |
| 3 | D `TupleStruct(0.1)` on a `(f32, u32)` | `.0 = 0.1`, `.1 = 0` | §4.6.5: only index 0 present in the `DynamicTupleStruct` | `partial_tuple_struct` `lib.rs:1678-1697` |
| 4 | D `Foo::Baz(10)`, then D `Foo::Bar { x: 1 }`, then D `Foo::Bar { y: 2 }` | `Foo::Bar { x:1, y:2, z:0 }` | `EnumVariant`: 2nd patch sees variant `Baz` ≠ `Bar` ⇒ apply `full` (defaults + `x=1`); 3rd sees `Bar` == `Bar` ⇒ apply `partial` (`y=2` only) | `enum_patching` `lib.rs:1741-1799` |
| 5 | …then D `Foo::Qux` | `Foo::Qux` | `full` for a unit variant is `DynamicEnum(Qux, Unit)` | `enum_patching` `lib.rs:1796-1799` |
| 6 | D `TextFont { font_size: TextSize::Large }` where `From<TextSize> for FontSize` is registered as a conversion | `FontSize(24)` | §4.6.3 + coercion step 2 | `enum_variant_field_values_use_implicit_into` `lib.rs:2740-2773` |
| 7 | D `Sprite("image.png")` | `Handle<Image>` equal to `asset_server.load("image.png")`; the path appears in `ScenePatch::dependencies` | §4.6.2 step 4 (`ReflectConvert` at `bevy_asset/src/lib.rs:704`) + §4.10 | `handle_template` `lib.rs:1903-1927` |
| 8 | **D over S**: `bsn! { Position { y: 2. } {dynamic_scene} }` where the dynamic scene patches `Position { x: 1. }` | `x=1, y=2, z=0` | slot holds a typed `PositionTemplate`; `try_as_partial_reflect_mut` → `None` → §4.9.1 `ReflectFromPtr` recovery → `try_apply` | analogue of `inline_scene_patching` `lib.rs:1209-1253` |
| 9 | **S over D (uncached)**: `bsn! { {dynamic_scene} Position { x: 1. } }` where the dynamic scene patches `Position { y: 2. }` | `x=1, y=2, z=0` | `get_or_insert_template::<PositionTemplate>` finds a `DynamicComponentTemplate`; C-7 typed recovery converts it via `ReflectFromReflect` and replaces the slot with the typed box (§4.9.2) | `inline_scene_patching` `lib.rs:1209-1253` |
| 9b | Same as 9 but `ResolveContext::type_registry` is `None`, or `PositionTemplate` lacks `ReflectFromReflect` | `x=1, y=0` **and an `error!` log**; never a panic | C-7 fallback (§4.9.2) | — |
| 10 | **S over D (cached base)** — *the headline case*: `bsn! { :"a.bsn" Position { x: 1. } Children [ #Y ] }`, `a.bsn` = `Position { y: 2. } Children [ #X ]` | children `[X, Y]`; `Position`: `x=1, y=2, z=0` | copy-on-write `clone_template` at `resolved_scene.rs:476` yields a `DynamicComponentTemplate` carrying `a.bsn`'s populated value; C-7 typed recovery converts it in place, so the base's `y` survives under the typed patch. Children are appended by `apply_related` (`resolved_scene.rs:284-287`) so they are unaffected | `loaded_asset_cached_patching` `lib.rs:1084-1206` |
| 11 | **D with a cached base**: `.bsn` `b` = `:"a.bsn"` + `Position { x: 1. }`; `a.bsn` = `Position { y: 2. }` | `x=1, y=2, z=0`; `Position` inserted **once** | base first (§4.8 step 1) ⇒ `context.cached` set; `get_or_insert_erased_template` clones the cached dynamic template and records `duplicate_templates`; addendum **C-6** makes the skip at `resolved_scene.rs:325` match | `loaded_asset_cached_patching` `lib.rs:1084-1206`; `cached_patching` `lib.rs:1003-1047` |
| 12 | Same as 11 **without** C-6 | `Position` pushed to the `BundleWriter` twice with the same `ComponentId` | `(**template).type_id()` returns `TypeId::of::<DynamicComponentTemplate>()`, never in `duplicate_templates` | — (this row is the justification for C-6) |
| 13 | D `Children [...]` and S `Children [...]` on the same entity | one `Children` collection, dynamic-then-static in document order | Contract C item 3 keys the erased accessor by `relationship_type_id`, the same key the generic accessor uses (`resolved_scene.rs:540`) | `hierarchy` `lib.rs:1256`; `scene_list_children` `lib.rs:1958` |
| 14 | D `#Root Children [ (#Child ChildOf(#Root)) ]` | `#Child`'s `ChildOf` resolves to the `#Root` entity | §4.6.8 + `SceneEntityReference::from_asset` identity | `bsn_name_references` `lib.rs:1341`; `child_of_template` `lib.rs:1930` |
| 15 | D names a fully-qualified path `::bevy_ecs::prelude::Children[]` | resolves | `get_with_type_path` after stripping the leading `::` (SPEC-3 normalizes) | `supports_fully_qualified_component_paths` `lib.rs:991-1000` |

---

## 8. Test plan

### 8.1 Unit tests — `crates/bevy_scene/src/dynamic/value.rs` (`mod tests`)

All build a minimal `TypeRegistry` with the fixture types, call `build_value`, and assert
on the produced `Box<dyn PartialReflect>` (via `try_apply` onto a concrete value + `assert_eq!`).

| Test | Asserts |
|---|---|
| `int_literal_exact_widths` | `1` into each of `i8..i128`, `u8..u128`, `isize`, `usize` yields the exactly-typed value |
| `int_literal_out_of_range_errors` | `300` into `u8` ⇒ `IntegerOutOfRange`; `-1` into `u32` ⇒ `IntegerOutOfRange` |
| `int_literal_into_float_exact` | `1` into `f32`/`f64` ⇒ `1.0` |
| `int_literal_into_float_inexact_errors` | `1 << 30` into `f32` ⇒ `LiteralNotRepresentable` |
| `float_literal_into_f32_and_f64` | `1.5` into both; `1.1` into `f32` ⇒ `1.1f32` |
| `float_literal_into_int_errors` | `1.0` into `u32` ⇒ `ValueTypeMismatch` |
| `bool_literal` | `true` into `bool`; into `u32` ⇒ error |
| `string_into_string_and_cow` | `"x"` into `String` and `Cow<'static, str>` |
| `string_into_static_str_errors` | `ValueTypeMismatch` (§6.7) |
| `string_into_handle_template_via_convert` | `"a.png"` into `HandleTemplate<Image>` ⇒ `HandleTemplate::Path(AssetPath::parse("a.png"))`, and one `(TypeId::of::<Image>(), "a.png")` dependency recorded |
| `string_into_asset_path` | `"a.png"` into an `AssetPath<'static>` field |
| `unit_struct_value` | `Marker` into a `Marker` field |
| `unit_enum_variant_value` | `MyEnum::Qux` into a `MyEnum` field ⇒ `DynamicEnum(Qux, Unit)` wrapping (jbuehler23 parity) |
| `enum_variant_value_via_reflect_convert` | `TextSize::Large` into an `FontSize` field ⇒ `FontSize(24)` |
| `struct_value_same_type_is_partial` | `Bar { a: 1 }` into a `Bar` field returns a `DynamicStruct` with exactly one field |
| `struct_value_other_type_is_full_then_converted` | full construction + `ReflectConvert` |
| `tuple_struct_partial_leading_fields` | `Bar(2)` into a 3-field `Bar` yields a `DynamicTupleStruct` of length 1 |
| `tuple_struct_too_many_fields_errors` | `TooManyTupleFields` |
| `unknown_field_errors` | `Bar { nope: 1 }` ⇒ `UnknownField` (**not** silently dropped, unlike raw `try_apply`) |
| `duplicate_field_errors` | `Bar { a: 1, a: 2 }` ⇒ `DuplicateField` |
| `enum_struct_variant_fills_defaults` | `MyEnum::Bar { x: 1 }` ⇒ `full` contains `x,y,z` with `y=z=0`; `partial` contains only `x` |
| `enum_tuple_variant_fills_defaults` | ditto for a tuple variant |
| `enum_variant_field_missing_reflect_default_errors` | field type without `ReflectDefault` ⇒ `MissingReflectDefault` |
| `option_field_implicit_some` | `5` into `Option<u32>` ⇒ `Some(5)`; `None` into `Option<u32>` ⇒ `None` |
| `option_field_explicit_some` | `Some(5)` ⇒ `Some(5)` |
| `list_value` | `[1, 2]` into `Vec<u8>` |
| `list_item_type_mismatch_errors` | `["a"]` into `Vec<u8>` ⇒ `ValueTypeMismatch` |
| `entity_ref_value` | `#Root` into an `EntityTemplate` field ⇒ `EntityTemplate::SceneEntityReference` with the document-stable reference |

### 8.2 Unit tests — `crates/bevy_scene/src/dynamic/build.rs`

| Test | Asserts |
|---|---|
| `resolve_symbol_unit_struct` / `_enum_variant` / `_fully_qualified` | §4.4.1 branches |
| `resolve_symbol_unknown_type_errors` | `UnknownType` with the full path |
| `resolve_symbol_unknown_variant_errors` | `a::MyEnum::Nope` ⇒ `UnknownVariant` (pcwalton would have accepted this) |
| `template_type_id_uses_reflect_from_template` | a `#[derive(FromTemplate)]` component's patch keys on `…Template`'s `TypeId` |
| `template_type_id_defaults_to_self` | a `Clone + Default` component keys on its own `TypeId` |
| `tilde_template_uses_named_type_directly` | `~HandleTemplate<Image>` |
| `scene_component_entry_errors` | `@Widget` ⇒ `SceneComponentUnsupported` |
| `unknown_relationship_errors` | `NotARelationship [ ]` ⇒ `UnsupportedRelationship` |
| `build_errors_carry_spans` | every error variant produced by a fixture document reports the right byte span |

### 8.3 Unit tests — `crates/bevy_scene/src/dynamic/template.rs`

| Test | Asserts |
|---|---|
| `dynamic_template_applies_component` | a hand-built `DynamicComponentTemplate` over `Position`, applied through `ResolvedSceneRoot::apply`, produces the component |
| `dynamic_template_single_archetype_move` | spawning an entity with 3 dynamic templates results in exactly one archetype (assert `Archetype::components()` and that no intermediate archetype was created — mirrors `scene_expression_passing_pointless` `lib.rs:1700-1738`) |
| `dynamic_template_uses_build_template` | a template with `ReflectTemplate` (e.g. `HandleTemplate<Image>`) produces the `Handle`, not the template |
| `clone_template_preserves_concrete_type` | `clone_template().try_as_partial_reflect().unwrap().reflect_type_path()` is the template's path, not a `Dynamic*` path |
| `clone_template_falls_back_to_from_reflect` | a type whose `reflect_clone` fails still clones |
| `template_type_id_is_the_template_not_the_wrapper` | addendum C-6 |
| `erased_reflect_mut_dynamic_slot` | returns `Some` via `try_as_partial_reflect_mut` |
| `erased_reflect_mut_typed_slot_via_reflect_from_ptr` | insert a real `PositionTemplate` through `insert_template`, then obtain `&mut dyn PartialReflect` and mutate it; assert the typed value changed |
| `erased_reflect_mut_unregistered_type_errors` | `UnpatchableTemplate` |
| `dynamic_component_template_is_not_clone` | a `trybuild`/`compile_fail` doc-test (or a comment-anchored `static_assertions::assert_not_impl_any!`) pinning that `DynamicComponentTemplate: !Clone`, so a future refactor cannot reintroduce the blanket-impl coherence collision (§4.7) |

### 8.4 Integration tests — `crates/bevy_scene/src/dynamic/mod.rs` (`mod tests`)

These mirror the existing `lib.rs` scene tests but build the scene from a hand-constructed
`BsnDocument` fixture (a `fn doc(src: &str) -> BsnDocument` helper over SPEC-3's parser), so
they do not depend on SPEC-5's loader. Each uses `test_app()`-style setup
(`lib.rs:980-988`) plus `app.register_type::<…>()` for the fixture types.

| Test | Mirrors | Asserts |
|---|---|---|
| `dynamic_struct_patching` | `struct_patching` `lib.rs:1803` | row 2 of §7 |
| `dynamic_field_patching_with_default` | `field_patching_with_default` `lib.rs:1853` | row 2 |
| `dynamic_partial_tuple_struct` | `partial_tuple_struct` `lib.rs:1678` | row 3 |
| `dynamic_enum_patching` | `enum_patching` `lib.rs:1741` | rows 4, 5 |
| `dynamic_primitive_literals` | `primitive_literals` `lib.rs:1517` | every primitive field type round-trips |
| `dynamic_handle_template` | `handle_template` `lib.rs:1903` | row 7 incl. `ScenePatch::dependencies.len() == 1` |
| `dynamic_hierarchy` | `hierarchy` `lib.rs:1256` | nested `Children [ … ]`, names in order |
| `dynamic_name_references` | `bsn_name_references` `lib.rs:1341` | row 14 |
| `dynamic_custom_relationship` | `scene_list_children` `lib.rs:1958` | a non-`Children` `RelationshipTarget` works (pcwalton hard-errored: `dynamic_bsn.rs:426`) |
| `dynamic_over_static_patch` | `inline_scene_patching` `lib.rs:1209` | row 8 — the `ReflectFromPtr` path |
| `static_over_dynamic_patch_merges` | `inline_scene_patching` | row 9 — `x=1, y=2, z=0`: C-7 typed recovery preserves the dynamic side's fields under the typed patch |
| `static_over_dynamic_patch_without_registry_resets` | — | row 9b — resolve with `ResolveContext::type_registry = None`; asserts `x=1, y=0`, that an `error!` was logged, and that **no panic** occurs |
| `dynamic_cached_base_patching` | `loaded_asset_cached_patching` `lib.rs:1084` | row 11: `x=1, y=2, z=0` **and** that `Position` was inserted once (assert `Archetype` component count, and add a `Drop`-counting component like `drop_is_called_for_uninserted_components` `lib.rs:2487` to prove no double push) |
| `static_cached_dynamic_base` | `loaded_asset_cached_patching` `lib.rs:1084` | row 10 (**the headline case**), using a `FakeSceneLoader` (`lib.rs:1133-1151`) that returns `ScenePatch::load_with(load_context, dynamic_scene)`: asserts `x=1, y=2, z=0` and children `[X, Y]` |
| `static_cached_dynamic_base_enum_component` | `enum_patching` `lib.rs:1741` | same shape with an enum component: the base's `Foo::Bar { y: 2 }` survives a typed `Foo::Bar { x: 1 }` on top, proving typed recovery does not go through a variant reset |
| `dynamic_children_of_cached_base_are_appended` | `cached_patching` `lib.rs:1003` | children are `[X, Y]`, and no panic from §6.9 |
| `dynamic_missing_base_dependency_errors` | — | resolving before the base loads ⇒ `MissingSceneDependency` |
| `dynamic_late_base_errors` | — | a malformed document with the base after a patch ⇒ `CachedSceneError::LateCached` |

---

## 9. Acceptance criteria

1. `cargo test -p bevy_scene --features bsn_asset` passes, including every test in §8.
2. `cargo clippy -p bevy_scene --features bsn_asset -- -D warnings` is clean; every `unsafe`
   block carries a `// SAFETY:` comment; no `unwrap`/`expect`/`panic!` outside `#[cfg(test)]`
   except the two justified ones in §6.10.
3. Every public item has a doc comment (SPEC-0 §7); `#![warn(missing_docs)]` holds.
4. `DynamicScene: Clone + Send + Sync + 'static` and implements `Scene` (compile-time
   assertions in the crate).
5. **Every** row of §7 (1–8, 9, 9b, 10, 11, 13–15) is demonstrated by a passing test — no
   `#[ignore]`s. Row 12 is a rationale row for C-6 and is covered indirectly by row 11's
   single-insertion assertion.
6. Spawning an entity from a purely dynamic scene with *n* components performs exactly one
   archetype move (test `dynamic_template_single_archetype_move`).
7. No `.bsn` input — well-formed or not — can cause a panic in `bevy_scene`; a fuzz-style
   test feeding malformed `BsnDocument` fixtures (wrong kinds, missing fields, extreme
   integers) asserts that every outcome is an `Err`.

---

## 10. Open questions

### 10.1 Closed by the review pass (SPEC-0 §7) — recorded for traceability

| # | Question | Resolution |
|---|---|---|
| OQ-1 | Addendum A-1, `ReflectTemplate::output_type_id` | **RATIFIED** into SPEC-1's Contract A. §4.2, §4.4.2 and §4.10 now assume it unconditionally; the "degradation if absent" paragraph is deleted. |
| OQ-2 | §4.9.2 typed-patch-over-dynamic-slot | **RESOLVED** by SPEC-2's ratified C-7 typed recovery (`ResolveContext::type_registry` + `ReflectFromReflect` conversion). §4.9.2 rewritten: the headline case now **merges**, it does not degrade. The crate-move + `ReflectTemplate::erase` proposal is retained only as a recorded upstream-alignment option. |
| OQ-3 | Addendum C-6, `ErasedComponentTemplate::template_type_id` | **RATIFIED** into SPEC-2 as specced. |
| OQ-4 | `ReflectFromPtr` recovery in §4.9.1 | **ACCEPTED.** The error path keeps Contract C's ratified `UnpatchableTemplate` variant for the unrecoverable case. |
| OQ-5 | `OptionTemplate`/`VecTemplate` reflectability | **RESOLVED** by SPEC-1 Phase 5. §6.8 rewritten from "unsupported" to "supported", with the `VecTemplate` tuple-struct unwrap added to §4.6.7. |
| OQ-7 | Prerequisite P1 | **RESOLVED**: SPEC-1 Phase 5, opt-in `#[template(reflect)]` + an annotated seed set. Unannotated components fail at load with `TypeNotRegistered`; §4.4.2's error message names the attribute. |

### 10.2 Still open

**OQ-6 — Int-to-float literal coercion (§4.6.1).** SPEC-4 allows it with an exactness
check, diverging (more permissively) from `bsn!`, which requires `1.0`. If the series
prefers exact macro parity, delete the two float rows from the coercion table; the tests
`int_literal_into_float_exact`/`_inexact_errors` collapse into a single "errors" test.
Low risk either way — it changes only which documents are accepted, never what an accepted
document means.

**OQ-8 — `context.cached` leakage into children (§6.9).** SPEC-4 defensively clears and
restores `ResolveContext::cached` around related-entity resolution, working around a latent
`main` bug that is also reachable from `bsn!` (a cached base root and a local child both
patching the same component ⇒ `resolved_scene.rs:490`'s `unwrap()` panics). Should a
separate upstream PR fix `RelatedScenes::resolve` (`scene.rs:387-396`) and the `SceneList`
impls to clear `context.cached` centrally, rather than each `Scene` impl doing it
defensively? SPEC-4 is correct either way; the question is only whether the workaround can
later be deleted.

**OQ-9 — `SceneEntityReferenceSource::Asset { path_hash: u64 }` collisions (§4.6.8).**
Contract C ratified the hashed form to keep `EntityTemplate: Copy`. Two `.bsn` files whose
paths collide under the chosen 64-bit hash would alias their `#Name` references. Accepted
risk per Contract C; SPEC-4 records it here because SPEC-4 is where the hash is computed.
Should the builder additionally mix the document's root node id into the hash to make a
collision require *both* a path collision and identical document shape?
