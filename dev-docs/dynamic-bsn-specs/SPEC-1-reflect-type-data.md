# SPEC-1: Reflection type data for Templates & Relationships in `bevy_ecs`

**Status: ACCEPTED (review pass 1) — conforms to SPEC-0 Contracts A and B *as amended by
SPEC-0 §7*.**
**Owner crate: `bevy_ecs` (plus two-line touches in `bevy_asset`).**
**Depends on: nothing. Consumed by: SPEC-2 (Contract B), SPEC-4 (Contracts A + B).**

Target: `/home/tony/workspace/bevy`, branch `main`, `0.20.0-dev`.

**Amendments folded in from SPEC-0 §7 (all ratified, all normative here):**

- `ReflectTemplate` gains `output_type_id: TypeId` (SPEC-0 §7 A-1) — §4.4.
- `ReflectFromTemplate` gains `template_type_path: &'static str` — §4.4.
- `ReflectRelationship` is **renamed `ReflectRelationshipTarget`**; the attribute ident is
  therefore `#[reflect(RelationshipTarget)]` — §4.5, §4.7, §4.8.1.
- The strengthened bound `T::Template: Reflect` stands — §4.4.1.
- **Phase 5 is a ratified series prerequisite for SPEC-4**, and additionally absorbs SPEC-4's
  P3 (`EntityTemplate: Reflect`, `SceneEntityReference` as `#[reflect(opaque)]`) — §5 Phase 5.
- Phase 5's generated templates must keep auto-derived `FromReflect`, which SPEC-2's C-7 typed
  recovery depends on — §4.9.2.

---

## 1. Goals

1. Add `bevy_ecs::reflect::template` with `ReflectFromTemplate` and `ReflectTemplate` exactly as
   specified in SPEC-0 Contract A, ported from `FromType` (pcwalton's draft, scratchpad
   `reflect_template.rs`) to the current `CreateTypeData` trait.
2. Add `bevy_ecs::reflect::relationship` with `ReflectRelationshipTarget` exactly as specified in
   SPEC-0 Contract B, with function-pointer bodies lifted from
   `RelatedResolvedScenes::new::<R>()` (`crates/bevy_scene/src/resolved_scene.rs:670-692`).
3. Extend `ReflectComponentFns` with `push_to_bundle_writer` (Contract A), the erased insertion
   path that preserves `ResolvedScene`'s single-archetype-move apply.
4. Add a **non-panicking** `try_from_reflect_with_fallback` + `FromReflectError` next to the
   existing panicking `from_reflect_with_fallback`, because SPEC-0 decision-log item 6 requires
   "errors, never panics" in the dynamic path.
5. Register the new type data on the in-repo types that can carry it today: `Children`
   (`#[reflect(RelationshipTarget)]`), `Handle<A>` / `HandleTemplate<A>` (runtime registration
   in `bevy_asset`).
6. **(Phase 5, ratified prerequisite for SPEC-4)** Make derive-generated `*Template` types
   reflectable via an opt-in `#[template(reflect)]` container attribute, and make
   `OptionTemplate`, `VecTemplate` and `EntityTemplate` `Reflect`. Without this, Contract A's
   `ReflectFromTemplate` is inert for every `#[derive(FromTemplate)]` component in the repo
   (§4.9.1).

This spec produces two standalone, upstreamable PRs (Steps 1-10, then Phase 5). It adds no
dependency on `bevy_scene` and does not change `bevy_scene`.

## 2. Non-goals

- No changes to `bevy_scene` (`ResolvedScene`, `RelatedResolvedScenes`, `ErasedComponentTemplate`).
  SPEC-2 rewires `RelatedResolvedScenes` construction to consume `ReflectRelationshipTarget`; SPEC-1
  only *provides* the data. `RelatedResolvedScenes::new::<R>()` stays as-is.
- No parser, no loader, no `DynamicScene` (SPEC-3/4/5).
- No `SceneEntityReference` **identity** changes: Contract C item 4 / SPEC-0 §7's
  `SceneEntityReferenceSource` enum belongs to SPEC-2. Phase 5 only makes the *existing*
  `SceneEntityReference` opaque-reflectable so `EntityTemplate` can derive `Reflect`; the two
  changes are independent and compose (an opaque type's internals are irrelevant to reflection).
- No reflection for `HasWindows`/`OnMonitor` in `bevy_window` (they are not `Reflect` at all
  today; out of scope, Open Question 3).
- No new `.bsn`-facing public API.

## 3. Background (all citations are `file:line` in this repo)

### 3.1 The type-data mechanism

- `CreateTypeData` trait: `crates/bevy_reflect/src/type_data.rs:132-144`.

  ```rust
  pub trait CreateTypeData<T, Input = ()>: TypeData {
      fn create_type_data(input: Input) -> Self;
      fn insert_dependencies(type_registration: &mut TypeRegistration) {}
  }
  ```

  `FromType` **no longer exists** anywhere in `crates/` (migration guide:
  `_release-content/migration-guides/bevy_reflect_parameterized_type_data.md`). pcwalton's
  `impl<T> FromType<T> for ReflectX { fn from_type() -> Self }` becomes
  `impl<T> CreateTypeData<T> for ReflectX { fn create_type_data(_input: ()) -> Self }`.
- `TypeData` blanket impl: `crates/bevy_reflect/src/type_data.rs:26-33` — any
  `Clone + Send + Sync + 'static`. **Every `ReflectX` in this spec must derive `Clone`.**
- `#[reflect(Foo)]` parsing: `crates/bevy_reflect/derive/src/container_attributes.rs:222-257`
  dispatches any non-keyword ident to `parse_type_data`
  (`container_attributes.rs:264-281`). Naming rule: `crates/bevy_reflect/derive/src/ident.rs:18-38`
  — the **last path segment** is prefixed with `Reflect` unless it already starts with `Reflect`.
  Full paths work: `#[reflect(a::b::Foo)]` → `a::b::ReflectFoo` (test:
  `crates/bevy_reflect/src/lib.rs:4053-4088`).
- Generated code: `crates/bevy_reflect/derive/src/registration.rs:47-79` emits
  `registration.register_type_data_with::<#reflect_path, Self, _>(());` — which is
  `insert(...)` **plus** `insert_dependencies(...)`
  (`crates/bevy_reflect/src/type_registry.rs:695-709`).
- Reserved idents that will never become type data: `Clone`, `Debug`, `Hash`, `PartialEq`,
  `PartialOrd`, `from_reflect`, `type_path`, `opaque`, `no_field_bounds`, `no_auto_register`
  (`container_attributes.rs:21-32`). `FromTemplate`, `Template`, `Relationship` are all free.
- **No changes to `bevy_reflect_derive` are required by this spec.** Adding a `kw::` entry would
  actively break the generic path.
- Auto-registration (`reflect_auto_register`) needs nothing extra: it only carries
  `fn(&mut TypeRegistry)` that calls `T::get_type_registration()`
  (`crates/bevy_reflect/src/lib.rs:742-746`). Note generic types are *never* auto-registered
  (`crates/bevy_reflect/derive/src/impls/common.rs:183-185`) — this is why `Handle<A>` /
  `HandleTemplate<A>` use runtime `register_type_data` in `bevy_asset` (§4.8.2).
- `insert_dependencies` precedent: `crates/bevy_ecs/src/reflect/resource.rs:33-41`
  (`ReflectResource` pulls in `ReflectComponent`).

### 3.2 `ReflectComponent` and its codegen-cost tradeoff

- `ReflectComponent(ReflectComponentFns)`: `crates/bevy_ecs/src/reflect/component.rs:80-138`.
- `impl<C: Component + Reflect + TypePath> CreateTypeData<C> for ReflectComponent`:
  `component.rs:309-411`.
- The documented cost, `component.rs:45-54`:
  > Adding `N` fields on `ReflectComponentFns` will generate `N × M` additional functions, where
  > `M` is how many types derive `#[reflect(Component)]`.
  This spec adds exactly **one** field (§4.10 quantifies it).
- Mutability is handled per-field: `apply`, `reflect_mut`, `reflect_unchecked_mut` panic for
  immutable components (`component.rs:320-330, 375-401`); `insert` /
  `apply_or_insert_mapped`'s insert branch do not care. `push_to_bundle_writer` is an
  *insert*-shaped operation, so **it is mutability-agnostic** — see §4.4.3.

### 3.3 `from_reflect_with_fallback`

`crates/bevy_ecs/src/reflect/mod.rs:108-164`. Ladder: `ReflectFromReflect` → `ReflectDefault` +
`apply` → `ReflectFromWorld` + `apply`. It **panics** on failure (`mod.rs:140-144`) and on a
type mismatch (`mod.rs:146-151`), and it takes `&mut World`. Neither is acceptable for the
dynamic path (SPEC-0 decision 6), and `&mut World` is unavailable while a
`ComponentsRegistrator` is alive.

### 3.4 Templates

- `Template` / `FromTemplate`: `crates/bevy_ecs/src/template.rs:32-41`, `348-351`.
- Blanket impls: `template.rs:390-400` (`T: Clone + Unpin` ⇒ `Template<Output = T>`) and
  `template.rs:404-406` (`T: Clone + Default + Unpin` ⇒ `FromTemplate<Template = Self>`).
  Because `FromTemplate: Template<Output = Self>`, the projection
  `<C::Template as Template>::Output` **normalizes to `C`** for every `C: FromTemplate`.
- `TemplateContext<'a, 'w>`: `template.rs:45-90`.
- The `FromTemplate` derive: `crates/bevy_ecs/macros/src/template.rs`. Naming
  `format_ident!("{type_ident}Template")` (line 20); visibility mirrors the source type
  (line 22); the generated struct/enum carries **no derives at all** — only `#[allow(missing_docs)]`
  plus hand-written `impl Template` (lines 45/76/105/283) and `impl Default`
  (lines 60/91/116). Consequence: **no derive-generated `*Template` type in the repo is
  `Reflect`.** The only reflectable template type today is the hand-written
  `HandleTemplate<T>` (`crates/bevy_asset/src/handle.rs:269-281`).

### 3.5 Relationships

- `Relationship`: `crates/bevy_ecs/src/relationship/mod.rs:111-133` (`type RelationshipTarget`,
  `fn from(entity) -> Self`).
- `RelationshipTarget`: `relationship/mod.rs:270-303` (`Component<Mutability = Mutable>`,
  `type Relationship`, `fn with_capacity(usize) -> Self` at `:341`).
- `RelatedResolvedScenes` and the exact fn-pointer bodies to lift:
  `crates/bevy_scene/src/resolved_scene.rs:650-692`.
- `BundleWriter::push_component` / `push_component_by_id`:
  `crates/bevy_ecs/src/bundle/writer.rs:92-125` — both `unsafe`, requiring that `components`
  comes from the same `World` as every other push and the following `write`.
- Every `RelationshipTarget` in the repo comes from the `#[relationship_target(...)]` derive
  (parsed `crates/bevy_ecs/macro_logic/src/component.rs:151`); there are **no** manual
  `impl RelationshipTarget for` blocks. Production targets: `Children`
  (`crates/bevy_ecs/src/hierarchy.rs:148-152`) and `HasWindows`
  (`crates/bevy_window/src/monitor.rs:60-61`, not `Reflect`).

---

## 4. Detailed design

### 4.1 Module layout

| Path | Action |
| --- | --- |
| `crates/bevy_ecs/src/reflect/mod.rs` | modify: add `mod relationship; mod template;`, re-exports, `FromReflectError`, `try_from_reflect_with_fallback`, refactor `from_reflect_with_fallback` |
| `crates/bevy_ecs/src/reflect/template.rs` | **new** |
| `crates/bevy_ecs/src/reflect/relationship.rs` | **new** |
| `crates/bevy_ecs/src/reflect/component.rs` | modify: one new `ReflectComponentFns` field + method + doc |
| `crates/bevy_ecs/src/lib.rs` | modify: prelude re-exports (`lib.rs:113-117`) |
| `crates/bevy_ecs/src/hierarchy.rs` | modify: `Children` reflect attribute (`hierarchy.rs:150`) |
| `crates/bevy_ecs/macros/src/template.rs` | modify (Phase 5 only): `#[template(reflect)]` |
| `crates/bevy_asset/src/lib.rs` | modify: 2 lines in `register_asset_reflect` (`lib.rs:690-708`) |

Everything lives under `#[cfg(feature = "bevy_reflect")]` implicitly: `pub mod reflect` is
already gated at `crates/bevy_ecs/src/lib.rs:47-48`. No new Cargo features, no new
dependencies (`thiserror` is already a hard dependency: `crates/bevy_ecs/Cargo.toml:109`).

> **Note for the implementer:** do **not** use `derive_more::{Deref, DerefMut}` the way
> pcwalton's draft does. `bevy_ecs` enables only the `from, display, into, as_ref` features of
> `derive_more` (`Cargo.toml:110-115`), and Contract A specifies flat structs anyway. Drop the
> `ReflectFromTemplateData` / `ReflectTemplateData` inner structs entirely.

### 4.2 `FromReflectError` and `try_from_reflect_with_fallback`

**File: `crates/bevy_ecs/src/reflect/mod.rs`.** Replace lines 108-164 with the following
(imports to add at the top of the file: `alloc::boxed::Box`, `bevy_reflect::ApplyError`,
`thiserror::Error`).

```rust
/// Errors returned by [`try_from_reflect_with_fallback`].
#[derive(Error, Debug)]
pub enum FromReflectError {
    /// The target type is not present in the [`TypeRegistry`] at all.
    #[error("The type `{type_name}` is not registered in the `TypeRegistry`")]
    NotRegistered {
        /// The name of the type that could not be constructed.
        type_name: DebugName,
    },
    /// The target type is registered, but carries neither the type data needed to construct a
    /// value nor a way to fall back to a default.
    #[error(
        "Couldn't create an instance of `{type_name}` using the reflected {traits}. \
         Are you perhaps missing a `#[reflect(Default)]` attribute?"
    )]
    MissingConstructor {
        /// The name of the type that could not be constructed.
        type_name: DebugName,
        /// A human-readable list of the traits that were tried.
        traits: &'static str,
    },
    /// A default value was produced, but applying the reflected value on top of it failed.
    #[error("Failed to apply a reflected value onto a default instance of `{type_name}`: {error}")]
    ApplyFailed {
        /// The name of the type being constructed.
        type_name: DebugName,
        /// The underlying apply error.
        #[source]
        error: ApplyError,
    },
    /// The registered type data produced a value of the wrong concrete type.
    #[error(
        "The registration for the reflected `{source_trait}` trait for the type `{type_name}` \
         produced a value of a different type"
    )]
    MismatchedType {
        /// The name of the expected type.
        type_name: DebugName,
        /// Which reflected trait produced the wrong value.
        source_trait: &'static str,
    },
}

/// The shared, type-erased implementation behind [`from_reflect_with_fallback`] and
/// [`try_from_reflect_with_fallback`].
///
/// Strategies, in order: reflected `FromReflect`; reflected `Default` + `try_apply`; reflected
/// `FromWorld` + `try_apply` (only when `world` is `Some`).
#[inline(never)]
fn from_reflect_erased(
    reflected: &dyn PartialReflect,
    world: Option<&mut World>,
    registry: &TypeRegistry,
    type_id: TypeId,
    type_name: DebugName,
) -> Result<alloc::boxed::Box<dyn Reflect>, FromReflectError> {
    if registry.get(type_id).is_none() {
        return Err(FromReflectError::NotRegistered { type_name });
    }

    let (mut value, source_trait) = if let Some(value) = registry
        .get_type_data::<ReflectFromReflect>(type_id)
        .and_then(|reflect_from_reflect| reflect_from_reflect.from_reflect(reflected))
    {
        (value, "FromReflect")
    } else if let Some(reflect_default) = registry.get_type_data::<ReflectDefault>(type_id) {
        (reflect_default.default(), "Default")
    } else if let Some((reflect_from_world, world)) = registry
        .get_type_data::<ReflectFromWorld>(type_id)
        .zip(world)
    {
        (reflect_from_world.from_world(world), "FromWorld")
    } else {
        return Err(FromReflectError::MissingConstructor {
            type_name,
            traits: if world_was_available {
                "`FromReflect`, `Default` or `FromWorld` traits"
            } else {
                "`FromReflect` or `Default` traits"
            },
        });
    };

    if source_trait != "FromReflect" {
        value
            .try_apply(reflected)
            .map_err(|error| FromReflectError::ApplyFailed {
                type_name: type_name.clone(),
                error,
            })?;
    }

    if value.as_any().type_id() != type_id {
        return Err(FromReflectError::MismatchedType {
            type_name,
            source_trait,
        });
    }
    Ok(value)
}
```

Two mechanical details the implementer must get right:

- `world` is moved by `.zip(world)`, so capture `let world_was_available = world.is_some();`
  as the **first** statement of the function and use that in the error arm (as written above).
- `DebugName` is `Clone` (`crates/bevy_utils/src/debug_info.rs:17`) but not `Copy`; clone it
  before the `?` in the `ApplyFailed` arm, as written.

Public wrappers:

```rust
/// Creates a `T` from a `&dyn PartialReflect`, returning an error instead of panicking.
///
/// Unlike [`from_reflect_with_fallback`] this has no access to a [`World`], so the reflected
/// `FromWorld` strategy is not available: only reflected `FromReflect` and `Default` are tried.
/// This is the variant used by the reflection-driven scene path, where the input is user data
/// and failures must be reported rather than aborting the process.
pub fn try_from_reflect_with_fallback<T: Reflect>(
    reflected: &dyn PartialReflect,
    registry: &TypeRegistry,
) -> Result<T, FromReflectError> {
    let type_name = DebugName::type_name::<T>();
    let value = from_reflect_erased(reflected, None, registry, TypeId::of::<T>(), type_name)?;
    // `from_reflect_erased` already verified the concrete `TypeId` matches `T`.
    match value.downcast::<T>() {
        Ok(value) => Ok(*value),
        Err(_) => Err(FromReflectError::MismatchedType {
            type_name: DebugName::type_name::<T>(),
            source_trait: "unknown",
        }),
    }
}

/// Creates a `T` from a `&dyn PartialReflect`.  (docs unchanged from the current impl)
///
/// # Panics
/// ... (unchanged)
pub fn from_reflect_with_fallback<T: Reflect + TypePath>(
    reflected: &dyn PartialReflect,
    world: &mut World,
    registry: &TypeRegistry,
) -> T {
    let type_name = DebugName::type_name::<T>();
    match from_reflect_erased(reflected, Some(world), registry, TypeId::of::<T>(), type_name) {
        Ok(value) => *value
            .downcast::<T>()
            .unwrap_or_else(|_| panic!("Reflected value was not of the expected type")),
        Err(error) => panic!("{error}"),
    }
}
```

Behavior changes to `from_reflect_with_fallback`, all acceptable and to be called out in the PR
description:

1. A type that is not registered at all now panics with "not registered in the `TypeRegistry`"
   instead of "Couldn't create an instance of …". Strictly more informative.
2. `apply` became `try_apply`, so an apply mismatch panics with a wrapped `ApplyError` message
   instead of `apply`'s own panic. Same failure set, different text.
3. The panic strings otherwise keep the substring `Couldn't create an instance of` and
   `produced a value of a different type`. A repo-wide grep confirms **no test asserts on these
   strings** (`grep -rn "Couldn't create an instance of" --include=*.rs .` → only
   `reflect/mod.rs:141`).

### 4.3 `ReflectComponentFns::push_to_bundle_writer`

**File: `crates/bevy_ecs/src/reflect/component.rs`.**

Add to the imports: `crate::bundle::BundleWriter`, `crate::component::ComponentsRegistrator`,
`crate::error::BevyError`, `super::try_from_reflect_with_fallback`.

New field on `ReflectComponentFns` (append after `register_component`, `component.rs:137`):

```rust
    /// Function pointer implementing [`ReflectComponent::push_to_bundle_writer()`].
    ///
    /// Builds a concrete component value from `value` and pushes it into the given
    /// [`BundleWriter`], registering the component in `components` if needed. `value` is first
    /// downcast to the concrete component type (the fast path: values produced by
    /// [`ReflectTemplate::build_template`] are always concrete); if that fails, it is rebuilt
    /// with [`try_from_reflect_with_fallback`].
    ///
    /// This exists so that reflection-driven scene spawning can insert an arbitrary number of
    /// dynamically-typed components with a **single** archetype move, instead of one
    /// `EntityWorldMut::insert` per component.
    ///
    /// # Safety
    ///
    /// `components` must come from the same [`World`] as every other
    /// [`BundleWriter::push_component`] / [`BundleWriter::push_component_by_id`] call on this
    /// `bundle_writer`, and as the following [`BundleWriter::write`].
    ///
    /// [`ReflectTemplate::build_template`]: crate::reflect::ReflectTemplate::build_template
    pub push_to_bundle_writer: unsafe fn(
        Box<dyn PartialReflect>,
        &TypeRegistry,
        &mut ComponentsRegistrator,
        &mut BundleWriter,
    ) -> Result<(), BevyError>,
```

Implementation. To keep the monomorphized surface minimal and to allow a `SAFETY` comment on a
real item rather than inside a closure, add a free generic function next to the
`CreateTypeData` impl and take a function pointer to it:

```rust
/// Backing implementation of [`ReflectComponentFns::push_to_bundle_writer`] for a concrete `C`.
///
/// # Safety
/// See [`ReflectComponentFns::push_to_bundle_writer`].
unsafe fn push_to_bundle_writer<C: Component + Reflect>(
    value: Box<dyn PartialReflect>,
    registry: &TypeRegistry,
    components: &mut ComponentsRegistrator,
    bundle_writer: &mut BundleWriter,
) -> Result<(), BevyError> {
    let component: C = match value.try_take::<C>() {
        Ok(component) => component,
        Err(value) => try_from_reflect_with_fallback::<C>(value.as_ref(), registry)?,
    };
    // SAFETY: the caller guarantees `components` and `bundle_writer` belong to the same `World`
    // as every other push on this writer and as the following `write`.
    unsafe { bundle_writer.push_component(components, component) };
    Ok(())
}
```

and in `impl<C: Component + Reflect + TypePath> CreateTypeData<C> for ReflectComponent`
(`component.rs:309`) add the field:

```rust
            push_to_bundle_writer: push_to_bundle_writer::<C>,
```

Public method on `ReflectComponent` (place after `register_component`, `component.rs:266`):

```rust
    /// Builds a concrete value of this [`Component`] type from `value` and pushes it into
    /// `bundle_writer`.
    ///
    /// See [`ReflectComponentFns::push_to_bundle_writer`].
    ///
    /// # Safety
    ///
    /// `components` must come from the same [`World`] as every other push on `bundle_writer`
    /// and as the following [`BundleWriter::write`].
    pub unsafe fn push_to_bundle_writer(
        &self,
        value: Box<dyn PartialReflect>,
        registry: &TypeRegistry,
        components: &mut ComponentsRegistrator,
        bundle_writer: &mut BundleWriter,
    ) -> Result<(), BevyError> {
        // SAFETY: safety requirements deferred to the caller
        unsafe { (self.0.push_to_bundle_writer)(value, registry, components, bundle_writer) }
    }
```

Notes:

- `<dyn PartialReflect>::try_take::<T: Any>(self: Box<Self>) -> Result<T, Box<dyn PartialReflect>>`
  is `crates/bevy_reflect/src/reflect.rs:492-494`; it internally does `try_into_reflect()` then
  `downcast()`, so it succeeds exactly when the box holds a concrete `C`.
- **Mutability:** `push_component` → `push_component_by_id` → `insert_by_ids_internal` is an
  insertion, which is legal for immutable components. `push_to_bundle_writer` therefore has
  **no mutability guard and never panics for `Component<Mutability = Immutable>`**, unlike
  `apply` / `reflect_mut`. This is deliberate; add a doc sentence saying so.
- **`FromWorld` is unavailable** here (§4.2). A component that has neither `#[reflect(Default)]`
  nor a working `FromReflect`, and that arrives as a *dynamic* value, produces
  `FromReflectError::MissingConstructor` rather than being built via `FromWorld`. In practice the
  dynamic path always hands over concrete values (it builds them from a `ReflectDefault`
  template), so the fallback is a safety net, not the common path. See Open Question 1.
- **Leaking:** if `push_to_bundle_writer` returns `Err`, nothing was pushed for *this* component,
  but previously-pushed components are still in the scratch space. The caller (SPEC-4) must
  follow the existing contract: either `BundleWriter::write` or `BundleScratch::manual_drop`
  (`crates/bevy_ecs/src/bundle/writer.rs:49-71`). Repeat that sentence in the method docs.

### 4.4 `crates/bevy_ecs/src/reflect/template.rs` (new)

```rust
//! Definitions for [`Template`] and [`FromTemplate`] reflection.
//!
//! These two type data types let reflection-driven scene formats (e.g. dynamically loaded
//! `.bsn` assets) answer two questions that are otherwise only expressible in the type system:
//!
//! * Given a component type `C`, what is its [`FromTemplate::Template`] type?
//!   → [`ReflectFromTemplate`], registered with `#[reflect(FromTemplate)]` on `C`.
//! * Given a template value, how do I run [`Template::build_template`] on it?
//!   → [`ReflectTemplate`], registered with `#[reflect(Template)]` on the template type.
//!
//! Both are *opt-in*: their absence has a defined meaning (see each type's docs), so the
//! overwhelmingly common `Clone + Default` blanket case needs no annotation at all.

use alloc::boxed::Box;
use core::any::TypeId;

use bevy_reflect::{CreateTypeData, Reflect, TypePath};

use crate::{
    error::BevyError,
    template::{FromTemplate, Template, TemplateContext},
};

/// Type data registered on a **component** type `C` that has a custom [`FromTemplate`] impl,
/// recording which type is its [`FromTemplate::Template`].
///
/// Register it with `#[reflect(Component, FromTemplate)]` on the component (the
/// `ReflectFromTemplate` type must be in scope, e.g. via `bevy_ecs::prelude::*`).
/// A compiling doctest mirroring §4.7's example belongs here.
///
/// **The absence of this type data means `<C as FromTemplate>::Template == C`** — i.e. the
/// `Clone + Default + Unpin` blanket impl (`crate::template`, blanket at `template.rs:404`).
/// Consumers must default to `TypeId::of::<C>()` when this data is missing, *not* error.
#[derive(Clone, Debug)]
pub struct ReflectFromTemplate {
    /// `TypeId::of::<<C as FromTemplate>::Template>()`.
    pub template_type_id: TypeId,
    /// `<<C as FromTemplate>::Template as TypePath>::type_path()`.
    ///
    /// Consumers need this for error messages in exactly the case where the `TypeId` is useless:
    /// when the template type is *not* present in the `TypeRegistry` and so cannot be named
    /// from the id alone (SPEC-2 `ResolveSceneError::TypeNotRegistered { type_path }`).
    pub template_type_path: &'static str,
}

impl<T> CreateTypeData<T> for ReflectFromTemplate
where
    T: FromTemplate,
    T::Template: Reflect + TypePath,
    <T::Template as Template>::Output: Reflect,
{
    fn create_type_data(_input: ()) -> Self {
        ReflectFromTemplate {
            template_type_id: TypeId::of::<T::Template>(),
            template_type_path: <T::Template as TypePath>::type_path(),
        }
    }
}

/// Type data registered on a **template** type `T` whose [`Template::Output`] differs from `T`,
/// exposing an erased [`Template::build_template`].
///
/// Register it with `#[reflect(Template)]` on the template type.
///
/// **The absence of this type data means the output equals the template**, so a consumer should
/// fall back to cloning the template value (`PartialReflect::reflect_clone`).
#[derive(Clone)]
pub struct ReflectTemplate {
    /// `TypeId::of::<<T as Template>::Output>()`.
    ///
    /// Lets a consumer answer "what does this template produce?" without building it — used by
    /// SPEC-4 to locate [`ReflectComponent`](crate::reflect::ReflectComponent) on the output
    /// type at load time (so the erased insertion path can be resolved once, up front) and to
    /// discover `Handle`-typed outputs for asset-dependency registration (SPEC-0 §7 A-1).
    pub output_type_id: TypeId,
    /// Erased [`Template::build_template`].
    ///
    /// The first argument must be a value of the template type this data was registered for;
    /// passing anything else returns an error rather than panicking. The returned box always
    /// holds a concrete `<T as Template>::Output`, i.e. its `Any::type_id()` always equals
    /// [`Self::output_type_id`].
    pub build_template:
        fn(&dyn Reflect, &mut TemplateContext) -> Result<Box<dyn Reflect>, BevyError>,
}

impl core::fmt::Debug for ReflectTemplate {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ReflectTemplate")
            .field("output_type_id", &self.output_type_id)
            .finish_non_exhaustive()
    }
}

impl<T> CreateTypeData<T> for ReflectTemplate
where
    T: Template + Reflect,
    <T as Template>::Output: Reflect,
{
    fn create_type_data(_input: ()) -> Self {
        ReflectTemplate {
            output_type_id: TypeId::of::<<T as Template>::Output>(),
            build_template: |this, context| {
                let Some(this) = this.downcast_ref::<T>() else {
                    return Err(BevyError::error(
                        "`ReflectTemplate::build_template` was called with a value that is not \
                         of the template type it was registered for",
                    ));
                };
                Ok(Box::new(<T as Template>::build_template(this, context)?))
            },
        }
    }
}
```

#### 4.4.1 Working out the bounds

- `T::Template: 'static` is required for `TypeId::of::<T::Template>()`. It is implied by
  `T::Template: Reflect` (`Reflect: Any`), so it need not be written separately.
- `<T::Template as Template>::Output: Reflect` is Contract A's stated bound. Because
  `FromTemplate::Template: Template<Output = Self>` (`template.rs:350`), this projection
  **normalizes to `T: Reflect`**. Writing it in the contract's form is kept for traceability;
  the implementer may equivalently write `T: Reflect`. Keep the contract's form.
- **`T::Template: Reflect + TypePath` is a ratified strengthening of Contract A** (SPEC-0 §7).
  Rationale: without it, `#[reflect(FromTemplate)]` compiles happily for a component whose
  template type is not reflectable, and the failure surfaces much later as a runtime
  `ResolveSceneError::TypeNotRegistered` naming a type the user never wrote. With it, the error
  is a compile error pointed at the `FromTemplate` ident in the attribute (spans are preserved —
  `registration.rs:52-54`). `TypePath` is additionally required by the ratified
  `template_type_path` field; it is a supertrait-in-practice of anything the `Reflect` derive
  produces, so it costs nothing.
- For `ReflectTemplate`, `T: Reflect` is required in practice anyway (the data can only be
  registered through the `Reflect` derive), and `downcast_ref` needs `T: Any`.
  `TypeId::of::<T::Output>()` for `output_type_id` needs `T::Output: 'static`, implied by
  `T::Output: Reflect`.
- **Types whose `Output` is not `Reflect` simply cannot carry `#[reflect(Template)]`** — they
  get `the trait bound ...: Reflect is not satisfied` at the attribute. That is the intended
  outcome: an erased `build_template` returning a non-`Reflect` value is not expressible.
  `EntityTemplate` (`Output = Entity`) is fine on the `Output` side and becomes `Reflect` itself
  in Phase 5, at which point it can and does carry `#[reflect(Template)]`.

#### 4.4.2 Higher-ranked function pointer types

`fn(&dyn Reflect, &mut TemplateContext) -> Result<Box<dyn Reflect>, BevyError>` elaborates to
`for<'a, 'b, 'c, 'd> fn(&'a dyn Reflect, &'b mut TemplateContext<'c, 'd>) -> ...`
(`TemplateContext<'a, 'w>` is at `template.rs:45`). Closures coerce to it because the expected
type is known from the struct field. Do **not** try to name the lifetimes explicitly.

### 4.5 `crates/bevy_ecs/src/reflect/relationship.rs` (new)

```rust
//! Definitions for [`Relationship`] reflection.

use core::any::TypeId;

use bevy_reflect::{CreateTypeData, Reflect, TypePath, TypeRegistration};

use crate::{
    bundle::BundleWriter,
    component::ComponentsRegistrator,
    entity::Entity,
    reflect::ReflectComponent,
    relationship::{Relationship, RelationshipTarget},
};

/// Type data registered on a [`RelationshipTarget`] type (e.g. [`Children`]), exposing enough
/// of the [`Relationship`] pair to build relationships from types only known at runtime.
///
/// It is registered on the **target** rather than on the [`Relationship`] itself because scene
/// formats name the target: `Children [ ... ]`.
///
/// Register it with `#[reflect(RelationshipTarget)]` on the relationship target — note the
/// attribute ident is the *target* trait's name, since `ident.rs` prefixes it to reach
/// `ReflectRelationshipTarget`. This also registers [`ReflectComponent`] for the target (via
/// [`CreateTypeData::insert_dependencies`]), so `#[reflect(RelationshipTarget)]` implies
/// `#[reflect(Component)]`.
///
/// [`Children`]: crate::hierarchy::Children
#[derive(Clone, Debug)]
pub struct ReflectRelationshipTarget {
    /// `TypeId::of::<<T as RelationshipTarget>::Relationship>()` — e.g. `ChildOf`.
    pub relationship_type_id: TypeId,
    /// `TypeId::of::<T>()` — e.g. `Children`.
    pub relationship_target_type_id: TypeId,
    /// `core::any::type_name::<<T as RelationshipTarget>::Relationship>()`, for error messages.
    pub relationship_name: &'static str,
    /// Pushes `Relationship::from(target)` into a [`BundleWriter`].
    ///
    /// # Safety
    /// `components` must come from the same [`World`](crate::world::World) as every other push
    /// on `bundle_writer` and as the following [`BundleWriter::write`].
    pub insert_relationship:
        unsafe fn(&mut BundleWriter, &mut ComponentsRegistrator, target: Entity),
    /// Pushes `RelationshipTarget::with_capacity(capacity)` into a [`BundleWriter`].
    ///
    /// # Safety
    /// Same as [`Self::insert_relationship`].
    pub insert_relationship_target:
        unsafe fn(&mut BundleWriter, &mut ComponentsRegistrator, usize),
}

impl<T: RelationshipTarget + Reflect + TypePath> CreateTypeData<T> for ReflectRelationshipTarget {
    fn create_type_data(_input: ()) -> Self {
        ReflectRelationshipTarget {
            relationship_type_id: TypeId::of::<T::Relationship>(),
            relationship_target_type_id: TypeId::of::<T>(),
            relationship_name: core::any::type_name::<T::Relationship>(),
            insert_relationship: |bundle_writer, components_registrator, target| {
                let relationship = <T::Relationship as Relationship>::from(target);
                // SAFETY: the caller guarantees `bundle_writer` is only ever used with the
                // `World` that `components_registrator` came from.
                unsafe { bundle_writer.push_component(components_registrator, relationship) };
            },
            insert_relationship_target: |bundle_writer, components_registrator, capacity| {
                let relationship_target = <T as RelationshipTarget>::with_capacity(capacity);
                // SAFETY: the caller guarantees `bundle_writer` is only ever used with the
                // `World` that `components_registrator` came from.
                unsafe {
                    bundle_writer.push_component(components_registrator, relationship_target)
                };
            },
        }
    }

    fn insert_dependencies(type_registration: &mut TypeRegistration) {
        type_registration.register_type_data::<ReflectComponent, T>();
    }
}
```

Differences from `RelatedResolvedScenes::new::<R>()` (`resolved_scene.rs:670-692`), all
mechanical:

| there | here | why |
| --- | --- | --- |
| generic over `R: Relationship` | generic over `T: RelationshipTarget` | Contract B registers on the target; `T::Relationship` recovers `R` |
| `R::from(target)` | `<T::Relationship as Relationship>::from(target)` | same call, longer path |
| `<<R as Relationship>::RelationshipTarget as RelationshipTarget>::with_capacity(c)` | `<T as RelationshipTarget>::with_capacity(c)` | `T` *is* the target |
| `relationship_name: type_name::<R>()` | `type_name::<T::Relationship>()` | identical string |

Both fn-pointer bodies compile inside `bevy_ecs` unchanged: `BundleWriter`
(`crate::bundle::BundleWriter`) and `ComponentsRegistrator`
(`crate::component::ComponentsRegistrator`) are `bevy_ecs` types; `bevy_scene` merely re-uses
them. The `unsafe` blocks and their `SAFETY` comments are the same obligation as in
`resolved_scene.rs:676, 684-687`, restated in `# Safety` sections on the two fields (which
`RelatedResolvedScenes` does not currently have — an improvement, since Bevy's lint set requires
safety docs on public `unsafe fn` items).

`insert_dependencies` mirrors `ReflectResource` (`reflect/resource.rs:33-41`). It is why the
impl carries the extra `Reflect + TypePath` bounds (they are `ReflectComponent`'s
`CreateTypeData` bounds; `RelationshipTarget: Component` supplies the rest).

### 4.6 Exports

`crates/bevy_ecs/src/reflect/mod.rs` — add next to the existing `mod`/`pub use` block
(`mod.rs:14-31`):

```rust
mod relationship;
mod template;
// ...
pub use relationship::ReflectRelationshipTarget;
pub use template::{ReflectFromTemplate, ReflectTemplate};
```

`crates/bevy_ecs/src/lib.rs:113-117` — extend the reflect prelude so the short attribute idents
resolve in crates that do `use bevy_ecs::prelude::*`:

```rust
    #[doc(hidden)]
    #[cfg(feature = "bevy_reflect")]
    pub use crate::reflect::{
        AppTypeRegistry, ReflectComponent, ReflectEvent, ReflectFromTemplate, ReflectFromWorld,
        ReflectMessage, ReflectRelationshipTarget, ReflectResource, ReflectTemplate,
    };
```

(`FromReflectError` and `try_from_reflect_with_fallback` are **not** preludes; they are reached
as `bevy_ecs::reflect::…`.)

### 4.7 Registration story: what a user writes, and what it expands to

For a component with a custom template:

```rust
use bevy_ecs::prelude::*;          // brings ReflectFromTemplate into scope
use bevy_reflect::prelude::*;

#[derive(Component, FromTemplate, Reflect)]
#[reflect(Component, FromTemplate)]      // <- the new bit
struct Sprite { image: Handle<Image> }
```

Expansion (`registration.rs:47-79`), inside `Sprite::get_type_registration()`:

```rust
registration.register_type_data_with::<ReflectComponent, Self, _>(());
registration.register_type_data_with::<ReflectFromTemplate, Self, _>(());
```

`register_type_data_with` (`type_registry.rs:704-709`) does
`insert(<ReflectFromTemplate as CreateTypeData<Sprite, ()>>::create_type_data(()))` followed by
`ReflectFromTemplate::insert_dependencies(registration)` (a no-op here).

For the template type:

```rust
#[derive(Reflect)]
#[reflect(Default, Template)]            // ReflectTemplate must be in scope
struct SpriteTemplate { image: HandleTemplate<Image> }
```

For a relationship target — the attribute may be written as a **path**, avoiding an import
(only the last segment is `Reflect`-prefixed, `ident.rs:24-38`):

```rust
#[reflect(Component, FromWorld, Default, crate::reflect::RelationshipTarget)]
```

Failure modes and what the compiler says (all pointed at the attribute ident):

| mistake | diagnostic |
| --- | --- |
| `ReflectFromTemplate` not in scope | `cannot find type ReflectFromTemplate in this scope` |
| component has no custom `FromTemplate` and is not `Clone + Default + Unpin` | `the trait bound Sprite: FromTemplate is not satisfied` |
| template type is not `Reflect` | `the trait bound SpriteTemplate: Reflect is not satisfied` |
| `#[reflect(Template)]` on a type whose `Output` is not `Reflect` | `the trait bound …::Output: Reflect is not satisfied` |
| `#[reflect(RelationshipTarget)]` on a non-target | `the trait bound X: RelationshipTarget is not satisfied` |
| attribute repeated | `conflicting type data registration` (`container_attributes.rs:45`) |

Generic types (`Handle<A>`, `HandleTemplate<A>`, `VirtualKeyboard<T>`) are a special case: the
`Reflect` derive adds **no** where-clauses for type data (`registration.rs:28`), so
`#[reflect(FromTemplate)]` on a generic type may fail to prove its bounds. For those, register at
runtime instead: `type_registry.register_type_data::<Handle<A>, ReflectFromTemplate>()`
(`type_registry.rs:344-352`). This is what §4.8.2 does, and it is also why generic types are
never auto-registered (`impls/common.rs:183-185`).

### 4.8 In-repo types that get annotations

#### 4.8.1 `Children` — `crates/bevy_ecs/src/hierarchy.rs:150`

```diff
-#[cfg_attr(feature = "bevy_reflect", reflect(Component, FromWorld, Default))]
+#[cfg_attr(
+    feature = "bevy_reflect",
+    reflect(Component, FromWorld, Default, crate::reflect::RelationshipTarget)
+)]
 pub struct Children(Vec<Entity>);
```

`Children` is the only production `RelationshipTarget` in the repo that is `Reflect`. The other
one, `HasWindows` (`crates/bevy_window/src/monitor.rs:60-61`), derives only
`Component, Debug, Default` and its `Relationship` `OnMonitor`
(`crates/bevy_window/src/window.rs:1562-1564`) is likewise not `Reflect` — out of scope
(Open Question 3). Test-only targets (`crates/bevy_ecs/src/relationship/mod.rs:814+`,
`crates/bevy_remote/src/builtin_methods.rs:2213`, `examples/ecs/relationships.rs:33`) are not
annotated.

#### 4.8.2 `Handle<A>` / `HandleTemplate<A>` — `crates/bevy_asset/src/lib.rs:690-708`

`register_asset_reflect` already registers both types and even the
`String → HandleTemplate<A>` conversion (`lib.rs:698-703`). Add two lines:

```diff
             type_registry.register_type_data::<A, ReflectAsset>();
             type_registry.register_type_data::<Handle<A>, ReflectHandle>();
+            type_registry.register_type_data::<Handle<A>, ReflectFromTemplate>();
+            type_registry.register_type_data::<HandleTemplate<A>, ReflectTemplate>();
             type_registry
                 .register_type_conversion::<String, HandleTemplate<A>, _>(|s| Ok(s.into()));
```

with `use bevy_ecs::reflect::{ReflectFromTemplate, ReflectTemplate};` added to the imports.

Bounds check: `Handle<A>: FromTemplate` (`crates/bevy_asset/src/handle.rs:242`);
`HandleTemplate<A>: Reflect` (`handle.rs:269`, `#[derive(Reflect)]`);
`HandleTemplate<A>: Template<Output = Handle<A>>` (`handle.rs:339`); `Handle<A>: Reflect`
(`handle.rs:132`). The function's existing bounds
(`A: Asset + Reflect + FromReflect + GetTypeRegistration`) already prove `Handle<A>` and
`HandleTemplate<A>` are registerable, since `register::<Handle<A>>()` /
`register::<HandleTemplate<A>>()` are already called there. If the compiler still asks for
`A: TypePath`, add it to the `where` clause of `register_asset_reflect` **and** of its trait
declaration — do not weaken the type data bounds.

**No `Reflect` impl needs to be added in `bevy_asset`.** (The parent question "does
`HandleTemplate` need a reflect impl added there?" — answer: no, `handle.rs:269` already has
`#[derive(Reflect)]`, and it is already registered and already has a `ReflectConvert` from
`String`.)

#### 4.8.3 Components with derive-generated templates

The repo has ~50 `#[derive(FromTemplate)]` types (full inventory below). **None of their
generated `*Template` types is `Reflect`**, so none of them can carry `#[reflect(FromTemplate)]`
until Phase 5 lands. Phase 5 seed set (non-generic, already `Reflect`, high value for `.bsn`):

| component | file | template |
| --- | --- | --- |
| `Sprite` | `crates/bevy_sprite/src/sprite.rs:15` | `SpriteTemplate` |
| `ImageNode` | `crates/bevy_ui/src/widget/image.rs:15` | `ImageNodeTemplate` |
| `TextFont` | `crates/bevy_text/src/text.rs:669` | `TextFontTemplate` |
| `AudioPlayer` | `crates/bevy_audio/src/audio.rs:248` | `AudioPlayerTemplate` |
| `Mesh2d` | `crates/bevy_mesh/src/components.rs:40` | `Mesh2dTemplate` |
| `Mesh3d` | `crates/bevy_mesh/src/components.rs:97` | `Mesh3dTemplate` |
| `MeshMaterial2d<M>` | `crates/bevy_sprite_render/src/mesh2d/material.rs:213` | generic — runtime registration only |
| `MeshMaterial3d<M>` | `crates/bevy_pbr/src/mesh_material.rs:39` | generic — runtime registration only |

Remaining `#[derive(FromTemplate)]` components, for completeness (all deferred; annotate later
in the same mechanical way once Phase 5 exists): `ChildOf` (`bevy_ecs/src/hierarchy.rs:94`),
`SimplifiedMesh` (`bevy_picking/src/mesh_picking/ray_cast/mod.rs:113`), `SpriteMesh`
(`bevy_sprite/src/sprite_mesh.rs:16`), `AnimationGraphHandle` (`bevy_animation/src/graph.rs:134`),
`Readback` (`bevy_render/src/gpu_readback.rs:83`, not `Reflect`), `Mesh2dWireframe`
(`bevy_sprite_render/src/mesh2d/wireframe2d.rs:446`), `TilemapChunk`
(`bevy_sprite_render/src/tilemap_chunk/mod.rs:52`), `SkinnedMesh` (`bevy_mesh/src/skinning.rs:16`),
`MaterialNode` (`bevy_ui_render/src/ui_material.rs:166`), `Lightmap`
(`bevy_pbr/src/lightmap/mod.rs:87`), `MeshletMesh3d` (`bevy_pbr/src/meshlet/mod.rs:229`),
`Mesh3dWireframe` (`bevy_pbr/src/wireframe.rs:935`), `AtmosphereEnvironmentMap`
(`bevy_pbr/src/atmosphere/environment.rs:33`, not `Reflect`), `RaytracingMesh3d`
(`bevy_solari/src/scene/types.rs:18`), `EnvironmentMapLight` / `Skybox` /
`GeneratedEnvironmentMapLight` / `IrradianceVolume` (`bevy_light/src/probe.rs:105, 233, 267, 335`),
`PointLightTexture` / `SpotLightTexture` / `DirectionalLightTexture`
(`bevy_light/src/{point_light.rs:159, spot_light.rs:209, directional_light.rs:173}`), `Atmosphere`
(`bevy_light/src/atmosphere.rs:33`, not `Reflect`), `Gizmo` (`bevy_gizmos/src/retained.rs:64`),
`GizmoMeshConfig` (`bevy_gizmos/src/config.rs:279`, not `Reflect`), `RenderTarget` /
`ManualTextureViewHandle` (`bevy_camera/src/camera.rs:890, 966`), `EntityCursor` /
`InheritableFont` (`bevy_feathers/src/{cursor.rs:32, font_styles.rs:19}`), the `SceneComponent`
widgets (`bevy_feathers/src/controls/{color_plane.rs:50, checkbox.rs:49, virtual_keyboard.rs:20}`),
`Scrollbar` (`bevy_ui_widgets/src/scrollbar.rs:67`), `ScenePatchInstance`
(`bevy_scene/src/scene_patch.rs:109`, not `Reflect`), `WorldAssetRoot` / `DynamicWorldRoot`
(`bevy_world_serialization/src/components.rs:17, 35`). Non-component `FromTemplate` derives
(`FontSource`, `GenericFontFamily`, `ImageRenderTarget`, `TextureAtlas`, `CustomCursorImage`,
`CustomCursor`) are field types, not components; they need `Reflect` on their templates for
Phase 5 to reach the components that contain them, but never `#[reflect(FromTemplate)]`
themselves unless they are used as a component.

The other two explicit `impl FromTemplate` blocks are `Entity`/`EntityTemplate`
(`bevy_ecs/src/template.rs:474`, handled by Phase 5 Step 11b) and
`SystemHandle`/`SystemHandleTemplate` (`bevy_ecs/src/system/system_registry.rs:307`, neither is
`Reflect`; out of scope).

### 4.9 Phase 5 is a ratified series prerequisite (NORMATIVE)

#### 4.9.1 Why it is required

`crates/bevy_ecs/macros/src/template.rs` emits the generated template type with **no derives at
all** (only `#[allow(missing_docs)]` plus hand-written `impl Template` and `impl Default`), so
every one of the ~50 `#[derive(FromTemplate)]` components in the repo has a non-reflectable
template. Three consequences, each independently blocking:

1. Contract E (SPEC-4) constructs the template with `ReflectDefault` **on the template type** and
   patches its fields reflectively — impossible if the template is not `Reflect`.
2. Decision-log item 3 requires keying template slots by the **real** template `TypeId` so that
   dynamic `.bsn` patches and `bsn!` patches of the same component merge. If the dynamic path
   fell back to using the component as its own template, the two would land in *different*
   slots — silently losing the merge exactly where it matters most.
3. `Sprite { image: "player.png" }` cannot work at all without `SpriteTemplate`, because the
   `String → HandleTemplate<A>` conversion (`bevy_asset/src/lib.rs:702`) targets the *template*
   field type, not `Handle<A>`.

SPEC-0 §7 ratifies Phase 5 as a prerequisite of SPEC-4 and additionally folds SPEC-4's P3
(`EntityTemplate: Reflect`) into it. Components without `#[template(reflect)]` are **not
`.bsn`-usable** and fail at load with `ResolveSceneError::TypeNotRegistered` naming
`ReflectFromTemplate::template_type_path` — which is precisely why that field was ratified.

Phase 5 ships as a **second PR** (Steps 11-13). Steps 1-10 do not depend on it and are
independently upstreamable.

#### 4.9.2 `FromReflect` on generated templates is a hard requirement

SPEC-2's C-7 typed-recovery path (`get_or_insert_template::<T>` recovering a typed slot from a
dynamic occupant) looks up `ReflectFromReflect` for `TypeId::of::<T>()`, calls `from_reflect`,
and downcasts the result to the concrete `T`. `T` there is a **template type**. Therefore every
type produced by Phase 5's `#[template(reflect)]` emission **must** carry auto-derived
`FromReflect`.

The `Reflect` derive registers `ReflectFromReflect` automatically
(`crates/bevy_reflect/derive/src/registration.rs:30-37`, gated on
`meta.from_reflect().should_auto_derive()`), which is on by default and disabled only by an
explicit `#[reflect(from_reflect = false)]`. **Phase 5 must therefore never emit
`from_reflect = false` on a generated template**, and Step 12 must assert this: the emitted
attribute list is exactly `#[derive(Reflect)] #[reflect(Default, <path>::Template)]`. A test
(`generated_template_has_from_reflect`, §7.7) pins it. Note the same requirement makes
`try_from_reflect_with_fallback`'s first rung work for dynamic template values, so it is
doubly load-bearing.

### 4.10 Binary size

`component.rs:45-54` warns that each new `ReflectComponentFns` field costs one monomorphized
function per `#[reflect(Component)]` type. This spec adds exactly one field, so the cost is
`1 × M`. The generated body is:

1. `<dyn PartialReflect>::try_take::<C>` — one vtable call plus a `TypeId` compare and a move.
2. `try_from_reflect_with_fallback::<C>` — a thin wrapper: `TypeId::of`, `DebugName::type_name`,
   a call to the shared `#[inline(never)] from_reflect_erased`, and `Box::downcast`. All real
   work is in the shared erased function.
3. `BundleWriter::push_component::<C>` — `register_component::<C>()`, `OwningPtr::make`,
   `Layout::new::<C>()`, a `memcpy`.

Item 3 dominates and is unavoidable for a typed push. Mitigations already applied: the fallback
ladder is erased and `#[inline(never)]`; the body is a named generic `fn` (not a closure) so it
is emitted once per `C` and never inlined into a caller.

**Required measurement (acceptance criterion A7):** build `cargo build --release --example
breakout` before and after the change and record both binary sizes in the PR description.
Budget: **≤ 1.5 %** growth. If exceeded, fall back to the alternative in Open Question 2 rather
than shipping over budget.

---

## 5. Step-by-step implementation plan

Every step leaves `cargo check -p bevy_ecs --all-features` (and, from Step 7, `cargo check
--workspace`) green.

**Step 1 — error type and non-panicking ladder.**
In `crates/bevy_ecs/src/reflect/mod.rs`, add `FromReflectError`, `from_reflect_erased`,
`try_from_reflect_with_fallback`, and rewrite `from_reflect_with_fallback` on top of the shared
helper exactly as in §4.2. No other file changes. Verify: `cargo test -p bevy_ecs`.

**Step 2 — `reflect/template.rs`.**
Create the file verbatim from §4.4, add `mod template;` + `pub use` in `reflect/mod.rs`. No
callers yet.

**Step 3 — `reflect/relationship.rs`.**
Create the file verbatim from §4.5, add `mod relationship;` + `pub use`.

**Step 4 — prelude.**
Extend `crates/bevy_ecs/src/lib.rs:113-117` per §4.6.

**Step 5 — `push_to_bundle_writer`.**
Add the field, the free `unsafe fn push_to_bundle_writer<C>`, the `CreateTypeData` field
initializer, and the `ReflectComponent` method per §4.3. Also update the module doc at
`component.rs:45-54` with a sentence noting this field's cost and its erased-ladder mitigation.
This is the step that must compile for *every* `#[reflect(Component)]` type in the workspace, so
run `cargo check --workspace --all-features` here.

**Step 6 — `Children` annotation.**
Apply the diff in §4.8.1. Verify with the new test
`children_registers_reflect_relationship` (§7).

**Step 7 — `bevy_asset` registration.**
Apply the diff in §4.8.2. Run `cargo check --workspace --all-features`.

**Step 8 — tests.**
Add all tests from §7. Run `cargo test -p bevy_ecs --all-features` and
`cargo test -p bevy_asset`.

**Step 9 — docs & release notes.**
Add `_release-content/release-notes/…` entry? No — this is an additive API with no migration.
Add a migration guide only if Step 1's panic-text change is judged user-visible (it is not:
panics are not API). Instead, document the new type data in the `reflect` module docs and
mention `push_to_bundle_writer` in the `ReflectComponentFns` docs (done in Step 5).

**Step 10 — size measurement.** Per §4.10. Record in the PR body.

### Phase 5 (second PR, RATIFIED PREREQUISITE for SPEC-4): reflectable templates

Rationale and normative status: §4.9. Nothing in Steps 1-10 depends on Phase 5, so it ships as a
separate PR, but SPEC-4 cannot land without it.

**Step 11a — `Reflect` for the built-in collection templates.**
In `crates/bevy_ecs/src/template.rs`, add to `OptionTemplate<T>` (`:521-528`) and
`VecTemplate<T>` (`:564`):

```rust
#[cfg_attr(feature = "bevy_reflect", derive(bevy_reflect::Reflect))]
```

No `#[reflect(Default)]` (their `Default` impls are generic and would need a manual
`#[reflect(where …)]`; SPEC-4 constructs them by conversion/`FromReflect`, not by `Default`).
`TemplateTuple<T>` (`:384`) gets the same treatment only if a field of that type appears in a
seed-set template — check before adding.

**Step 11b — `Reflect` for `EntityTemplate` (absorbs SPEC-4's P3).**
`EntityTemplate` (`crates/bevy_ecs/src/template.rs:421-431`) is the template behind `#Name`
entity references and must be `Reflect` for SPEC-4 to construct one reflectively. Its blocker is
`SceneEntityReference` (`template.rs:141-152`), which wraps
`Hashed<InnerSceneEntityReference>` containing a `&'static str`. Fix, in order:

1. On `SceneEntityReference` (`template.rs:142`):

   ```rust
   #[cfg_attr(feature = "bevy_reflect", derive(bevy_reflect::Reflect))]
   #[cfg_attr(feature = "bevy_reflect", reflect(opaque, Clone, PartialEq, Hash, Debug))]
   ```

   `#[reflect(opaque)]` treats the value as a leaf: no field reflection, no `TypePath` demands on
   `Hashed`/`InnerSceneEntityReference`. It requires `Clone + Send + Sync + 'static`, all of
   which `SceneEntityReference` already has (it is `Copy`). Opaque is the right semantic anyway —
   `.bsn` never patches a reference's *fields*, it constructs whole references.
2. On `EntityTemplate` (`template.rs:421`):

   ```rust
   #[cfg_attr(feature = "bevy_reflect", derive(bevy_reflect::Reflect))]
   #[cfg_attr(feature = "bevy_reflect", reflect(Default, Clone, Debug, crate::reflect::Template))]
   ```

   `EntityTemplate` is a `#[derive(Default)]` enum with `#[default] None`, its `Output` is
   `Entity` (which is `Reflect`), so both `ReflectDefault` and `ReflectTemplate` bounds hold.
3. Register both in whatever registers core `bevy_ecs` types — search for
   `register::<Entity>()`; if `bevy_ecs` does not self-register, they are picked up by
   `reflect_auto_register` (neither type is generic, so auto-registration applies:
   `impls/common.rs:183-185`).

This is independent of, and composes with, SPEC-2's `SceneEntityReferenceSource` change: an
opaque type's internal representation is invisible to reflection, so SPEC-2 may reshape
`InnerSceneEntityReference` freely afterwards. **Constraint handed to SPEC-2:** keep
`SceneEntityReference` `Copy + Clone + Eq + Hash + Debug + Send + Sync + 'static` (SPEC-0 §7
already ratifies the `Copy` enum for exactly this reason).

**Step 12 — `#[template(reflect)]` in the `FromTemplate` derive.**
In `crates/bevy_ecs/macros/src/template.rs`:

- add `const TEMPLATE_REFLECT_ATTRIBUTE: &str = "reflect";`
- before building `template`, scan `ast.attrs` for `#[template(...)]` at container level and set
  `let derive_reflect: bool` when it contains the bare ident `reflect`. Emit a
  `syn::Error` for any other container-level `template(...)` content.
- resolve the reflect path once:
  `let bevy_reflect = BevyManifest::shared(|m| m.get_path("bevy_reflect"));`
  (same pattern as line 15's `get_path("bevy_ecs")`).
- when `derive_reflect`, prepend to each of the four generated `#template_ident` definitions
  (lines 40, 71, 102, and the enum at ~273):

  ```rust
  #[derive(#bevy_reflect::Reflect)]
  #[reflect(Default, #bevy_ecs::reflect::Template)]
  ```

  and **nothing else** — in particular never `from_reflect = false`, so that the derive keeps
  auto-registering `ReflectFromReflect` (§4.9.2; SPEC-2's C-7 recovery depends on it).
  The path form is required — the generated code lands in the *user's* module, where
  `ReflectTemplate` may not be imported; `ident.rs:24-38` rewrites only the last segment, giving
  `#bevy_ecs::reflect::ReflectTemplate`.
- document the attribute in the `FromTemplate` rustdoc (`crates/bevy_ecs/src/template.rs:195-347`),
  including the requirement that every field's template type is itself `Reflect`.

**Step 13 — apply to the seed set.**
For each row of §4.8.3's seed table: add `#[template(reflect)]` to the component and
`FromTemplate` to its `#[reflect(...)]` list (e.g.
`#[reflect(Component, Default, Debug, Clone, FromTemplate)]` for `Sprite`), adding
`use bevy_ecs::reflect::ReflectFromTemplate;` where `bevy_ecs::prelude::*` is not already
imported. Compile after **each** crate; a field whose template type is not `Reflect` will fail
here and must either get `#[template(reflect)]` itself (if it is a `FromTemplate` derive) or be
dropped from the seed set with a note.

Also register the generated templates: a `#[template(reflect)]` template type is a plain
non-generic `Reflect` type, so `reflect_auto_register` picks it up. For crates that register
types explicitly in their plugin `build()`, add `app.register_type::<SpriteTemplate>()` next to
the existing `register_type::<Sprite>()` call. Verify per type with
`template_type_registered_for_seed_set` (§7.7).

---

## 6. Edge cases & error handling

1. **Missing `ReflectFromTemplate`** — defined to mean `Template == C`. Consumers must not
   error. Stated in the rustdoc; asserted by `from_template_absent_means_self` (§7).
2. **Missing `ReflectTemplate`** — defined to mean output == template; consumer clones. Stated
   in the rustdoc.
3. **`build_template` called with the wrong receiver type** — returns
   `Err("… not of the template type it was registered for")`, never panics. Asserted by
   `template_build_rejects_wrong_receiver`.
4. **`build_template` propagates user errors** — `Template::build_template` returns
   `Result<_, BevyError>` (e.g. `EntityTemplate::None` at `template.rs:458`); the erased version
   forwards with `?`. Asserted by `template_build_propagates_error`.
5. **`push_to_bundle_writer` with a concrete value** — fast path, no registry lookup at all.
   Works even for components with **no** `FromReflect` and **no** `Default`.
6. **`push_to_bundle_writer` with a `DynamicStruct`** — `try_take` fails, ladder runs. If the
   type has neither `FromReflect` nor `Default` type data, returns
   `FromReflectError::MissingConstructor` → converted into `BevyError` by `?`
   (`impl<E> From<E> for BevyError where Box<dyn Error + Send + Sync>: From<E>`,
   `crates/bevy_ecs/src/error/bevy_error.rs:515-521`).
7. **`push_to_bundle_writer` with a value of an unrelated concrete type** — `try_take` fails,
   `FromReflect` fails (type mismatch), `Default` + `try_apply` fails →
   `FromReflectError::ApplyFailed`. Never a panic, never a wrong-typed push.
8. **Immutable components** — supported (see §4.3). Asserted by
   `push_to_bundle_writer_supports_immutable_components`.
9. **Error mid-bundle leaks** — documented caller obligation (`BundleScratch::manual_drop`).
   SPEC-4 must honor it; SPEC-1 only documents it.
10. **`Children` already carrying a custom `ReflectComponent`** — `insert_dependencies` would
    overwrite it with the default one. No such case exists in-repo (nothing constructs a custom
    `ReflectComponent` for a relationship target); the same hazard already exists for
    `ReflectResource`. Noted, not guarded.
11. **`no_std`** — everything used here (`alloc::boxed::Box`, `core::any`, `thiserror` with
    `default-features = false`) is `no_std`-compatible. `DebugName` collapses to a
    zero-field struct without the `debug` feature and still `Display`s
    (`crates/bevy_utils/src/debug_info.rs:10-36`), so error messages degrade gracefully.
12. **`bevy_reflect` feature off** — the whole `reflect` module disappears
    (`lib.rs:47-48`); `Children`'s attribute is already inside `cfg_attr(feature =
    "bevy_reflect", …)`. Verify with `cargo check -p bevy_ecs --no-default-features
    --features std`.

---

## 7. Test plan

All tests are `#[cfg(test)] mod tests` in the file under test unless stated otherwise.

### 7.1 `crates/bevy_ecs/src/reflect/mod.rs`

| test | asserts |
| --- | --- |
| `try_from_reflect_unregistered_type_errors` | a type never registered → `Err(FromReflectError::NotRegistered { .. })` |
| `try_from_reflect_missing_constructor_errors` | registered with `#[reflect(no_auto_register)]`-style bare `Reflect` but `from_reflect = false` and no `Default` → `Err(MissingConstructor)`, and the message contains "`FromReflect` or `Default` traits" (i.e. *not* `FromWorld`) |
| `try_from_reflect_uses_from_reflect` | a `DynamicStruct` with all fields → `Ok(value)` equal to the expected struct |
| `try_from_reflect_uses_default_and_applies` | a type with `#[reflect(Default)]`, `from_reflect = false`, partial `DynamicStruct` → `Ok` with patched field and defaulted other field |
| `from_reflect_with_fallback_still_uses_from_world` | a type with only `#[reflect(FromWorld)]` still constructs through the panicking entry point (regression guard for the Step-1 refactor) |

### 7.2 `crates/bevy_ecs/src/reflect/template.rs`

Fixtures: `struct CustomComponent(u32)` with a hand-written
`struct CustomComponentTemplate(u32)` implementing `Template<Output = CustomComponent>`, both
`#[derive(Reflect)]`; plus `#[derive(Component, Reflect, Clone, Default)] struct Plain(u32)`.

| test | asserts |
| --- | --- |
| `from_template_reports_custom_template` | `registry.get_type_data::<ReflectFromTemplate>(TypeId::of::<CustomComponent>()).unwrap().template_type_id == TypeId::of::<CustomComponentTemplate>()` |
| `from_template_reports_template_type_path` | the same data's `template_type_path == <CustomComponentTemplate as TypePath>::type_path()`, and the assertion holds **without** `CustomComponentTemplate` itself being registered (that is the case the field exists for) |
| `template_reports_output_type_id` | `registry.get_type_data::<ReflectTemplate>(TypeId::of::<CustomComponentTemplate>()).unwrap().output_type_id == TypeId::of::<CustomComponent>()` |
| `template_output_type_id_matches_built_value` | the box returned by `build_template` has `Any::type_id() == output_type_id` (pins the invariant SPEC-4 relies on when it pre-resolves `ReflectComponent` from `output_type_id`) |
| `from_template_absent_means_self` | `registry.get_type_data::<ReflectFromTemplate>(TypeId::of::<Plain>()).is_none()` — documents the "absence means self" contract |
| `from_template_registered_on_blanket_type_is_self` | when `#[reflect(FromTemplate)]` *is* written on a `Clone + Default` type, `template_type_id == TypeId::of::<Plain>()` |
| `template_build_produces_output` | `ReflectTemplate::build_template` on a `CustomComponentTemplate(7)` returns a box that `downcast::<CustomComponent>()`s to `CustomComponent(7)` |
| `template_build_rejects_wrong_receiver` | passing a `Plain` value returns `Err` and does not panic |
| `template_build_propagates_error` | a fixture template whose `build_template` returns `Err("boom")` surfaces that error |
| `template_build_can_use_context` | a fixture template that calls `context.entity.id()` builds successfully inside `world.spawn_empty()` scope (proves the higher-ranked fn-pointer signature is usable) |

`TemplateContext` construction in tests: `let mut refs = SceneEntityReferences::default(); let mut
entity = world.spawn_empty(); let mut ctx = TemplateContext::new(&mut entity, &mut refs);`
(`crates/bevy_ecs/src/template.rs:52-62`).

### 7.3 `crates/bevy_ecs/src/reflect/relationship.rs`

| test | asserts |
| --- | --- |
| `children_registers_reflect_relationship` | after `registry.register::<Children>()`, `get_type_data::<ReflectRelationshipTarget>(TypeId::of::<Children>())` is `Some`, `relationship_type_id == TypeId::of::<ChildOf>()`, `relationship_target_type_id == TypeId::of::<Children>()`, `relationship_name == core::any::type_name::<ChildOf>()` |
| `reflect_relationship_target_registers_reflect_component` | the same registration also yields `get_type_data::<ReflectComponent>(TypeId::of::<Children>())` (`insert_dependencies`) |
| `reflect_relationship_target_inserts_relationship` | using a `BundleScratch`, call `insert_relationship(&mut writer, &mut registrator, parent)` then `write(&mut child)`; assert `child.get::<ChildOf>().unwrap().parent() == parent` and `parent` has `Children` containing `child` (the relationship hook ran) |
| `reflect_relationship_target_inserts_relationship_target_with_capacity` | `insert_relationship_target(.., 4)` then write; assert the entity has an empty `Children` and `children.capacity() >= 4` if a capacity accessor exists, otherwise just that `Children` is present and empty |
| `reflect_relationship_target_custom` | a locally-defined `#[derive(Component, Reflect)] #[relationship_target(relationship = Likes)] struct LikedBy(Vec<Entity>)` with `#[reflect(RelationshipTarget)]` produces the correct ids — proves the data is not `Children`-specific |

Model the `BundleScratch` usage on `crates/bevy_ecs/src/bundle/writer.rs:185-204`.

### 7.4 `crates/bevy_ecs/src/reflect/component.rs`

| test | asserts |
| --- | --- |
| `push_to_bundle_writer_takes_concrete_value` | `Box::new(Marker(3)) as Box<dyn PartialReflect>` is pushed and written; entity has `Marker(3)`. Fixture has **no** `Default` and `from_reflect = false`, proving the fast path never touches the ladder |
| `push_to_bundle_writer_uses_from_reflect` | a `DynamicStruct` representing `Marker { value: 3 }` produces `Marker { value: 3 }` |
| `push_to_bundle_writer_uses_default_fallback` | partial `DynamicStruct` (one of two fields) on a `#[reflect(Default)] from_reflect = false` type → other field is the default |
| `push_to_bundle_writer_errors_without_constructor` | dynamic value + type with neither `FromReflect` nor `Default` → `Err`, and `bundle_writer.is_empty()` is still true |
| `push_to_bundle_writer_supports_immutable_components` | `#[component(immutable)]` fixture is pushed and written successfully (contrast: `ReflectComponent::apply` panics for it) |
| `push_to_bundle_writer_single_archetype_move` | push two components then one `write`; a component hook `on_insert` registered on the first asserts the entity **already has** the second — i.e. both arrived in one move |
| `push_to_bundle_writer_registers_component` | the component id is registered by the call itself: `world.component_id::<Marker>()` is `None` before and `Some` after |

Each of these needs `unsafe { … }` with a `// SAFETY: a single World is used for every writer
operation` comment, mirroring `writer.rs:193-194`.

### 7.5 `crates/bevy_asset`

In `crates/bevy_asset/src/lib.rs` tests (or `handle.rs` tests): `handle_template_type_data_registered`
— build an `App` with `AssetPlugin`, `init_asset::<TestAsset>()` +
`register_asset_reflect::<TestAsset>()`, then assert
`ReflectFromTemplate` on `Handle<TestAsset>` has `template_type_id ==
TypeId::of::<HandleTemplate<TestAsset>>()` and `ReflectTemplate` on `HandleTemplate<TestAsset>`
is `Some`.

### 7.6 Compile-fail (optional but recommended)

`crates/bevy_reflect/compile_fail` is bevy_reflect-local, so instead add a `#[test]` in
`reflect/template.rs` documented as "must not compile" only if a `trybuild` harness already
exists for `bevy_ecs` — **it does not**, so skip. Document the four failure modes in §4.7 in
rustdoc instead.

### 7.7 Phase 5 (second PR)

In `crates/bevy_ecs/src/template.rs` tests:

| test | asserts |
| --- | --- |
| `template_reflect_attribute_generates_reflect_template` | a local `#[derive(FromTemplate)] #[template(reflect)] struct Foo { count: usize }` yields a `FooTemplate` for which `TypeRegistry::register::<FooTemplate>()` then `get_type_data::<ReflectTemplate>()` is `Some` with `output_type_id == TypeId::of::<Foo>()` |
| `generated_template_has_from_reflect` | the same registration also has `ReflectFromReflect` **and** `ReflectDefault` — pins §4.9.2, the requirement SPEC-2's C-7 recovery depends on |
| `generated_template_round_trips_through_from_reflect` | a partial `DynamicStruct` for `FooTemplate` → `try_from_reflect_with_fallback::<FooTemplate>` → `Ok`, with the unpatched field defaulted |
| `template_reflect_attribute_is_opt_in` | a `#[derive(FromTemplate)]` **without** `#[template(reflect)]` still compiles and its template is not `Reflect` (compile-level: simply that the existing tests still pass) |
| `entity_template_is_reflect` | `TypeRegistry::register::<EntityTemplate>()`; `get_type_data::<ReflectDefault>` and `get_type_data::<ReflectTemplate>` are both `Some`, and the latter's `output_type_id == TypeId::of::<Entity>()` |
| `scene_entity_reference_is_opaque` | `SceneEntityReference::default_or_new(...)` reflected has `ReflectRef::Opaque` and `reflect_clone()` succeeds |
| `option_and_vec_templates_are_reflect` | `OptionTemplate<u32>` and `VecTemplate<u32>` register and produce `TypeInfo` without panicking |

Per-crate (Step 13), one test in each seed crate:
`template_type_registered_for_seed_set` — after building an `App` with that crate's plugin,
`AppTypeRegistry` contains the component's `ReflectFromTemplate`, and its `template_type_id`
resolves to a registration that carries `ReflectTemplate` + `ReflectDefault`. This is the exact
lookup chain SPEC-4 performs, so it is the highest-value integration assertion in Phase 5.

---

## 8. Acceptance criteria

- **A1.** `cargo check --workspace --all-features` and `cargo check -p bevy_ecs
  --no-default-features --features std` both pass.
- **A2.** `bevy_ecs::reflect` exports `ReflectFromTemplate`, `ReflectTemplate`,
  `ReflectRelationshipTarget`, `FromReflectError`, `try_from_reflect_with_fallback`; the first three
  are also in `bevy_ecs::prelude`.
- **A3.** `ReflectFromTemplate`/`ReflectTemplate` field names and types match SPEC-0 Contract A
  **as amended by §7** exactly: `template_type_id: TypeId` + `template_type_path: &'static str`;
  `output_type_id: TypeId` + `build_template: fn(&dyn Reflect, &mut TemplateContext) ->
  Result<Box<dyn Reflect>, BevyError>`. `ReflectRelationshipTarget` (renamed per §7) has exactly
  the five Contract B fields, including the `unsafe fn` pointer shapes, which are byte-for-byte
  compatible with `RelatedResolvedScenes`' fields (`resolved_scene.rs:654-659`) so SPEC-2 can
  assign them directly.
- **A3b.** For every registered `ReflectTemplate`, the value returned by `build_template` has
  `Any::type_id() == output_type_id` (SPEC-4 pre-resolves `ReflectComponent` from that id).
- **A4.** `ReflectComponentFns::push_to_bundle_writer` matches Contract A's signature exactly,
  and `ReflectComponent::push_to_bundle_writer` exposes it.
- **A5.** No `panic!`, `unwrap`, or `expect` is reachable from `push_to_bundle_writer`,
  `try_from_reflect_with_fallback`, or `ReflectTemplate::build_template` for any input.
  (Verify by reading; `#[deny(clippy::unwrap_used)]` is not enabled crate-wide.)
- **A6.** All tests in §7 pass.
- **A7.** `--release` `breakout` example binary grows by ≤ 1.5 %, measured and recorded.
- **A8.** Every new public item has rustdoc; every new `unsafe` block and public `unsafe fn` has
  a `# Safety` / `// SAFETY:` comment (CI lint `missing_docs` + `clippy::undocumented_unsafe_blocks`).
- **A9.** `Children` carries `ReflectRelationshipTarget` and (transitively) `ReflectComponent` after
  `TypeRegistry::register::<Children>()`.
- **A10.** `Handle<A>` carries `ReflectFromTemplate` and `HandleTemplate<A>` carries
  `ReflectTemplate` after `register_asset_reflect::<A>()`.
- **A11 (Phase 5).** Every `#[template(reflect)]` template type carries `ReflectFromReflect`,
  `ReflectDefault` and `ReflectTemplate` once registered; `EntityTemplate`, `OptionTemplate<T>`
  and `VecTemplate<T>` are `Reflect`; every seed-set component's
  `ReflectFromTemplate::template_type_id` resolves to a registration carrying `ReflectTemplate`
  and `ReflectDefault`.

---

## 9. Open questions

*(OQ-1 — strengthened `T::Template: Reflect` bound; OQ-2 — `template_type_path`; OQ-3 — rename
to `ReflectRelationshipTarget`; OQ-4 — Phase 5 as a series prerequisite: all four were
**ACCEPTED** in SPEC-0 §7 and are now normative in §4.4, §4.4.1, §4.5 and §4.9. The former OQ-5
(`EntityTemplate` reflection) is resolved by Phase 5 Step 11b. The three below remain open.)

1. **No `FromWorld` in `push_to_bundle_writer`.** The `&mut World` is unavailable while a
   `ComponentsRegistrator` is borrowed, so the fallback ladder is FromReflect → Default only.
   Confirm acceptable (I believe it is: the dynamic path always builds concrete values from a
   `ReflectDefault` template, so the ladder is a safety net). If not, Contract A's signature
   would need `&mut World` instead of `&mut ComponentsRegistrator`, which breaks the
   single-archetype-move property.
2. **Binary-size fallback design.** If A7's 1.5 % budget is exceeded, replace the monomorphized
   `push_component::<C>` with a fully erased push: look up `ComponentInfo::layout()` for the
   registered id, `Box::into_raw` the built value, `push_component_by_id(id, OwningPtr::new(ptr),
   layout)`, then deallocate the box's allocation **without** dropping its contents. That reduces
   the per-type code to `register_component::<C>()` plus the downcast, at the cost of more
   `unsafe`. Not chosen by default because the extra `unsafe` is harder to review than a 1 %
   binary-size delta.
3. **`HasWindows` / `OnMonitor`.** The only other production relationship pair in the repo is
   not `Reflect` at all (`crates/bevy_window/src/monitor.rs:60`,
   `crates/bevy_window/src/window.rs:1562`). Making it reflectable would let `.bsn` express
   `HasWindows [ ... ]`, but is unrelated to this series' goals. Proposed: leave alone, file a
   separate issue.
