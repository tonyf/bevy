# SPEC-2: Erased-API extensions to the scene core

**Status: NORMATIVE.** Conforms to SPEC-0 Contract C **as amended by SPEC-0 §7 (Ratified
amendments)**. Consumes SPEC-1's `ReflectRelationshipTarget` (Contract B, renamed in §7) *by
name only*.

Target: `/home/tony/workspace/bevy`, branch `main`, `0.20.0-dev`.

**Revision 2** — incorporates the review verdict: C2's registry-assisted helper, C4's `Copy`
`SceneEntityReferenceSource`, and `ResolveSceneError::UnpatchableTemplate` are ratified and are
now normative (no longer open questions); two new ratified sections **C6** (`template_type_id`)
and **C7** (`ResolveContext::type_registry` + typed recovery in `get_or_insert_template`) are
added; the SPEC-6 `SceneEntityReferences` freshness invariant is recorded in C4.

---

## 1. Goals

Make the scene core (`bevy_scene`) usable from code that only has runtime type information —
a `TypeId`, a `&TypeRegistration`, a `&dyn PartialReflect` — instead of a compile-time `T`, and
make statically- and dynamically-defined patches on the same component merge correctly **in both
directions**.

| # | Change | Crate | Depends on |
| --- | --- | --- | --- |
| C1 | `ResolvedScene::get_or_insert_erased_template` takes `impl FnOnce` instead of `fn()` | `bevy_scene` | — |
| C2 | `ErasedComponentTemplate::try_as_partial_reflect{,_mut}` (default `None`) + registry-assisted helpers | `bevy_scene` | — |
| C3 | `RelatedResolvedScenes::new_erased` + `ResolvedScene::get_or_insert_related_resolved_scenes_erased` | `bevy_scene` | SPEC-1 (second half only) |
| C4 | `SceneEntityReference` gains an asset-based identity | `bevy_ecs` | — |
| C5 | Eight new `ResolveSceneError` variants | `bevy_scene` | — |
| C6 | `ErasedComponentTemplate::template_type_id` + cached duplicate-skip uses it | `bevy_scene` | C2 (same file) |
| C7 | `ResolveContext::type_registry` + typed recovery in `get_or_insert_template` | `bevy_scene` | C2 |

C1–C6 are separately reviewable and separately mergeable; C7 depends on C2 (it calls
`erased_template_as_partial_reflect`). See §6 for the landing order.

## 2. Non-goals

- No new `Scene` impls, no dynamic template type, no parser, no loader. Those are SPEC-3/4/5.
- No change to `Scene::resolve`'s by-value signature, to the `BundleWriter` apply path, or to
  the copy-on-write caching *rules* (C6/C7 fix bugs in how those rules are executed for
  type-erased templates; they do not change the rules).
- Moving `ErasedComponentTemplate` into `bevy_ecs`, or adding an `erase` fn to
  `ReflectTemplate`, is recorded in SPEC-0 §7 as an upstream-alignment option and is **not** in
  v1.
- No `#[non_exhaustive]` on `ResolveSceneError` (§10, Open Question 1).
- No serialization of `SceneEntityReference` (it is a runtime-only identity, as today).

---

## 3. Background (repo citations, all against current `main`)

- `ResolvedScene` fields, incl. the private `related: TypeIdIndexMap<RelatedResolvedScenes>` and
  `template_indices: TypeIdHashMap<usize>`: `crates/bevy_scene/src/resolved_scene.rs:164-183`.
- `apply_with` — applies the cached scene's templates first (passing
  `&cached.duplicate_templates` as the skip set), then the local scene's:
  `resolved_scene.rs:222-312`. `CachedSceneInfo::duplicate_templates`: `:570-579`.
- `apply_templates_without_bundle_write` — the duplicate skip that C6 fixes:
  `resolved_scene.rs:318-347`, specifically `:325`.
- `get_or_insert_template::<T>` — upcasts to `&mut dyn Any` and `downcast_mut().unwrap()`s
  (the panic C7 removes): `resolved_scene.rs:409-422`.
- `get_or_insert_erased_template` incl. the copy-on-write branch: `resolved_scene.rs:461-498`.
- `get_or_insert_related_resolved_scenes::<R>` — keys the `related` map by
  **`TypeId::of::<R>()` where `R: Relationship`** (i.e. `ChildOf`, *not* `Children`):
  `resolved_scene.rs:537-543`.
- `RelatedResolvedScenes` (all fields `pub`) and `new::<R: Relationship>()`:
  `resolved_scene.rs:650-692`.
- `ErasedComponentTemplate` trait + blanket impl for `T: Template<Output: Component>`:
  `resolved_scene.rs:696-732`.
- `ResolveSceneError`: `crates/bevy_scene/src/scene.rs:155-167`. `ResolveContext`: `scene.rs:169-177`.
  `CachedSceneAsset::resolve` (sets `context.cached`): `scene.rs:422-442`.
- Resolve entry points C7 must plumb: `ScenePatch::resolve` `scene_patch.rs:57-67`,
  `SceneListPatch::resolve` `scene_patch.rs:146-157`, `ResolvedSceneRoot::resolve`
  `resolved_scene.rs:26-43`, `ResolvedSceneListRoot::resolve` `resolved_scene.rs:93-110`,
  `World::spawn_scene`/`spawn_scene_list` `spawn.rs:188-215`,
  `EntityWorldMut::apply_scene` `spawn.rs:503-508`, `resolve_scene_patches` `spawn.rs:607-645`.
- `bsn!` codegen emits `SceneEntityReference::new(#invocation, #index, _call_id)` and
  `EntityTemplate::from_reference(#invocation, #index, _call_id)`, where `#invocation` is the
  tuple literal `(#file, #line, #column)` built at `macros/src/bsn/mod.rs:31-39` from
  `Span::call_site()` (so `(&'static str, usize, usize)`), `#index` is a `usize` name index and
  `_call_id` is a `u64` from `macro_utils::CallCounter::increment()`
  (`macros/src/bsn/codegen.rs:97-101, 291-294, 578-581, 715-719`).
- `SceneEntityReference` / `InnerSceneEntityReference` / `SceneEntityReferences`:
  `crates/bevy_ecs/src/template.rs:92-193`. `EntityTemplate::from_reference`: `template.rs:433-442`.
- `ResolvedSceneRoot::apply` constructs a **fresh** `SceneEntityReferences` per apply:
  `resolved_scene.rs:70`; ditto `ResolvedSceneListRoot::spawn_with` at `:122`.
- `ReflectFromPtr` — registered **automatically** by `#[derive(Reflect)]`
  (`crates/bevy_reflect/derive/src/registration.rs:69`); API at
  `crates/bevy_reflect/src/type_registry.rs:946-1005`.
- `ReflectFromReflect` — also registered automatically by `#[derive(Reflect)]` unless
  `#[reflect(from_reflect = false)]` (`derive/src/registration.rs:30-37`); API
  `fn from_reflect(&self, &dyn PartialReflect) -> Option<Box<dyn Reflect>>`
  (`crates/bevy_reflect/src/from_reflect.rs:106-128`).
- `<dyn Reflect>::downcast::<T>(self: Box<dyn Reflect>) -> Result<Box<T>, Box<dyn Reflect>>`:
  `crates/bevy_reflect/src/reflect.rs:542`.
- `AppTypeRegistry(pub TypeRegistryArc)` with `Deref`: `crates/bevy_ecs/src/reflect/mod.rs:36-45`;
  `TypeRegistryArc::read() -> RwLockReadGuard<'_, TypeRegistry>`
  (`bevy_platform::sync::RwLockReadGuard`): `crates/bevy_reflect/src/type_registry.rs:592-596`.
- `bevy_ecs` re-exports `bevy_ptr` as `bevy_ecs::ptr` (`crates/bevy_ecs/src/lib.rs:59`).
- `bevy_scene` already depends on `bevy_reflect`, already opts into `unsafe_code`
  (`crates/bevy_scene/src/lib.rs:1`), and uses `use tracing::error;` for logging
  (`crates/bevy_scene/src/spawn.rs:11`).
- `bevy_platform::hash::fixed_hash_one(impl Hash) -> u64` — deterministic, fixed-seed
  (`crates/bevy_platform/src/hash.rs:33-36`).
- Test conventions: `mod tests` at `crates/bevy_scene/src/lib.rs:961`, helper `test_app()` at
  `:980`, cached-asset scaffold (memory asset source + `FakeSceneLoader`) at `:1113-1207`.
  `bevy_ecs` template tests: `crates/bevy_ecs/src/template.rs:588-610`.
- Prior art: pcwalton's `ErasedTemplatePatch` / `DefaultDynamicErasedTemplate`,
  `scratchpad/dynamic_bsn.rs:905-1002`. His branch had `get_or_insert_erased_template` take a
  closure and `ErasedTemplate::try_as_partial_reflect_mut`; both are re-derived below against
  `main`'s `ErasedComponentTemplate` + `BundleWriter` API (his `apply` used
  `entity.insert_reflect`, which `main` does not use). He had no equivalent of C6 or C7.

---

## 4. Detailed design

### C1 — `get_or_insert_erased_template` accepts any `FnOnce`

**File:** `crates/bevy_scene/src/resolved_scene.rs`.

```rust
    pub fn get_or_insert_erased_template<'a>(
        &'a mut self,
        context: &mut ResolveContext,
        type_id: TypeId,
-       default: fn() -> Box<dyn ErasedComponentTemplate>,
+       default: impl FnOnce() -> Box<dyn ErasedComponentTemplate>,
    ) -> &'a mut dyn ErasedComponentTemplate {
```

The body is unchanged **in this step** (C7 refactors it into a private index-returning helper).
`default()` is invoked at most once, inside the
`template_indices.entry(type_id).or_insert_with(...)` closure, which is itself `FnOnce`, so
moving an `FnOnce` value into it type-checks. The closure's disjoint captures
(`&mut self.component_templates`, `&mut *context`, `&mut is_cached`) are unaffected.

Add to the doc comment: *"`default` is only called when neither this [`ResolvedScene`] nor its
cached scene already contains a template for `type_id`."*

**Caller audit (exhaustive — `rg 'get_or_insert_erased_template'` over `crates/`, `examples/`,
`benches/`, `tests/` returns exactly these):**

1. `resolved_scene.rs:416` — inside `get_or_insert_template::<T>`, passes
   `|| Box::new(T::default())`. Non-capturing, previously coerced to `fn()`, now matches
   `impl FnOnce` directly. ✔ compiles unchanged.
2. Doc/comment mentions at `resolved_scene.rs:419, 452-460`. ✔ not code.

No other crate, no macro codegen, no example calls it. `bsn!` only emits
`get_or_insert_template::<T>` (`macros/src/bsn/codegen.rs:262, 271`).

**Dyn-compatibility analysis.** `get_or_insert_erased_template` is an *inherent* method on the
concrete struct `ResolvedScene`. `ResolvedScene` is never used behind `dyn`, is never a
supertrait's associated type, and no trait declares this method. Generic parameters on inherent
methods of concrete types impose no object-safety constraint. Nothing in the repo stores
`fn() -> Box<dyn ErasedComponentTemplate>` as a function pointer either. ✔ safe.

**Semver note (migration guide, §7):** with argument-position `impl Trait`, an explicit turbofish
such as `scene.get_or_insert_erased_template::<'a>(..)` no longer compiles. No occurrence exists
in the repo. If a reviewer objects, the identical alternative is a named parameter
`<'a, F: FnOnce() -> Box<dyn ErasedComponentTemplate>>`, which preserves lifetime turbofish;
either spelling satisfies Contract C1.

### C2 — reflecting into an erased template (RATIFIED)

**File:** `crates/bevy_scene/src/resolved_scene.rs`.

#### C2.a Two new provided methods on `ErasedComponentTemplate`

```rust
pub trait ErasedComponentTemplate: Any + Send + Sync {
    // ... existing `apply` and `clone_template` unchanged ...

    /// Returns a [`PartialReflect`] view of the value this template will build from, if this
    /// template stores its data in a type-erased, reflected form.
    ///
    /// This returns `None` for ordinary statically-typed templates (everything covered by the
    /// blanket `impl<T: Template<Output: Component>> ErasedComponentTemplate for T`), because
    /// the blanket impl cannot know whether `T` implements [`PartialReflect`]. Use
    /// [`erased_template_as_partial_reflect`] instead, which additionally recovers a reflected
    /// view of statically-typed templates through the [`TypeRegistry`].
    fn try_as_partial_reflect(&self) -> Option<&dyn PartialReflect> {
        None
    }

    /// The mutable counterpart of [`ErasedComponentTemplate::try_as_partial_reflect`]. This is
    /// how a runtime-constructed patch (e.g. one produced by the `.bsn` asset loader) writes
    /// fields into a template it did not create.
    ///
    /// See [`erased_template_as_partial_reflect_mut`].
    fn try_as_partial_reflect_mut(&mut self) -> Option<&mut dyn PartialReflect> {
        None
    }
}
```

Imports to add at the top of `resolved_scene.rs`:

```rust
use bevy_ecs::ptr::{Ptr, PtrMut};
use bevy_reflect::{PartialReflect, ReflectFromPtr, TypeRegistry};
use core::ptr::NonNull;
```

Both methods take `&self`/`&mut self` and return trait-object references, so
`ErasedComponentTemplate` stays dyn-compatible. Both are *provided*, so the blanket impl at
`resolved_scene.rs:714` — the only impl in the repo — needs no change.

#### C2.b Registry-assisted helpers (free functions, same module)

```rust
/// Returns a [`PartialReflect`] view of an [`ErasedComponentTemplate`], if one can be obtained.
///
/// This first asks the template itself
/// ([`ErasedComponentTemplate::try_as_partial_reflect`]), which is how templates that store an
/// erased `Box<dyn Reflect>` answer. If the template does not answer, and the template's
/// *concrete* Rust type is a registered [`Reflect`](bevy_reflect::Reflect) type, the value is
/// viewed through [`ReflectFromPtr`]. This second path is what makes a statically-defined
/// `bsn!` template (for example the canonical template of a
/// `#[derive(Component, Reflect, Clone, Default)]` component, which is the component type
/// itself) patchable by a runtime-constructed scene.
///
/// Returns `None` if neither path applies — typically a template type generated by
/// `#[derive(FromTemplate)]` without SPEC-1's `#[template(reflect)]`, or a `Reflect` type that
/// was never registered.
pub fn erased_template_as_partial_reflect<'a>(
    template: &'a dyn ErasedComponentTemplate,
    type_registry: &TypeRegistry,
) -> Option<&'a dyn PartialReflect> {
    // See the `_mut` version for why the call is duplicated.
    if template.try_as_partial_reflect().is_some() {
        return template.try_as_partial_reflect();
    }
    let type_id = (*template).type_id();
    let from_ptr = type_registry.get_type_data::<ReflectFromPtr>(type_id)?;
    debug_assert_eq!(from_ptr.type_id(), type_id);
    let ptr = (template as *const dyn ErasedComponentTemplate).cast::<u8>();
    // SAFETY: same argument as the `_mut` version below, with a shared borrow: `ptr` is the data
    // pointer of the `&'a` borrow of `template`, so it is non-null, aligned and valid for reads
    // for `'a`; `type_id` is the concrete type of the pointee and is the type `from_ptr` was
    // created for. `cast_mut` is only needed to build a `NonNull`; the pointer is never written.
    let reflect = unsafe { from_ptr.as_reflect(Ptr::new(NonNull::new_unchecked(ptr.cast_mut()))) };
    Some(reflect.as_partial_reflect())
}

/// The mutable counterpart of [`erased_template_as_partial_reflect`].
pub fn erased_template_as_partial_reflect_mut<'a>(
    template: &'a mut dyn ErasedComponentTemplate,
    type_registry: &TypeRegistry,
) -> Option<&'a mut dyn PartialReflect> {
    // Templates that store their value type-erased know their own reflected view, which is not
    // their concrete Rust type. Ask them first.
    //
    // NOTE: the call is deliberately duplicated: NLL cannot see that the borrow taken by the
    // `if` condition ends before the `return`. Both trait impls are trivial accessors.
    if template.try_as_partial_reflect_mut().is_some() {
        return template.try_as_partial_reflect_mut();
    }

    let type_id = (*template).type_id();
    let from_ptr = type_registry.get_type_data::<ReflectFromPtr>(type_id)?;
    debug_assert_eq!(from_ptr.type_id(), type_id);
    let ptr = (template as *mut dyn ErasedComponentTemplate).cast::<u8>();
    // SAFETY:
    // - `ptr` is the data pointer of the `&'a mut` borrow of `template`, which the cast above
    //   consumed, so it is non-null, well-aligned for the pointee, and uniquely valid for `'a`.
    // - `type_id` is the concrete type of that pointee (`Any::type_id` through the
    //   `ErasedComponentTemplate: Any` supertrait), and `from_ptr` was created for exactly that
    //   type (looked up by `type_id`; re-asserted above), which is `as_reflect_mut`'s contract.
    let reflect = unsafe { from_ptr.as_reflect_mut(PtrMut::new(NonNull::new_unchecked(ptr))) };
    Some(reflect.as_partial_reflect_mut())
}
```

Both are re-exported automatically by `pub use resolved_scene::*;`
(`crates/bevy_scene/src/lib.rs:921`).

#### C2.c Why the blanket impl cannot override the default (RATIFIED as designed)

The blanket impl is

```rust
impl<T: Template<Output: Component> + Send + Sync + 'static> ErasedComponentTemplate for T
```

Returning `Some(self.as_partial_reflect_mut())` inside it requires `T: PartialReflect`, which is
not in the bounds. Three ways to get it, all rejected:

1. **Real specialization** (`#![feature(specialization)]`): nightly-only. Rejected.
2. **Auto-trait pseudo-specialization**, the `Clone + Unpin` trick used at
   `crates/bevy_ecs/src/template.rs:388-406` and `SpecializeFromTemplate` at `:408-416`. That
   trick works by letting a type *opt out* of an auto trait so a blanket impl stops applying to
   it; it needs two non-overlapping impls. Here they would be `impl<T: ... + Unpin>` and
   `impl<T: ... + PartialReflect>`, which overlap for every type that is both — i.e. essentially
   every reflected component — a coherence error. Making reflected templates opt out of `Unpin`
   is impossible too: `impl<T: Clone + Unpin> Template for T` (`template.rs:390`) is precisely
   how those types get their `Template` impl, so opting out deletes the impl being specialized.
3. **Autoref/autoderef specialization** (`(&value).method()` / `(&&value).method()`): resolves
   only where the receiver's concrete type is known at the call site. Inside a generic
   `impl<T: ...>` body `T` is opaque and trait selection uses only `T`'s declared bounds, so the
   fallback branch is always chosen. It does not work in generic context.

**Ratified decision:** trait defaults stay `None`; C2.b's registry-assisted helpers provide the
capability instead. This is strictly better than a hypothetical specialized blanket impl because
it also covers types the blanket impl could not reach, needs no specialization hackery, and adds
no bound to `Template`.

#### C2.d The merge scenario, concretely

`bsn! { Position { x: 1.0 } }` merged with a `.bsn` patch `Position { y: 2.0 }` on one entity:

- `Position` derives `Component, Reflect, Clone, Default`, so `Position::Template == Position`
  and the slot key is `TypeId::of::<Position>()` (SPEC-0 decision 3: key by the *real* template
  `TypeId`).
- The static patch runs first and stores `Box::new(Position { x: 1.0, ..default() })`, typed via
  the blanket impl.
- The dynamic patch calls
  `get_or_insert_erased_template(context, TypeId::of::<Position>(), || …)`; the slot is occupied,
  so the `default` closure is never called and the *typed* template comes back.
- `template.try_as_partial_reflect_mut()` → `None` (blanket default).
- `erased_template_as_partial_reflect_mut(template, registry)` → `Some(&mut dyn PartialReflect)`
  via `ReflectFromPtr`; `try_apply(&DynamicStruct { y: 2.0 })` merges field-wise. ✔
- If the registry lookup also fails, SPEC-4 returns
  `ResolveSceneError::UnpatchableTemplate { type_path }` (C5) — never a panic.

The **reverse** direction (`bsn! { :"a.bsn" Position { x: 1.0 } }`, a typed patch over a dynamic
base) is fixed by **C7**, not here.

#### C2.e Constraint on SPEC-4 (coherence)

SPEC-4's dynamic template implements `ErasedComponentTemplate` *manually*. For that impl not to
conflict with the blanket impl, the dynamic template type must **not** implement `Template` —
i.e. it must not be `Clone + Unpin` (`template.rs:390`). Concretely: do not `#[derive(Clone)]` on
it; expose cloning only through `ErasedComponentTemplate::clone_template`. (SPEC-0 §7 Contract E
states the same.)

### C3 — erased related-scene access

**File:** `crates/bevy_scene/src/resolved_scene.rs`.

> **Naming note:** SPEC-0 §7 renamed Contract B's `ReflectRelationship` to
> **`ReflectRelationshipTarget`** (it is registered on the *target* type, e.g. `Children`). Field
> names are unchanged: `relationship_type_id`, `relationship_target_type_id`,
> `relationship_name`, `insert_relationship`, `insert_relationship_target`.

#### C3.a `RelatedResolvedScenes::new_erased` (no SPEC-1 dependency)

```rust
impl RelatedResolvedScenes {
    /// Creates a new empty [`RelatedResolvedScenes`] from already type-erased relationship
    /// functions, for callers that only have runtime type information.
    ///
    /// The three arguments have exactly the semantics of the same-named fields; the canonical
    /// source of a matching set is
    /// [`ReflectRelationshipTarget`](bevy_ecs::reflect::ReflectRelationshipTarget), whose
    /// function pointers have the same bodies as [`RelatedResolvedScenes::new`]'s.
    ///
    /// The stored function pointers are `unsafe fn` whose contracts are discharged at *call*
    /// time by [`ResolvedScene`]'s apply path, so this constructor itself is safe. They must
    /// nonetheless be a matching pair for a single [`Relationship`] type, or spawning will
    /// produce entities with mismatched relationship / relationship-target components.
    pub fn new_erased(
        insert_relationship: unsafe fn(&mut BundleWriter, &mut ComponentsRegistrator, Entity),
        insert_relationship_target: unsafe fn(&mut BundleWriter, &mut ComponentsRegistrator, usize),
        relationship_name: &'static str,
    ) -> Self {
        Self {
            scenes: Vec::new(),
            insert_relationship,
            insert_relationship_target,
            relationship_name,
        }
    }
}
```

`RelatedResolvedScenes::new::<R>` is rewritten to call `new_erased` with the two existing closure
literals (behavior identical; one definition of the struct layout).

#### C3.b `get_or_insert_related_resolved_scenes_erased` (needs SPEC-1)

```rust
    /// The type-erased counterpart of
    /// [`ResolvedScene::get_or_insert_related_resolved_scenes`], for callers that only have a
    /// [`ReflectRelationshipTarget`] (looked up from the [`TypeRegistry`] on a
    /// [`RelationshipTarget`] type such as `Children`).
    ///
    /// This uses the **same** keying as the generic method —
    /// [`ReflectRelationshipTarget::relationship_type_id`], i.e. `TypeId::of::<ChildOf>()` — so
    /// statically- and dynamically-defined children of the same relationship land in a single
    /// [`RelatedResolvedScenes`] and are spawned as one contiguous group, in the order the
    /// scenes were pushed.
    pub fn get_or_insert_related_resolved_scenes_erased(
        &mut self,
        data: &ReflectRelationshipTarget,
    ) -> &mut RelatedResolvedScenes {
        self.related
            .entry(data.relationship_type_id)
            .or_insert_with(|| {
                RelatedResolvedScenes::new_erased(
                    data.insert_relationship,
                    data.insert_relationship_target,
                    data.relationship_name,
                )
            })
    }
```

Keying verification: the generic method (`resolved_scene.rs:537-543`) does
`self.related.entry(TypeId::of::<R>())` with `R: Relationship`; for `Children [ … ]` the macro
passes `<Children as RelationshipTarget>::Relationship`, i.e. `ChildOf`
(`macros/src/bsn/codegen.rs:283-287`). Contract B defines `relationship_type_id` as
`TypeId::of::<ChildOf>()`. Identical key. ✔ `TypeIdIndexMap` is an `IndexMap`, so
`or_insert_with` preserves the existing entry and the first-insertion order of relationship
groups, which is what `apply_related` (`resolved_scene.rs:349-399`) iterates.

Imports: `use bevy_ecs::reflect::ReflectRelationshipTarget;` (exact path per SPEC-1;
`bevy_ecs::reflect` is gated on `bevy_ecs`'s default-on `bevy_reflect` feature). To be robust
against a downstream `default-features = false`, change `crates/bevy_scene/Cargo.toml`:

```toml
-bevy_ecs = { path = "../bevy_ecs", version = "0.20.0-dev" }
+bevy_ecs = { path = "../bevy_ecs", version = "0.20.0-dev", features = ["bevy_reflect"] }
```

### C4 — asset identity for `SceneEntityReference` (RATIFIED)

**File:** `crates/bevy_ecs/src/template.rs`.

#### Representation

A `Copy` enum discriminating the definition scope, with `name_id`/`runtime` shared:

```rust
/// Identifies the "definition scope" that a [`SceneEntityReference`] belongs to. Two references
/// with different sources are never equal.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum SceneEntityReferenceSource {
    /// The reference was produced by a macro (such as `bsn!`) expanded at a source location.
    CallSite {
        /// The source file of the macro invocation.
        file: &'static str,
        /// The line of the macro invocation.
        line: usize,
        /// The column of the macro invocation.
        column: usize,
    },
    /// The reference was produced by a scene *asset* (such as a `.bsn` file).
    ///
    /// `path_hash` is a deterministic digest of the asset path
    /// ([`SceneEntityReference::asset_path_hash`]). The path string itself is intentionally not
    /// stored, so that [`SceneEntityReference`] stays [`Copy`], allocation-free, and small. Two
    /// distinct asset paths whose digests collide would alias their `#Name` references; at 64
    /// bits this is ~1e-10 for six million distinct scene files, and the digest is never
    /// persisted.
    Asset {
        /// A deterministic digest of the asset path this reference came from.
        path_hash: u64,
    },
}

/// The inner struct actually storing the unique index
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct InnerSceneEntityReference {
    source: SceneEntityReferenceSource,
    name_id: usize,
    runtime: u64,
}
```

#### Constructors (the existing one is byte-for-byte source compatible)

```rust
impl SceneEntityReference {
    /// Create a new [`SceneEntityReference`] from the invocation location, runtime time, and a
    /// local (per-macro) counter for names.
    pub fn new(
        (file, line, column): (&'static str, usize, usize),
        name_id: usize,
        runtime: u64,
    ) -> Self {
        Self::from_source(
            SceneEntityReferenceSource::CallSite { file, line, column },
            name_id,
            runtime,
        )
    }

    /// Create a [`SceneEntityReference`] for a named entity defined in a scene *asset*.
    ///
    /// * `asset_path` — the full path of the asset the scene was loaded from. Callers **must**
    ///   pass the fully-qualified asset path string (including asset source and label, e.g.
    ///   `"embedded://ui/menu.bsn#footer"`), so that identically-named files in different asset
    ///   sources stay distinct.
    /// * `node_id` — an identifier for the node inside that document that is **stable across
    ///   re-parses of an unchanged file** (SPEC-3's `BsnNodeId`, assigned in document order).
    ///
    /// The resulting reference is stable for a given `(asset_path, node_id)` pair, and distinct
    /// from every macro-produced reference.
    pub fn from_asset(asset_path: &str, node_id: u32) -> Self {
        Self::from_asset_hashed(Self::asset_path_hash(asset_path), node_id)
    }

    /// [`SceneEntityReference::from_asset`] for callers that already computed the digest once
    /// (e.g. once per loaded document rather than once per named entity).
    pub fn from_asset_hashed(path_hash: u64, node_id: u32) -> Self {
        Self::from_source(
            SceneEntityReferenceSource::Asset { path_hash },
            node_id as usize,
            0,
        )
    }

    /// The digest used by [`SceneEntityReference::from_asset`].
    ///
    /// Deterministic within and across processes for a given Bevy build, but an implementation
    /// detail: never persist it, never compare it across Bevy versions.
    pub fn asset_path_hash(asset_path: &str) -> u64 {
        bevy_platform::hash::fixed_hash_one(asset_path)
    }

    /// The definition scope this reference belongs to.
    pub fn source(&self) -> SceneEntityReferenceSource {
        self.0.source
    }

    fn from_source(
        source: SceneEntityReferenceSource,
        name_id: usize,
        runtime: u64,
    ) -> Self {
        Self(Hashed::new(InnerSceneEntityReference {
            source,
            name_id,
            runtime,
        }))
    }
}
```

`Display` (fields stay private; `Display` is in the same module and reaches them through the
`SceneEntityReference -> Hashed -> Inner` deref chain):

```rust
impl core::fmt::Display for SceneEntityReference {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.source {
            SceneEntityReferenceSource::CallSite { file, line, column } => {
                f.write_fmt(format_args!(
                    "global={file}:{line}:{column} name_id={} runtime={:?}",
                    self.name_id, self.runtime
                ))
            }
            SceneEntityReferenceSource::Asset { path_hash } => f.write_fmt(format_args!(
                "asset=#{path_hash:016x} node_id={}",
                self.name_id
            )),
        }
    }
}
```

`EntityTemplate` gains the matching constructor (`from_reference` unchanged):

```rust
impl EntityTemplate {
    /// Create an [`EntityTemplate::SceneEntityReference`] pointing at a named entity defined in
    /// a scene asset. See [`SceneEntityReference::from_asset`].
    pub fn from_asset_reference(asset_path: &str, node_id: u32) -> Self {
        Self::SceneEntityReference(SceneEntityReference::from_asset(asset_path, node_id))
    }
}
```

`SceneEntityReferences::get`/`set` (`template.rs:100-129`) are **unchanged** — they only use
`Hashed`'s precomputed hash and `PartialEq`, both derived through the new enum.

#### Soundness invariant shared with SPEC-6 (record in the doc comment)

Asset-based identity is only sound because **a fresh `SceneEntityReferences` map is built for
every apply**: `ResolvedSceneRoot::apply` (`resolved_scene.rs:70`) and
`ResolvedSceneListRoot::spawn_with` (`:122`) both construct `SceneEntityReferences::default()`
per call. A `SceneEntityReference` derived from `(asset_path, node_id)` is *identical* across
every spawn of that scene — unlike a macro reference, whose `runtime` counter differs per `bsn!`
invocation. If the map were ever hoisted, cached, or shared across applies (e.g. as an
optimization in SPEC-6's hot-reload re-apply), the second spawn of a `.bsn` scene would resolve
its `#Name` references to the **first** spawn's entities. Add to `SceneEntityReference`'s doc
comment:

```rust
/// # Invariant
///
/// A [`SceneEntityReferences`] map must never be shared across two applications of a scene.
/// `SceneEntityReference`s produced from a scene *asset* (see
/// [`SceneEntityReference::from_asset`]) are identical for every spawn of that asset, so a
/// shared map would alias entities across spawns. `ResolvedSceneRoot::apply` and
/// `ResolvedSceneListRoot::spawn_with` build a fresh map per call; keep it that way.
```

SPEC-6 pins this with its test #24.

#### Why not `Arc<str>` (ratified over Contract C4's suggested payload)

- `SceneEntityReference` is `Copy` today, and `EntityTemplate` — a **public** type users store in
  `SceneComponent` props structs (`crates/bevy_feathers/src/controls/scrollbar.rs:35`) — is
  `Copy` because of it. Removing `Copy` is a public API break for a feature most users never
  touch.
- `resolved_scene.rs:125, 229, 363` copy references out of `entity_references` on every spawn;
  with `Arc` each becomes an atomic refcount bump per named entity per spawned entity, on the
  spawn hot path.
- `bevy_ecs` cannot depend on `bevy_asset`, so there is no `AssetPath`/`AssetId` to intern
  against; `Arc<str>` would be a bare string with an allocation per document.
- The existing doc comment (`template.rs:132-140`) states the intent is a cheap value that could
  eventually be *hashed at compile time*; a `u64` source digest is directly along that axis.

If exactness is ever required, the drop-in replacement is a process-local path interner behind
`asset_path_hash` — the public API does not change, only that one function body.

#### Backward-compatibility verification against the `bsn!` macro

| Emitted code (codegen.rs) | Status |
| --- | --- |
| `SceneEntityReference::new(#invocation, #index, _call_id,)` `:293` | signature unchanged ✔ |
| `EntityTemplate::from_reference(#invocation, #index, _call_id)` `:580` | signature unchanged ✔ |
| `EntityTemplate::SceneEntityReference(SceneEntityReference::new(#invocation, #index, _call_id))` `:717-718` | signature unchanged ✔ |

`#invocation` is `(#file, #line, #column)` with `file` a `&'static str` literal and
`line`/`column` `usize` (`macros/src/bsn/mod.rs:31-39`), matching `new`'s destructuring parameter
exactly. `InnerSceneEntityReference`'s fields are private, so no downstream code can construct or
read them; changing them is not a breaking change. `SceneEntityReference` keeps
`Copy + Clone + PartialEq + Eq + Hash + Debug + Display + Deref + Equivalent`. Size grows 56 → 64
bytes. Test-only codegen path `codegen.rs:781` (`("", 0, 0)`) is unaffected. **Zero macro
changes.**

#### `NameEntityReference` interaction

`crates/bevy_scene/src/scene.rs:464-489` stores a `SceneEntityReference` verbatim;
`resolve_inline` pushes it onto `ResolvedScene::entity_references` and sets a `Name` template. It
is agnostic to the source variant, so SPEC-4 constructs
`NameEntityReference { name, reference: SceneEntityReference::from_asset(path, node_id) }` and
calls `resolve_inline` with no changes here.

#### Known pre-existing limitation (document, do not fix)

If the *same* cached scene is included by two different entities inside one spawn tree, both
copies carry identical `SceneEntityReference` values, so `SceneEntityReferences::set`
(first-writer-wins, `template.rs:117-129`) maps the second occurrence's `#Name` to the first
occurrence's entity. This is already true for `bsn!`-defined cached scenes today (a `ScenePatch`
is resolved once and its `ResolvedScene` is shared behind an `Arc`); the asset variant behaves
identically. Add a `# Limitations` paragraph saying so. Out of scope to fix.

### C5 — new `ResolveSceneError` variants

**File:** `crates/bevy_scene/src/scene.rs`, appended to the existing enum (`:155-167`).

```rust
    /// Caused when a scene refers to a type that is not present in the [`TypeRegistry`].
    #[error("The type `{type_path}` is not registered in the TypeRegistry. Register it with `app.register_type::<{type_path}>()`.")]
    TypeNotRegistered {
        /// The type path that could not be found.
        type_path: String,
    },
    /// Caused when a registered type cannot be viewed as reflection data (e.g. it has no
    /// `ReflectFromPtr` type data, which every `#[derive(Reflect)]` type registers).
    #[error("The type `{type_path}` cannot be accessed reflectively. It likely does not derive `Reflect`.")]
    TypeNotReflectable {
        /// The type path that is not reflectable.
        type_path: String,
    },
    /// Caused when a template type has to be default-constructed reflectively but has no
    /// `ReflectDefault` type data.
    #[error("The template type `{type_path}` has no `ReflectDefault` type data, so it cannot be created from a scene asset. Add `#[reflect(Default)]` to it.")]
    MissingReflectDefault {
        /// The template type path that is missing `ReflectDefault`.
        type_path: String,
    },
    /// Caused when a type used as a component patch has no `ReflectComponent` type data.
    #[error("The type `{type_path}` has no `ReflectComponent` type data, so it cannot be inserted as a component. Add `#[reflect(Component)]` to it.")]
    MissingReflectComponent {
        /// The type path that is missing `ReflectComponent`.
        type_path: String,
    },
    /// Caused when a scene names a relationship target type that cannot be used for related
    /// scenes (e.g. it has no `ReflectRelationshipTarget` type data).
    #[error("`{type_path}` cannot be used as a relationship in a scene. It must be a `RelationshipTarget` registered with `#[reflect(RelationshipTarget)]`.")]
    UnsupportedRelationship {
        /// The type path of the unsupported relationship target.
        type_path: String,
    },
    /// Caused when reflectively applying patched fields onto a template value fails.
    #[error("Failed to apply a scene patch to `{type_path}`: {error}")]
    ApplyFailed {
        /// The template type path being patched.
        type_path: String,
        /// The underlying reflection error.
        #[source]
        error: ApplyError,
    },
    /// Caused when a runtime-defined patch targets a template that already exists in the
    /// [`ResolvedScene`] but cannot be viewed reflectively (see
    /// [`erased_template_as_partial_reflect_mut`]).
    #[error("The template `{type_path}` already present in this scene cannot be patched reflectively. Its template type must derive `Reflect` and be registered.")]
    UnpatchableTemplate {
        /// The template type path that could not be patched.
        type_path: String,
    },
}
```

Imports for `scene.rs`: `use bevy_reflect::ApplyError;` (and `alloc::string::String` if not
already in scope). `ApplyError` is exactly what `PartialReflect::try_apply` returns and it
implements `core::error::Error`, so `#[source]` is valid.

`UnpatchableTemplate` is **ratified** (SPEC-0 §7) as the 8th variant.

### C6 — `template_type_id` (RATIFIED, new)

**File:** `crates/bevy_scene/src/resolved_scene.rs`.

#### The bug

`ResolvedScene::apply_with` applies the cached scene's templates first, passing
`&cached.duplicate_templates` as the skip set, then applies the local scene's
(`resolved_scene.rs:249-266`). `duplicate_templates` is populated in
`get_or_insert_erased_template` with the **slot key** `type_id` (`:494`) — i.e. the *template*
`TypeId`, e.g. `TypeId::of::<Position>()`. But the skip check compares against the template's
**concrete Rust type**:

```rust
// resolved_scene.rs:318-335, current
    unsafe fn apply_templates_without_bundle_write(
        &self,
        context: &mut TemplateContext,
        bundle_writer: &mut BundleWriter,
        skip_templates: impl SkipTemplate,
    ) -> Result<(), ApplySceneError> {
        for template in &self.component_templates {
            if skip_templates.should_skip((**template).type_id()) {
                continue;
            }
            // SAFETY: bundle_writer is used with the same World across all template.apply calls,
            // and the next bundle_writer.write call
            unsafe {
                template
                    .apply(context, bundle_writer)
                    .map_err(ApplySceneError::TemplateBuildError)?;
            }
        }
```

For every statically-typed template the two coincide (the blanket impl's `Self` *is* the template
type), so the bug is invisible today. For SPEC-4's dynamic template — filed under
`TypeId::of::<Position>()` but whose concrete type is `DynamicComponentTemplate` — they differ,
`should_skip` returns `false`, and the cached base's `Position` is pushed to the `BundleWriter`
**in addition to** the local scene's `Position`: the same `ComponentId` is pushed twice into one
bundle write, which is outside `BundleWriter::write`'s contract
(`crates/bevy_ecs/src/bundle/writer.rs:105-137` simply appends `component_ids`/`component_ptrs`),
and the copy-on-write "the local copy replaces the cached one" guarantee is broken.

#### The fix

Add a provided method to `ErasedComponentTemplate`:

```rust
    /// The [`TypeId`] of the [`Template`] this erased template stands in for — the key under
    /// which it was filed by [`ResolvedScene::insert_erased_template`] /
    /// [`ResolvedScene::get_or_insert_erased_template`].
    ///
    /// This defaults to the implementor's own concrete Rust type, which is correct for every
    /// statically-typed template (the blanket impl's `Self` *is* the template type). Templates
    /// that store their value type-erased — such as a template built by a scene asset loader —
    /// **must** override this to return the template type they represent, or the cached
    /// copy-on-write duplicate check will fail to skip them and the same component will be
    /// written twice.
    fn template_type_id(&self) -> TypeId {
        Any::type_id(self)
    }
```

(`Any::type_id(self)` is spelled explicitly rather than `self.type_id()` to make it obvious that
this is the supertrait's method and not a recursive call.)

And change one line:

```rust
        for template in &self.component_templates {
-           if skip_templates.should_skip((**template).type_id()) {
+           if skip_templates.should_skip(template.template_type_id()) {
                continue;
            }
```

`template` is `&Box<dyn ErasedComponentTemplate>`; `template.template_type_id()` auto-derefs
through the `Box` to the trait object and dispatches dynamically. Behavior for all existing
templates is byte-for-byte identical (`Any::type_id` on the concrete type is exactly what
`(**template).type_id()` returned).

Also add one sentence to `insert_erased_template`'s and `get_or_insert_erased_template`'s docs:
*"For correctness, the stored template's [`ErasedComponentTemplate::template_type_id`] must equal
`type_id`."* (This restates and sharpens the existing "*the `TypeId` of the `Template` returned by
`default` should match the passed in `type_id`*" note.)

### C7 — `ResolveContext::type_registry` and typed recovery (RATIFIED, new)

This is the resolution of the headline interop hazard: a **typed patch over a dynamic base**,
i.e. `bsn! { :"player.bsn" Transform { translation: … } }`. That routes through the typed API
`get_or_insert_template::<TransformTemplate>`, whose copy-on-write branch clones the cached
template and then `downcast_mut::<TransformTemplate>().unwrap()`s it
(`resolved_scene.rs:416-421`). With a dynamic template in the cached slot, that `unwrap()`
**panics**.

C2's `ReflectFromPtr` helper cannot fix it: it yields `&mut dyn PartialReflect`, but the caller
needs an owned, concrete `&mut T`. The fix is a *typed recovery*: convert the occupant's
reflected value back into a real `T` with `ReflectFromReflect`, then replace the slot.

#### C7.a `ResolveContext` gains a `TypeRegistry`

**File:** `crates/bevy_scene/src/scene.rs:169-177`.

```rust
 pub struct ResolveContext<'a> {
     /// The current asset server
     pub assets: &'a AssetServer,
     /// The current [`ScenePatch`] asset collection
     pub patches: &'a Assets<ScenePatch>,
     /// The currently cached [`ScenePatch`], if there is one.
     pub cached: Option<&'a ScenePatch>,
+    /// The app's [`TypeRegistry`], if one was available where this scene's resolution was
+    /// started.
+    ///
+    /// This is `Some` for every resolve driven by a [`World`] that has an
+    /// [`AppTypeRegistry`](bevy_ecs::reflect::AppTypeRegistry) resource — which is all of
+    /// Bevy's own entry points (`World::spawn_scene`, `World::spawn_scene_list`,
+    /// `EntityWorldMut::apply_scene`, and the `resolve_scene_patches` system). It is `None`
+    /// only when a caller drives [`ResolvedSceneRoot::resolve`] by hand without a registry.
+    ///
+    /// Reflection-driven [`Scene`] implementations require it; statically-typed ones ignore it.
+    ///
+    /// A read lock on the app's registry is held for the whole of [`Scene::resolve`]. A
+    /// [`Scene`] implementation must therefore never take a **write** lock on the same
+    /// registry during resolution — that would deadlock.
+    pub type_registry: Option<&'a TypeRegistry>,
 }
```

Import `use bevy_reflect::TypeRegistry;` in `scene.rs`. `Option<&T>` is `Copy`, so nested
resolution (`CachedSceneAsset::resolve`, tuple/list `Scene` impls) propagates it for free —
those impls pass `context` through and never rebuild it.

#### C7.b Threading it through the resolve entry points

Four public methods gain a fourth parameter, `type_registry: Option<&TypeRegistry>`, placed last:

| File:line | Signature after |
| --- | --- |
| `resolved_scene.rs:26` | `ResolvedSceneRoot::resolve(scene: Box<dyn Scene>, assets: &AssetServer, patches: &Assets<ScenePatch>, type_registry: Option<&TypeRegistry>)` |
| `resolved_scene.rs:93` | `ResolvedSceneListRoot::resolve(scene_list: Box<dyn SceneList>, assets, patches, type_registry)` |
| `scene_patch.rs:57` | `ScenePatch::resolve(&mut self, assets, patches, type_registry)` |
| `scene_patch.rs:146` | `SceneListPatch::resolve(&mut self, assets, patches, type_registry)` |

The two `ResolveContext` struct literals (`resolved_scene.rs:33` and `:100` — the **only** two in
the repo) gain `type_registry,`.

All seven call sites (`rg '\.resolve\(&?assets|SceneRoot::resolve|SceneListRoot::resolve'`):

1. `scene_patch.rs:63` — forwards its new parameter to `ResolvedSceneRoot::resolve`.
2. `scene_patch.rs:155` — forwards to `ResolvedSceneListRoot::resolve`.
3. `spawn.rs:191` `World::spawn_scene`
4. `spawn.rs:214` `World::spawn_scene_list`
5. `spawn.rs:506` `EntityWorldMut::apply_scene`
6. `spawn.rs:619` `resolve_scene_patches` → `ResolvedSceneRoot::resolve`
7. `spawn.rs:644` `resolve_scene_patches` → `list_patch.resolve`

Sites 3–5 need a read guard on `AppTypeRegistry` that is dropped before the following
`&mut World` / `&mut EntityWorldMut` use. Add to `spawn.rs`:

```rust
use bevy_ecs::reflect::AppTypeRegistry;
```

and write each as a block that scopes the guard. `World::spawn_scene` becomes:

```rust
    fn spawn_scene<S: Scene>(&mut self, scene: S) -> Result<EntityWorldMut<'_>, SpawnSceneError> {
        let patch = {
            let assets = self.resource::<AssetServer>();
            let mut patch = ScenePatch::load(assets, scene);
            // The read guard is held only for the duration of `resolve`; `patch.spawn` below
            // needs `&mut World`. A `Scene::resolve` impl must not take a write lock on the
            // registry while this guard is alive.
            let type_registry = self.get_resource::<AppTypeRegistry>();
            let type_registry = type_registry.map(|registry| registry.read());
            patch.resolve(
                assets,
                self.resource::<Assets<ScenePatch>>(),
                type_registry.as_deref(),
            )?;
            patch
        };
        patch.spawn(self)
    }
```

(`AppTypeRegistry` derefs to `TypeRegistryArc`, so `registry.read()` yields
`RwLockReadGuard<'_, TypeRegistry>`; `Option::as_deref` turns `Option<Guard>` into
`Option<&TypeRegistry>`.)

`World::spawn_scene_list` (`:210-216`) is identical with `SceneListPatch` / `patch.spawn(self)`.
`EntityWorldMut::apply_scene` (`:503-508`) is identical using
`EntityWorldMut::get_resource::<AppTypeRegistry>()` (`world_mut.rs:684`) and ending in
`patch.apply(self)`.

`resolve_scene_patches` gains a system parameter and takes the guard once for the whole body:

```rust
 pub fn resolve_scene_patches(
     mut events: MessageReader<AssetEvent<ScenePatch>>,
     mut list_events: MessageReader<AssetEvent<SceneListPatch>>,
     assets: Res<AssetServer>,
     mut patches: ResMut<Assets<ScenePatch>>,
     mut list_patches: ResMut<Assets<SceneListPatch>>,
     mut waiting: ResMut<WaitingScenes>,
+    type_registry: Option<Res<AppTypeRegistry>>,
 ) {
+    // Held across every `resolve` below. `Scene::resolve` impls must not write-lock the registry.
+    let type_registry_guard = type_registry.as_ref().map(|registry| registry.read());
+    let type_registry = type_registry_guard.as_deref();
     // … `ResolvedSceneRoot::resolve(scene, &assets, &patches, type_registry)` at :619
     // … `list_patch.resolve(&assets, &patches, type_registry)` at :644
```

`Option<Res<T>>` is a valid system param and yields `None` when the resource is absent, so the
system is still schedulable in a `World` without reflection.

**Migration:** `ResolveContext` has public fields, so external struct-literal construction breaks.
Only two in-repo sites exist, both listed above. Covered by the §7 migration guide.

#### C7.c Refactoring `get_or_insert_erased_template` to expose the slot index

Typed recovery must *replace* the box in the slot, which is impossible while holding the
`&'a mut dyn ErasedComponentTemplate` that `get_or_insert_erased_template` returns (conditional
return of a borrow — NLL rejects it). Split the existing method into an index-returning private
helper plus a thin public wrapper. **The body is moved verbatim; only the return changes.**

```rust
    /// The shared implementation of [`ResolvedScene::get_or_insert_erased_template`], returning
    /// the index into `component_templates` so that callers can re-borrow `self`.
    fn get_or_insert_erased_template_index(
        &mut self,
        context: &mut ResolveContext,
        type_id: TypeId,
        default: impl FnOnce() -> Box<dyn ErasedComponentTemplate>,
    ) -> usize {
        let mut is_cached = false;
        let index = *self.template_indices.entry(type_id).or_insert_with(|| {
            let index = self.component_templates.len();
            let value = if let Some(cached_patch) = &mut context.cached
                && let Some(resolved_cached) = &cached_patch.resolved
                && let Some(cached_template) =
                    resolved_cached.scene.get_direct_erased_template(type_id)
            {
                is_cached = true;
                cached_template.clone_template()
            } else {
                default()
            };
            self.component_templates.push(value);
            index
        });

        if is_cached {
            self.cached
                .as_mut()
                .unwrap()
                .duplicate_templates
                .insert(type_id);
        }

        index
    }

    // …existing doc comment, unchanged…
    pub fn get_or_insert_erased_template<'a>(
        &'a mut self,
        context: &mut ResolveContext,
        type_id: TypeId,
        default: impl FnOnce() -> Box<dyn ErasedComponentTemplate>,
    ) -> &'a mut dyn ErasedComponentTemplate {
        let index = self.get_or_insert_erased_template_index(context, type_id, default);
        // The index was just produced by the call above, so it is in bounds.
        &mut *self.component_templates[index]
    }
```

(The pre-existing `self.cached.as_mut().unwrap()` is retained verbatim; it is infallible because
`is_cached` can only be set when `self.cached.is_some()`.)

#### C7.d `get_or_insert_template` with typed recovery

```rust
    /// This will get the [`Template`], if it already exists in this [`ResolvedScene`]. If it
    /// doesn't exist, it will use [`Default`] to create a new [`Template`].
    ///
    /// … existing paragraphs unchanged …
    ///
    /// If the slot for `T` is occupied by a template of a *different* concrete type — which
    /// happens when a cached scene resolved from a scene asset stored a reflection-driven
    /// template there — this converts that template back into a `T`, preserving its field
    /// values, using [`ReflectFromReflect`] from [`ResolveContext::type_registry`]. Fields the
    /// reflection system cannot see (`#[reflect(ignore)]`) come back as their [`Default`]. If no
    /// registry is available, or `T` cannot be produced from reflection, the slot is reset to
    /// `T::default()` and an error is logged: patched values are lost, but resolution never
    /// panics.
    pub fn get_or_insert_template<
        'a,
        T: Template<Output: Component> + Default + Send + Sync + 'static,
    >(
        &'a mut self,
        context: &mut ResolveContext,
    ) -> &'a mut T {
        let index = self.get_or_insert_erased_template_index(context, TypeId::of::<T>(), || {
            Box::new(T::default())
        });
        let slot = &mut self.component_templates[index];

        if !(&**slot as &dyn Any).is::<T>() {
            // Two statements: `*slot = Box::new(recover(&**slot, ..))` would hold a shared borrow
            // of `*slot` across the assignment and be rejected by the borrow checker.
            let recovered = recover_typed_template::<T>(&**slot, context.type_registry);
            *slot = Box::new(recovered);
        }

        // PERF: this could be unchecked, given that we control what is stored here.
        // Infallible: the branch above replaced any occupant that was not a `T`.
        (&mut **slot as &mut dyn Any).downcast_mut().unwrap()
    }
```

Borrow choreography: `get_or_insert_erased_template_index` takes `&mut self` and returns a plain
`usize`, releasing the borrow. `slot` then reborrows `self.component_templates` for `'a`. The
final `downcast_mut` reborrows `slot` and is returned with `'a`.

The recovery function, placed next to the C2 helpers in `resolved_scene.rs`:

```rust
/// Produces a `T` from a template that occupies `T`'s slot but is not a `T`.
///
/// This happens when a cached scene (typically resolved from a `.bsn` asset) stored a
/// reflection-driven template under `T`'s [`TypeId`], and a statically-typed patch then asks for
/// `&mut T`. Field values from `occupant` are preserved when both a reflected view of it and
/// [`ReflectFromReflect`] for `T` are available; otherwise `T::default()` is returned and an
/// error is logged. A `.bsn` file is user data, so this must never panic.
fn recover_typed_template<T: Default + Send + Sync + 'static>(
    occupant: &dyn ErasedComponentTemplate,
    type_registry: Option<&TypeRegistry>,
) -> T {
    let Some(type_registry) = type_registry else {
        error!(
            "The template slot for `{}` holds a template of a different type, and no TypeRegistry \
             was available to convert it, so its values were reset to `Default`. Resolve this \
             scene through `World::spawn_scene`, `EntityWorldMut::apply_scene` or the \
             `resolve_scene_patches` system, or pass a `TypeRegistry` to \
             `ResolvedSceneRoot::resolve`.",
            core::any::type_name::<T>()
        );
        return T::default();
    };

    let recovered = erased_template_as_partial_reflect(occupant, type_registry)
        .and_then(|reflect| {
            type_registry
                .get_type_data::<ReflectFromReflect>(TypeId::of::<T>())
                .and_then(|from_reflect| from_reflect.from_reflect(reflect))
        })
        .and_then(|value| value.downcast::<T>().ok());

    match recovered {
        Some(value) => *value,
        None => {
            error!(
                "The template slot for `{0}` holds a template of a different type that could not \
                 be converted back to `{0}`, so its values were reset to `Default`. `{0}` must \
                 derive `Reflect` (which registers `ReflectFromReflect`) and be registered in the \
                 TypeRegistry.",
                core::any::type_name::<T>()
            );
            T::default()
        }
    }
}
```

Additional imports for `resolved_scene.rs`: `use bevy_reflect::ReflectFromReflect;` and
`use tracing::error;` (matching `spawn.rs:11`).

`value.downcast::<T>()` is `<dyn Reflect>::downcast` (`reflect.rs:542`), returning
`Result<Box<T>, Box<dyn Reflect>>`; it always succeeds here because `ReflectFromReflect` for `T`
produces a concrete `T`, but the `Result` is handled rather than unwrapped.

`ReflectFromReflect` is registered automatically by `#[derive(Reflect)]`
(`derive/src/registration.rs:30-37`), so for the common `#[derive(Component, Reflect, Clone,
Default)]` component — whose canonical template is the component itself — recovery works with no
extra annotation. SPEC-1 Phase 5's `#[template(reflect)]` gives generated templates the same.

#### C7.e What this buys

`bsn! { :"player.bsn" Transform { translation: Vec3::X } }` where `player.bsn` sets
`Transform { scale: Vec3::splat(2.0) }`:

1. `CachedSceneAsset::resolve` sets `context.cached` to the loaded `player.bsn` `ScenePatch`.
2. The `Transform` patch calls `get_or_insert_template::<Transform>` →
   `get_or_insert_erased_template_index` copy-on-writes the dynamic template out of the cached
   scene and records `TypeId::of::<Transform>()` in `duplicate_templates`.
3. The occupant is not a `Transform`, so `recover_typed_template::<Transform>` runs:
   `try_as_partial_reflect()` on the dynamic template → `&dyn PartialReflect` holding
   `Transform { scale: 2, .. }` → `ReflectFromReflect` → concrete `Transform` → slot replaced.
4. `translation = Vec3::X` is applied on top. Result: `translation: X, scale: 2`. ✔
5. At apply time, C6's `template_type_id` makes the cached scene skip its own `Transform`, so the
   component is written exactly once. ✔

---

## 5. Edge cases & error handling

1. **`default` closure not called on cache hit (C1).** The copy-on-write branch clones the cached
   template *instead of* calling `default`. With an `FnOnce`, a closure capturing an expensive
   value is simply dropped un-called. Tested.
2. **`default` returning the wrong concrete type (C1/C6).** Pre-existing contract, now sharpened
   by C6: the stored template's `template_type_id()` must equal the `type_id` argument. If it
   does not, C7's recovery path rewrites the slot (with an error log) instead of panicking.
3. **Both reflect paths available (C2).** If a template both overrides `try_as_partial_reflect`
   *and* is a registered `Reflect` type, the trait method wins. Documented on the helpers.
   SPEC-4's dynamic template is not `Reflect`, so no ambiguity arises in practice.
4. **Zero-sized templates (C2).** A ZST's data pointer is a dangling-but-aligned non-null
   address, which is exactly what `ReflectFromPtr`'s `deref_mut::<T>` expects and what
   `PtrMut::new` requires. Safe.
5. **Unregistered / non-`Reflect` template (C2).** Helper returns `None` → SPEC-4 emits
   `UnpatchableTemplate`. Never panics.
6. **Mismatched erased relationship pair (C3).** `new_erased` cannot validate that the two
   function pointers belong to one `Relationship`; callers get them from a single
   `ReflectRelationshipTarget`, built monomorphically from one `R`. Documented.
7. **Same relationship inserted via both APIs (C3).** `or_insert_with` keeps the existing entry,
   so the second call reuses the first's function pointers and appends into the same `scenes`
   vector — the required merge behavior.
8. **Asset path with a label (C4).** `AssetPath::to_string()` includes `#label`, so
   `menu.bsn#root` and `menu.bsn#footer` hash differently. SPEC-5 must pass the full path.
9. **`node_id` vs `name_id` numeric overlap (C4).** A macro `name_id` of `3` and an asset
   `node_id` of `3` are distinct because the enum discriminant differs. Tested.
10. **`runtime` for asset references (C4).** Fixed at `0`. The macro's counter disambiguates two
    runtime invocations of one `bsn!` call site; a `.bsn` document is parsed and resolved into a
    `ScenePatch` once and its `ResolvedScene` is shared, so a per-resolve counter would add no
    distinctness. See the pre-existing-limitation note.
11. **`SceneEntityReferences` must stay per-apply (C4).** See the invariant above; SPEC-6 test
    #24 pins it.
12. **`push_template_erased` / `push_bundle_template_erased` and C6.** Pushed templates are not
    filed under a `TypeId` and are never in `duplicate_templates`, so `template_type_id`'s value
    is irrelevant for them; the default (concrete type) is fine.
13. **Registry absent during resolve (C7).** Only reachable when a caller drives
    `ResolvedSceneRoot::resolve` by hand with `None`. Typed recovery degrades to `T::default()` +
    `error!`. Reflection-driven scenes (SPEC-4) additionally return
    `ResolveSceneError::TypeNotRegistered`/`TypeNotReflectable` rather than panicking.
14. **Registry write-lock during resolve (C7).** Would deadlock; forbidden by contract,
    documented at both guard sites and on `ResolveContext::type_registry`.
15. **Recovery loses `#[reflect(ignore)]` state (C7).** `ReflectFromReflect` reconstructs `T` from
    its reflected fields only; ignored fields come back as their `Default`. Inherent to
    reflection-driven scenes; documented on `get_or_insert_template`.
16. **`ScenePlugin`-less worlds (C7).** `Option<Res<AppTypeRegistry>>` and
    `World::get_resource` mean every entry point degrades gracefully to `None`; no new panic path.

---

## 6. Step-by-step implementation plan

Every step compiles and passes `cargo test -p bevy_scene -p bevy_ecs` on its own.
Suggested PR grouping: **PR A** = steps 1–5 (`bevy_scene`), **PR B** = step 6 (`bevy_ecs`),
**PR C** = step 7 (after SPEC-1). PR A and PR B are independent and can go in parallel.

**Step 1 — C1.** In `resolved_scene.rs`, change the `default` parameter of
`get_or_insert_erased_template` to `impl FnOnce() -> Box<dyn ErasedComponentTemplate>` and extend
its doc comment. Verify `cargo check -p bevy_scene --all-features`.

**Step 2 — C5.** In `scene.rs`, add `use bevy_reflect::ApplyError;` and the eight variants.
Nothing constructs them yet; that is fine. Verify `cargo clippy -p bevy_scene -- -D warnings`
(missing-docs is deny-by-default here, so every variant *and every field* needs a doc comment —
they are written above).

**Step 3 — C2.** In `resolved_scene.rs`: add the imports (`Ptr`, `PtrMut`, `NonNull`,
`PartialReflect`, `ReflectFromPtr`, `TypeRegistry`); add the two provided methods to
`ErasedComponentTemplate`; add `erased_template_as_partial_reflect{,_mut}`. Leave the blanket
impl untouched. Every `unsafe` block needs a `SAFETY:` comment
(`clippy::undocumented_unsafe_blocks` is on in CI).

**Step 4 — C6.** In `resolved_scene.rs`: add the `template_type_id` provided method; change
`resolved_scene.rs:325` to `template.template_type_id()`; extend the two
`insert_erased_template` / `get_or_insert_erased_template` doc comments. Behavior-neutral for
all existing templates.

**Step 5 — C7.** Ordered sub-steps, each compiling:
  a. `scene.rs`: add `type_registry: Option<&'a TypeRegistry>` to `ResolveContext` and the
     `use bevy_reflect::TypeRegistry;` import.
  b. `resolved_scene.rs:33, 100`: add `type_registry,` to the two struct literals; add the new
     parameter to `ResolvedSceneRoot::resolve` and `ResolvedSceneListRoot::resolve`.
  c. `scene_patch.rs:57, 146`: add the parameter and forward it.
  d. `spawn.rs`: add `use bevy_ecs::reflect::AppTypeRegistry;`; rewrite sites 3–5 with the
     guard-scoping block; add `type_registry: Option<Res<AppTypeRegistry>>` to
     `resolve_scene_patches` and thread it to sites 6–7.
  e. `resolved_scene.rs`: split `get_or_insert_erased_template` into
     `get_or_insert_erased_template_index` + wrapper (behavior-neutral).
  f. `resolved_scene.rs`: add `recover_typed_template` (+ the `ReflectFromReflect` and
     `tracing::error` imports) and rewrite `get_or_insert_template`.
  Verify after (d) that the whole workspace still builds (`cargo check --workspace`), since
  `ResolveContext` and the four `resolve` functions are public.

**Step 6 — C4.** In `crates/bevy_ecs/src/template.rs`: add `SceneEntityReferenceSource`, change
`InnerSceneEntityReference`'s fields, rewrite `SceneEntityReference::new` in terms of the private
`from_source`, add `from_asset`, `from_asset_hashed`, `asset_path_hash`, `source`, rewrite
`Display`, add `EntityTemplate::from_asset_reference`, and add the `# Invariant` and
`# Limitations` doc paragraphs. Verify `cargo test -p bevy_scene` still passes — the existing
`#Name` tests (`lib.rs:1002, 2364, 2422`) are the regression gate for the macro.

**Step 7 — C3 (C3.a is unblocked; C3.b needs SPEC-1).** Add `RelatedResolvedScenes::new_erased`
and delegate `new::<R>` to it; then add `features = ["bevy_reflect"]` to `bevy_scene`'s
`bevy_ecs` dependency, `use bevy_ecs::reflect::ReflectRelationshipTarget;`, and
`get_or_insert_related_resolved_scenes_erased`.

---

## 7. Migration guide

One file, `_release-content/migration-guides/scene_erased_apis.md` (front-matter per
`_release-content/migration-guides/access_fields_rename.md`):

```markdown
---
title: "Type-erased scene APIs"
pull_requests: [TODO]
---

`ResolveContext` gained a `type_registry: Option<&TypeRegistry>` field, and
`ResolvedSceneRoot::resolve`, `ResolvedSceneListRoot::resolve`, `ScenePatch::resolve` and
`SceneListPatch::resolve` gained a matching trailing parameter. Pass
`world.get_resource::<AppTypeRegistry>().map(|r| r.read())` (keeping the guard alive across the
call, and never write-locking the registry during resolution) if you have a `World`, or `None` if
you do not — reflection-driven scenes such as those loaded from `.bsn` files require it,
statically-defined scenes ignore it.

`ResolvedScene::get_or_insert_erased_template`'s `default` parameter changed from
`fn() -> Box<dyn ErasedComponentTemplate>` to
`impl FnOnce() -> Box<dyn ErasedComponentTemplate>`. Function items, function pointers and
non-capturing closures continue to work unchanged; capturing closures are now accepted too. The
only source-incompatible case is an explicit lifetime turbofish
(`get_or_insert_erased_template::<'a>(..)`), which is no longer permitted.

`ResolveSceneError` gained the variants `TypeNotRegistered`, `TypeNotReflectable`,
`MissingReflectDefault`, `MissingReflectComponent`, `UnsupportedRelationship`, `ApplyFailed`
and `UnpatchableTemplate`. Exhaustive `match`es over it need new arms (or a `_` arm).

`ErasedComponentTemplate` gained three provided methods — `try_as_partial_reflect`,
`try_as_partial_reflect_mut` and `template_type_id` — all with defaults. Existing implementations
need no changes, **unless** your implementation stores its template value type-erased and is
filed under a `TypeId` other than its own concrete type: it must then override
`template_type_id` to return that `TypeId`, or the cached copy-on-write duplicate check will
write its component twice.

`ResolvedScene::get_or_insert_template` no longer panics when the template slot holds a value of
a different concrete type. It now converts that value back to `T` via reflection (preserving
field values) or, failing that, resets the slot to `T::default()` and logs an error.
```

No migration guide is needed for `SceneEntityReference`: `InnerSceneEntityReference`'s fields
were already private and every public constructor kept its signature.

---

## 8. Test plan

### `crates/bevy_scene/src/lib.rs`, inside the existing `mod tests` (`:961`)

Add to the module's `use` list: `core::any::{Any, TypeId}`,
`core::sync::atomic::{AtomicUsize, Ordering}`,
`bevy_reflect::{DynamicTupleStruct, PartialReflect, Reflect, TypeRegistry, Typed}`,
`bevy_ecs::{bundle::BundleWriter, error::BevyError, reflect::AppTypeRegistry,
template::TemplateContext}`, and
`crate::{erased_template_as_partial_reflect_mut, ErasedComponentTemplate, RelatedResolvedScenes,
ResolveContext, ResolveSceneError, ResolvedScene, SceneFunction}`.

**Shared fixtures** (module-level inside `mod tests`):

```rust
    #[derive(Component, Reflect, Clone, Default, Debug, PartialEq)]
    struct Marker(u32);

    #[derive(Component, Reflect, Clone, Default, Debug, PartialEq)]
    struct Position { x: f32, y: f32, z: f32 }

    /// Stands in for SPEC-4's dynamic template: it is filed under `type_id` but its own
    /// concrete type is `FakeDynamicTemplate`. Deliberately not `Clone` (see C2.e).
    struct FakeDynamicTemplate<T> { type_id: TypeId, value: T }

    static FAKE_APPLY_COUNT: AtomicUsize = AtomicUsize::new(0);

    impl<T: Component + Reflect + Clone> ErasedComponentTemplate for FakeDynamicTemplate<T> {
        unsafe fn apply(
            &self,
            context: &mut TemplateContext,
            bundle_writer: &mut BundleWriter,
        ) -> Result<(), BevyError> {
            FAKE_APPLY_COUNT.fetch_add(1, Ordering::Relaxed);
            // SAFETY: world_mut is only used to register components, which does not affect
            // entity location; the caller guarantees bundle_writer belongs to this World.
            let mut components = unsafe { context.entity.world_mut().components_registrator() };
            unsafe { bundle_writer.push_component(&mut components, self.value.clone()) };
            Ok(())
        }
        fn clone_template(&self) -> Box<dyn ErasedComponentTemplate> {
            Box::new(FakeDynamicTemplate { type_id: self.type_id, value: self.value.clone() })
        }
        fn template_type_id(&self) -> TypeId { self.type_id }
        fn try_as_partial_reflect(&self) -> Option<&dyn PartialReflect> { Some(&self.value) }
        fn try_as_partial_reflect_mut(&mut self) -> Option<&mut dyn PartialReflect> {
            Some(&mut self.value)
        }
    }
```

Tests:

1. `erased_template_default_accepts_capturing_closure` — **C1**.
   Capture `let captured = 7u32;` in a `move` closure inside a `SceneFunction` that calls
   `scene.get_or_insert_erased_template(context, TypeId::of::<Marker>(), move || Box::new(Marker(captured)))`.
   `world.spawn_scene(scene)`; assert the spawned entity's `Marker == Marker(7)`.
   *This test does not compile before C1* — that is the point.
2. `erased_template_default_called_at_most_once` — **C1**. In one `SceneFunction`, call
   `get_or_insert_erased_template` for the same `TypeId` twice, each with a closure incrementing
   a `static AtomicUsize`. Assert the counter is `1` after `spawn_scene`, and that the spawned
   component holds the *first* closure's value.
3. `typed_template_is_not_directly_reflectable` — **C2.a**.
   `let mut t: Box<dyn ErasedComponentTemplate> = Box::new(Marker(1));`
   assert both `try_as_partial_reflect()` and `try_as_partial_reflect_mut()` are `None`.
4. `registry_assisted_reflect_view_of_typed_template` — **C2.b**, the dynamic-over-static merge.
   `let mut registry = TypeRegistry::new(); registry.register::<Marker>();`
   `let mut t: Box<dyn ErasedComponentTemplate> = Box::new(Marker(1));`
   `let reflect = erased_template_as_partial_reflect_mut(&mut *t, &registry).unwrap();`
   `let mut patch = DynamicTupleStruct::default(); patch.insert(5u32);
    patch.set_represented_type(Some(Marker::type_info()));`
   `reflect.try_apply(&patch).unwrap();`
   assert `(&*t as &dyn Any).downcast_ref::<Marker>().unwrap() == &Marker(5)`.
5. `unregistered_template_has_no_reflect_view` — **C2.b**. Same as (4) with an empty
   `TypeRegistry`; assert `erased_template_as_partial_reflect_mut` returns `None`.
6. `template_type_id_defaults_to_concrete_type` — **C6**.
   `Marker(1).template_type_id() == TypeId::of::<Marker>()`; and for
   `let fake = FakeDynamicTemplate { type_id: TypeId::of::<Marker>(), value: Marker(1) };`
   assert `fake.template_type_id() == TypeId::of::<Marker>()` while
   `(&fake as &dyn Any).type_id() != TypeId::of::<Marker>()`.
7. `cached_dynamic_template_is_not_applied_twice` — **C6**, the real path.
   Copy the memory-asset-source + `FakeSceneLoader` scaffold from `loaded_asset_cached_patching`
   (`lib.rs:1113-1152`), with the loader returning
   `ScenePatch::load_with(load_context, base())` where

   ```rust
   fn base() -> impl Scene {
       SceneFunction(|_ctx: &mut ResolveContext, scene: &mut ResolvedScene| {
           scene.insert_erased_template(
               TypeId::of::<Marker>(),
               Box::new(FakeDynamicTemplate { type_id: TypeId::of::<Marker>(), value: Marker(1) }),
           );
       })
   }
   ```

   Then `FAKE_APPLY_COUNT.store(0, Ordering::Relaxed);` and spawn a scene equivalent to
   `bsn! { :"a.bsn" }` followed by a `SceneFunction` that calls
   `scene.get_or_insert_erased_template(ctx, TypeId::of::<Marker>(), || Box::new(Marker(9)))`
   and overwrites it with `insert_erased_template(TypeId::of::<Marker>(), Box::new(Marker(9)))`.
   Assert `FAKE_APPLY_COUNT.load(Ordering::Relaxed) == 0` (the cached copy was skipped) and the
   entity's `Marker == Marker(9)`. Before C6 the counter is `1` and the same `ComponentId` is
   pushed twice into one `BundleWriter` write.
8. `typed_patch_recovers_values_from_dynamic_base` — **C7**, the headline case. No loader needed:

   ```rust
   let mut app = test_app();
   app.world_mut().resource_mut::<AppTypeRegistry>().write().register::<Position>();
   let world = app.world();
   let registry = world.resource::<AppTypeRegistry>().read();
   let mut context = ResolveContext {
       assets: world.resource::<AssetServer>(),
       patches: world.resource::<Assets<ScenePatch>>(),
       cached: None,
       type_registry: Some(&registry),
   };
   let mut scene = ResolvedScene::default();
   scene.insert_erased_template(
       TypeId::of::<Position>(),
       Box::new(FakeDynamicTemplate {
           type_id: TypeId::of::<Position>(),
           value: Position { x: 0., y: 2., z: 0. },
       }),
   );
   let position = scene.get_or_insert_template::<Position>(&mut context);
   position.x = 1.;
   assert_eq!(*position, Position { x: 1., y: 2., z: 0. });
   ```

   `y` surviving is the whole point: the dynamic base's value was recovered, not discarded.
   Before C7 this test **panics** at `downcast_mut().unwrap()`.
9. `typed_patch_over_dynamic_base_falls_back_to_default_when_unregistered` — **C7 fallback**.
   As (8) but with a `TypeRegistry` that does *not* contain `Position`; assert the result is
   `Position { x: 1., y: 0., z: 0. }` and that no panic occurs.
10. `typed_patch_over_dynamic_base_with_no_registry` — **C7 registry-`None` path**.
    As (8) with `type_registry: None`; assert `Position { x: 1., y: 0., z: 0. }`.
11. `related_resolved_scenes_new_erased_matches_new` — **C3.a**.
    `let a = RelatedResolvedScenes::new::<ChildOf>();`
    `let b = RelatedResolvedScenes::new_erased(a.insert_relationship, a.insert_relationship_target, a.relationship_name);`
    assert `b.scenes.is_empty()`, `b.relationship_name == a.relationship_name`, and that the
    `insert_relationship` pointers compare equal via `core::ptr::fn_addr_eq`.
12. `resolve_scene_error_messages_name_the_type` — **C5**. For each of the eight new variants,
    `format!("{err}")` must contain `"my_crate::Foo"`; for `MissingReflectDefault` it must also
    contain `"#[reflect(Default)]"`.
13. `dynamic_and_static_children_merge_into_one_relationship` — **C3.b, requires SPEC-1**.
    Register `Children` so it carries `ReflectRelationshipTarget`. In one `SceneFunction`: push a
    `ResolvedScene` (with a `Name` template `"static"`) via
    `get_or_insert_related_resolved_scenes::<ChildOf>()`, then a second (`"dynamic"`) via
    `get_or_insert_related_resolved_scenes_erased(...)`. After `spawn_scene`, assert the root has
    exactly one `Children` with `len() == 2` and child names `["static", "dynamic"]`.
14. `erased_children_then_static_children_also_merge` — **C3.b**. As (13) with the calls swapped;
    assert `len() == 2`, order `["dynamic", "static"]`.

### `crates/bevy_ecs/src/template.rs`, inside the existing `mod tests` (`:588`)

 1. `asset_scene_entity_reference_is_stable` — **C4**. `from_asset("a.bsn", 3)` equals itself
    across two constructions, and their `.hash()` values (via `Deref` to `Hashed`) are equal.
 2. `asset_scene_entity_references_differ_by_path_and_node` — **C4**.
    `from_asset("a.bsn", 3) != from_asset("b.bsn", 3)`; `from_asset("a.bsn", 3) != from_asset("a.bsn", 4)`.
 3. `asset_and_call_site_references_never_collide` — **C4**.
    `from_asset("x", 0) != SceneEntityReference::new(("x", 0, 0), 0, 0)`.
 4. `scene_entity_references_map_resolves_asset_references` — **C4**. With a fresh
    `SceneEntityReferences` and `World`: `get(from_asset("a.bsn", 1))` twice yields the same
    `Entity`; `get(from_asset("a.bsn", 2))` yields a different one; both are alive.
 5. `fresh_reference_map_yields_fresh_entities` — **C4 invariant**. Two *separate*
    `SceneEntityReferences::default()` maps queried with the *same* `from_asset("a.bsn", 1)` must
    yield **different** entities. This is the invariant SPEC-6 must not break.
 6. `entity_template_from_asset_reference` — **C4**. `EntityTemplate::from_asset_reference("a.bsn", 1)`
    matches `EntityTemplate::SceneEntityReference(SceneEntityReference::from_asset("a.bsn", 1))`,
    and `clone_template()` round-trips it.
 7. `call_site_scene_entity_reference_unchanged` — **C4** regression.
    `new(("f.rs", 1, 2), 3, 4) == new(("f.rs", 1, 2), 3, 4)` and `!= new(("f.rs", 1, 2), 3, 5)`;
    `format!("{r}").starts_with("global=f.rs:1:2")`.

### Regression coverage (no new code)

The existing `bsn!` `#Name` tests (`lib.rs:1002`, `:2364`, `:2422`) gate C4's macro
compatibility; `cached_patching` (`:1002`), `cached_patching_order` (`:1049`) and
`loaded_asset_cached_patching` (`:1083`) gate C6/C7's copy-on-write behavior for purely typed
templates. All must pass unmodified.

Commands: `cargo test -p bevy_ecs --lib template`, `cargo test -p bevy_scene`,
`cargo check --workspace`,
`cargo clippy -p bevy_ecs -p bevy_scene --all-targets -- -D warnings`,
`cargo doc -p bevy_scene --no-deps` (intra-doc links must resolve).

---

## 9. Acceptance criteria

1. `cargo check --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` pass
   after **each** of the seven steps.
2. `get_or_insert_erased_template` accepts a capturing closure (test 1) and calls it at most once
   (test 2).
3. `ErasedComponentTemplate` remains dyn-compatible: `Box<dyn ErasedComponentTemplate>` still
   compiles everywhere it does today (`scene.rs:354`, `resolved_scene.rs:167, 436, 518`).
4. `erased_template_as_partial_reflect_mut` returns `Some` for a registered `Reflect` template
   stored through the blanket impl, and `try_apply` on it merges field-wise (test 4).
5. A template filed under a `TypeId` other than its own concrete type is skipped by the cached
   duplicate check and applied exactly once overall (tests 6, 7).
6. `get_or_insert_template::<T>` never panics for a non-`T` occupant: it recovers field values
   when a registry and `ReflectFromReflect` are available (test 8) and resets with an `error!`
   otherwise (tests 9, 10).
7. `ResolveContext::type_registry` is `Some` for every scene resolved through `World::spawn_scene`,
   `World::spawn_scene_list`, `EntityWorldMut::apply_scene` and `resolve_scene_patches` in an app
   with an `AppTypeRegistry`, and the guard is released before any `&mut World` use.
8. `get_or_insert_related_resolved_scenes_erased(data)` and
   `get_or_insert_related_resolved_scenes::<R>()` with
   `data.relationship_type_id == TypeId::of::<R>()` return the **same** `RelatedResolvedScenes`
   (tests 13, 14).
9. `SceneEntityReference::from_asset` is deterministic, distinct per path and per node, and never
   equal to a `::new` reference (tests 15–17); two separate reference maps never alias (test 19).
10. The `bsn!` macro crate is not modified at all, and the pre-existing `#Name` and
    cached-patching tests pass unmodified.
11. `ResolveSceneError` has all eight new variants with `thiserror` messages that name the
    offending type path and, where applicable, the attribute to add.
12. Every new public item has a doc comment; every new `unsafe` block has a `SAFETY:` comment.

---

## 10. Open questions

1. **`#[non_exhaustive]` on `ResolveSceneError`.** Not applied (no in-repo exhaustive matches
   exist, and it would force `_` arms downstream). It would make SPEC-4/5's future variant
   additions non-breaking. Deferred to reviewer preference.
2. **`ResolveContext`'s growing parameter list.** C7 adds a fourth positional parameter to four
   public `resolve` functions. If reviewers prefer, these could take a
   `ResolveOptions { assets, patches, type_registry }` struct instead; that is a strictly larger
   diff and was not adopted. Flagging only because it is a public-API shape decision.
3. **Naming.** `erased_template_as_partial_reflect{,_mut}` are free functions because they need a
   `&TypeRegistry` that a trait-method signature cannot carry. An alternative is an inherent
   method on a thin `TemplateReflector<'_>(&TypeRegistry)` wrapper. Free functions chosen for
   brevity.
4. **Recovery of `#[reflect(ignore)]` fields (C7).** Values on ignored fields in a dynamic base
   are lost during typed recovery (they come back as `Default`). Documented, not fixed —
   reflection cannot see them. Worth confirming no in-repo component depends on this.
5. **Registry lock held across resolve (C7).** The read guard spans the whole of
   `Scene::resolve`. This is safe today, but it is a new global invariant on `Scene`
   implementations ("never write-lock the `AppTypeRegistry` during resolve"). An alternative is to
   clone the `TypeRegistryArc` into `ResolveContext` and lock lazily per lookup, at the cost of a
   lock acquisition per reflected field. Not adopted; flagged for reviewer input.
