# SPEC-5: `DynamicBsnLoader`, plugin wiring, feature flags, and examples

**Status: DRAFT (conforms to SPEC-0, provides Contract F).**
Target: `/home/tony/workspace/bevy`, `main`, `0.20.0-dev`.
Depends on: SPEC-3 (Contract D — `BsnDocument` + parser, now the standalone `crates/bevy_bsn`
workspace crate per the ratified SPEC-0 §3), SPEC-4 (Contract E — `DynamicScene` +
`DynamicScene::from_document`). Consumed by: SPEC-6 (hot reload & e2e validation).
Revised after review: incorporates the SPEC-0 §7 ratified amendments (Contract E signature,
no associated consts, load-failure reporting in scope, self-include as a load error,
`ResolveContext::type_registry`).

All `file:line` references below were read against the working tree at commit `25368b78c`.

---

## 1. Goals

1. `asset_server.load::<ScenePatch>("scenes/player.bsn")` works out of the box for an app with
   `DefaultPlugins`, producing a `ScenePatch` that resolves and spawns through the *existing*
   spawn pipeline with no new systems.
2. `bsn! { :"scenes/player.bsn" ... }` (macro-side inheritance from an asset) works, with
   field-level merging against the loaded asset.
3. A `.bsn` file may itself inherit from another `.bsn` file (`:"base.bsn"`), with correct
   dependency-readiness gating and correct resolve *ordering* (base resolved before derived).
4. Every failure mode is a *logged error with file:line:column*, never a panic.
5. The feature is behind a default-on cargo feature `bsn_asset` that can be compiled out.
6. A first-party example (`examples/scene/dynamic_bsn.rs` + `assets/scenes/*.bsn`) demonstrates
   both spawn-from-asset and macro-side inheritance, and doubles as the acceptance gate for the
   whole series.

## 2. Non-goals

- Everything in SPEC-0 §2 (imports, functions, closures/observers, `SceneComponent` invocation,
  write-back, catalogs, binary format, reconciliation).
- **Labeled sub-assets** (`player.bsn#Head`). A `.bsn` file produces exactly one `ScenePatch`.
  See §5.4.
- **Multi-root `.bsn` files** producing a `SceneListPatch`. v1 requires exactly one root
  entity per file; more is a loader error. See §5.4.
- Loader `Settings` (issue #24415). v1 uses `type Settings = ()`. See §4.5.
- **Associated constants** in `.bsn` values (`Color::WHITE`, `Val::ZERO`) — unsupported in v1
  per SPEC-0 §7; paths resolve to unit structs and unit enum variants only.
- Hot reload — SPEC-6 (which re-runs this loader on change; SPEC-5 stores no reload state).
- Multi-file include *cycle* detection; only self-include is caught here (§4.3.1).
- Asset processing (`AssetProcessor` / `.meta` transforms) for `.bsn`.

## 3. Background (existing code this spec builds on)

- **The `AssetLoader` trait** (`crates/bevy_asset/src/loader.rs:32-52`):

  ```rust
  pub trait AssetLoader: TypePath + Send + Sync + 'static {
      type Asset: Asset;
      type Settings: Settings + Default + Serialize + for<'a> Deserialize<'a>;
      type Error: Into<BevyError>;
      fn load(&self, reader: &mut dyn Reader, settings: &Self::Settings,
              load_context: &mut LoadContext)
          -> impl ConditionalSendFuture<Output = Result<Self::Asset, Self::Error>>;
      fn extensions(&self) -> &[&str] { &[] }
  }
  ```

  Note `Self::Error: Into<BevyError>` — any `thiserror` enum that is `Error + Send + Sync +
  'static` qualifies.
- **Canonical loader pattern** (`bevy_world_serialization/src/world_asset_loader.rs:18-34`):
  a `#[derive(Debug, TypePath)]` struct holding `TypeRegistryArc`, built by `FromWorld` from
  `world.resource::<AppTypeRegistry>().0.clone()`, registered with `init_asset_loader`.
- **The proven `ScenePatch` loader shape** (`bevy_scene/src/lib.rs:1135-1151`,
  `benches/benches/bevy_scene/spawn.rs:406-428`): `Ok(ScenePatch::load_with(load_context, scene))`.
  `ScenePatch::load_with` (`scene_patch.rs:41-53`) calls `Scene::register_dependencies` and
  turns each `SceneDependency` into an `UntypedHandle` via
  `LoadFromPath::load_from_path_erased`, storing them in the `#[dependency]` field
  (`scene_patch.rs:24-25`). `impl LoadFromPath for LoadContext<'_>` is at
  `bevy_asset/src/reflect.rs:406-414`.
- **Nested loads do not rewrite paths**: `LoadBuilder::load_internal`
  (`bevy_asset/src/loader_builders.rs:224-254`) uses the given `AssetPath` verbatim and inserts
  the resulting index into `LoadContext::dependencies`. `.bsn` include paths are therefore
  asset-source-root-relative, exactly like `bsn!` `:"..."` paths — nothing in the stack does
  "relative to the including file" resolution.
- **`LoadedWithDependencies` is recursive**: emitted only when `loading_rec_deps` is empty
  (`bevy_asset/src/server/info.rs:489-496`, plus the "dependent became ready" path at
  `info.rs:588-593`), so dependencies always get their event *before* their dependents.
- **Resolve/spawn pipeline** (`bevy_scene/src/spawn.rs:607-671, 695-788`), registered in
  `ScenePlugin::build` (`lib.rs:943-958`) with `resolve_scene_patches` `.chain()`ed *before*
  `spawn_queued`.
- **`CachedSceneAsset`** (`bevy_scene/src/scene.rs:404-442`): `resolve` requires
  `AssetServer::get_handle::<ScenePatch>(path)` to be `Some` *and* the patch to be in
  `Assets<ScenePatch>`; `register_dependencies` registers `ScenePatch` at that path.
  Copy-on-write merging (`resolved_scene.rs:467-491`) reads `cached_patch.resolved`, so **the
  base must already be resolved when the derived scene resolves** or the derived templates are
  built from `Default` and overwrite the base's fields; `ResolvedSceneRoot::apply`
  (`resolved_scene.rs:234-248`) errors outright if the cached patch is missing or unresolved.
- **`AssetServer::add`** (`bevy_asset/src/server/mod.rs:1014-1016`) routes through
  `LoadedAsset::new_with_dependencies` (`loader.rs:163`), so even manually-added `ScenePatch`
  assets participate in dependency gating and receive `LoadedWithDependencies`. *Verified
  empirically* with a throwaway integration test: `world.queue_spawn_scene(bsn! { :"a.bsn"
  Marker Position { x: 1. } })` against a not-yet-loaded `a.bsn` spawns correctly once the
  asset lands, with `x == 1.` (derived) and `y == 2.` (base) — field merging works.
- **Prior art**: `scratchpad/dynamic_bsn.rs:100-194` (pcwalton's `DynamicBsnLoader`,
  `AppTypeRegistry`-holding, old `ScenePatch { scene: Box<dyn Scene> }` shape).

---

## 4. Detailed design

### 4.1 Files created / modified

| File | Action |
| --- | --- |
| `crates/bevy_bsn/**` | **new crate, owned by SPEC-3** — consumed here as an optional dependency (§4.8); no SPEC-5 edits |
| `crates/bevy_scene/src/dynamic/loader.rs` | **new** — `DynamicBsnLoader`, `DynamicBsnLoaderError`, `decode_bsn_source`, `check_single_root`, `check_no_self_include`, `report_scene_patch_load_failures`, unit tests |
| `crates/bevy_scene/src/dynamic/mod.rs` | modified (owned by SPEC-4) — add `mod loader; pub use loader::*;` |
| `crates/bevy_scene/src/lib.rs` | modified — `#[cfg(feature = "bsn_asset")] mod dynamic;`, `ScenePlugin` registration, prelude, docs at `:863-882` and `:388` |
| `crates/bevy_scene/Cargo.toml` | modified — new `[features]` section |
| `crates/bevy_scene/macros/src/lib.rs` | modified — remove the "not yet implemented" warning at `:51` |
| `crates/bevy_scene/src/spawn.rs` | modified — doc-comment cleanup (9 sites, §7.2) |
| `crates/bevy_internal/Cargo.toml` | modified — `default-features = false` on `bevy_scene`, new `bsn_asset` feature |
| `Cargo.toml` (root) | modified — `bsn_asset` feature, `scene` collection, `[[example]]` block |
| `docs/cargo_features.md` | regenerated |
| `examples/scene/dynamic_bsn.rs` | **new** |
| `assets/scenes/dynamic_bsn_example.bsn` | **new** |
| `assets/scenes/dynamic_bsn_button.bsn` | **new** |
| `crates/bevy_scene/tests/dynamic_bsn.rs` | **new** — integration tests |
| `benches/benches/bevy_scene/spawn.rs` | modified — one new bench group (§9.4) |

`dynamic/` is SPEC-4's module; SPEC-5 adds one file to it. If SPEC-4 names the module
differently, the loader file moves with it — this is the only coupling.

### 4.2 `DynamicBsnLoader` and `FromWorld`

```rust
// crates/bevy_scene/src/dynamic/loader.rs

use crate::{dynamic::{DynamicScene, DynamicSceneBuildError}, ScenePatch};
use bevy_asset::{io::Reader, AssetLoadFailedEvent, AssetLoader, AssetPath, LoadContext};
use bevy_bsn::{BsnDocument, BsnNodeKind, LineCol};
use bevy_ecs::{message::MessageReader, reflect::AppTypeRegistry, world::{FromWorld, World}};
use bevy_reflect::TypePath;
use thiserror::Error;
use tracing::error;

/// An [`AssetLoader`] for `.bsn` files, producing a [`ScenePatch`].
///
/// Registered automatically by [`ScenePlugin`](crate::ScenePlugin) when the `bsn_asset`
/// cargo feature is enabled (it is on by default).
///
/// Every type named in a `.bsn` file must be registered in the [`AppTypeRegistry`]
/// (`#[derive(Reflect)]` types are registered automatically when the `reflect_auto_register`
/// feature is on; otherwise call `App::register_type`).
#[derive(Debug, TypePath)]
pub struct DynamicBsnLoader {
    type_registry: AppTypeRegistry,
}

impl FromWorld for DynamicBsnLoader {
    fn from_world(world: &mut World) -> Self {
        DynamicBsnLoader {
            type_registry: world.resource::<AppTypeRegistry>().clone(),
        }
    }
}
```

**`AppTypeRegistry` vs `TypeRegistryArc` (decision — revised by SPEC-0 §7).** Store
`AppTypeRegistry` (pcwalton's shape, `dynamic_bsn.rs:101-111`), **not** the bare
`TypeRegistryArc` of `world_asset_loader.rs:19-34`. Rationale:

- **The ratified Contract E entry point takes `&AppTypeRegistry`**:
  `DynamicScene::from_document(document, source, registry: &AppTypeRegistry)`. Storing the
  `TypeRegistryArc` would force the loader to rebuild an `AppTypeRegistry` wrapper on every
  load (legal — the field is `pub` — but pointless indirection). This is the one place SPEC-5
  deviates from the `world_asset_loader.rs` precedent, and it is driven by the callee's
  signature, not by preference.
- Behavior is otherwise identical: `AppTypeRegistry(pub TypeRegistryArc)` is
  `#[derive(Resource, Clone, Default)]` (`bevy_ecs/src/reflect/mod.rs:35-36`), so the clone
  aliases the same `Arc<RwLock<TypeRegistry>>`. **Types registered after `ScenePlugin::build`
  runs are therefore visible to the loader** — essential, because user `App::register_type`
  calls and later plugins' registrations all happen after `ScenePlugin` is built.
- `world.resource::<AppTypeRegistry>()` panics if the resource is absent. Acceptable and
  precedented: `AppTypeRegistry` is inserted by `App::default()`
  (`crates/bevy_app/src/app.rs:113-119`). Apps constructed with `App::empty()` must insert it
  before adding `ScenePlugin`; document this in the `ScenePlugin` docs.

### 4.3 `AssetLoader` impl

```rust
impl AssetLoader for DynamicBsnLoader {
    type Asset = ScenePatch;
    type Settings = ();
    type Error = DynamicBsnLoaderError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &Self::Settings,
        load_context: &mut LoadContext<'_>,
    ) -> Result<Self::Asset, Self::Error> {
        let path = load_context.path().to_string();

        // 1. bytes
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|source| DynamicBsnLoaderError::Io { path: path.clone(), source })?;

        // 2. UTF-8 (BOM policy in `decode_bsn_source`)
        let source = decode_bsn_source(&bytes, &path)?;

        // 3. parse (SPEC-3, the standalone `bevy_bsn` crate)
        let document = bevy_bsn::parse(source).map_err(|error| {
            let LineCol { line, column } = error.span().line_col(source);
            DynamicBsnLoaderError::Parse { path: path.clone(), line, column,
                                           message: error.message().to_string() }
        })?;

        // 4. document-level checks the builder cannot make (it does not know our own path)
        check_single_root(&document, source, &path)?;
        check_no_self_include(&document, source, &path, load_context.path())?;

        // 5. build (SPEC-4, ratified Contract E signature)
        let scene = DynamicScene::from_document(&document, path.clone(), &self.type_registry)
            .map_err(|error| DynamicBsnLoaderError::from_build_error(&path, source, error))?;

        // 6. dependencies + asset value
        Ok(ScenePatch::load_with(load_context, scene))
    }

    fn extensions(&self) -> &[&str] {
        &["bsn"]
    }
}
```

Notes a junior implementer must not get wrong:

1. **`from_document` takes no `LoadFromPath`.** The ratified Contract E entry point is
   `DynamicScene::from_document(document: &BsnDocument, source: impl Into<Arc<str>>,
   registry: &AppTypeRegistry) -> Result<DynamicScene, DynamicSceneBuildError>`. Asset
   dependencies accumulate *inside* the `DynamicScene` and reach the asset server exactly once,
   through `DynamicScene::register_dependencies` → `ScenePatch::load_with` (step 6). Do **not**
   pass `load_context` into the builder and do **not** register dependencies from the loader:
   a second registration path would desynchronise `ScenePatch::dependencies`, which SPEC-6's
   hot reload diffs. (This supersedes SPEC-5's earlier draft and closes cross-spec note X1.)
2. `source` (the second argument) is the **asset path string**, used by SPEC-4 to build the
   asset-based `SceneEntityReference` identity (`SceneEntityReferenceSource::Asset { path_hash }`,
   Contract C ratified). Pass `path.clone()` — the same string used in diagnostics — so that
   identities are stable across reloads of the same file and distinct across files.
3. There is **no registry guard in the loader** any more: `from_document` takes
   `&AppTypeRegistry` and does its own `read()` internally. The loader's only `.await` is
   `read_to_end` in step 1, so the returned future stays `ConditionalSendFuture`. If a future
   refactor ever holds a `RwLockReadGuard` in this function, it must not span an `.await`.
4. Step 6 must come after step 5 (both need `&mut LoadContext`/the built scene).
5. `ScenePatch::resolved` is left `None`; `resolve_scene_patches` fills it (§5.1).

#### 4.3.1 Document-level checks

```rust
/// v1 restriction: one `.bsn` file describes exactly one root entity (§5.4).
fn check_single_root(document: &BsnDocument, source: &str, path: &str)
    -> Result<(), DynamicBsnLoaderError>
{
    if document.roots.len() > 1 {
        let LineCol { line, column } = document.node(document.roots[1]).span.line_col(source);
        return Err(DynamicBsnLoaderError::MultipleRoots {
            path: path.to_string(), line, column, count: document.roots.len(),
        });
    }
    Ok(())
}

/// Rejects `a.bsn` whose base is `a.bsn`. Such a file would never finish loading, because its
/// own recursive-dependency state can never reach `Loaded` (§5.3).
fn check_no_self_include(
    document: &BsnDocument,
    source: &str,
    path: &str,
    own_path: &AssetPath<'static>,
) -> Result<(), DynamicBsnLoaderError> {
    for node in &document.nodes {
        let BsnNodeKind::Entity { base: Some(base), .. } = &node.kind else { continue };
        // `bevy_bsn` has no `bevy_asset` types, so the base is a plain `String` here; the
        // loader is the layer that gives it asset-path meaning.
        if AssetPath::from(base.as_str()) == *own_path {
            let LineCol { line, column } = node.span.line_col(source);
            return Err(DynamicBsnLoaderError::SelfInclude {
                path: path.to_string(), line, column,
            });
        }
    }
    Ok(())
}
```

Scope of the self-include check (normative, per SPEC-0 §7): it compares the **literal**
`AssetPath` of each base against `load_context.path()`. It catches the common copy-paste
mistake (`a.bsn` containing `:"a.bsn"`), including source-qualified spellings, because
`AssetPath::from` parses the `source://path#label` form on both sides. It does **not** catch
non-canonical spellings of the same file (`"./a.bsn"`), and it deliberately does not catch
multi-file cycles (`a` → `b` → `a`), which stay deferred as Q3. `AssetPath` is `Eq + Hash`, so
the comparison is a cheap string compare; the walk is O(nodes) over an already-parsed document.
If SPEC-3 stores bases only on root nodes, the loop degenerates to a single check.

### 4.4 Text decoding and BOM policy

```rust
/// Decodes `.bsn` source bytes as UTF-8, stripping a leading UTF-8 BOM.
fn decode_bsn_source<'a>(bytes: &'a [u8], path: &str)
    -> Result<&'a str, DynamicBsnLoaderError>
{
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Err(DynamicBsnLoaderError::UnsupportedEncoding { path: path.to_string() });
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    core::str::from_utf8(bytes).map_err(|error| DynamicBsnLoaderError::InvalidUtf8 {
        path: path.to_string(),
        valid_up_to: error.valid_up_to(),
    })
}
```

Policy (normative):

- `.bsn` files are UTF-8. A **UTF-8 BOM is silently stripped** (Windows editors emit it), and
  all spans/line-columns are computed against the *stripped* source. Consequence: a diagnostic
  on line 1 of a BOM'd file reports the column as if the BOM were absent, which matches what
  the user sees in their editor.
- UTF-16 BOMs are rejected with a dedicated message rather than a confusing
  "invalid utf-8 sequence at byte 1".
- Invalid UTF-8 anywhere else is rejected, reporting `valid_up_to`.
- No other encodings, no `\r\n` normalization (the lexer treats `\r` as whitespace — SPEC-3
  requirement, restated here because it affects column numbers).

### 4.5 `Settings`

**Decision: `type Settings = ();` for v1.**

`AssetLoader::Settings` must be `Settings + Default + Serialize + Deserialize`
(`loader.rs:36`), which forces `serde` bounds on any custom type. `()` satisfies them and is
what `WorldAssetLoader` (`world_asset_loader.rs:52`), `FakeSceneLoader` (`lib.rs:1141`), and
pcwalton's loader (`dynamic_bsn.rs:152`) all use.

Issue #24415 ("asset loader settings in BSN") asks for the *inverse* capability — being able
to specify per-handle loader settings for handles referenced *from* a `.bsn` file (e.g.
`image: "x.png"` with `ImageLoaderSettings`). That is a value-coercion feature in SPEC-4's
`ReflectConvert`/`HandleTemplate` path, not a `DynamicBsnLoader::Settings` feature, and
nothing in this spec forecloses it: `HandleTemplate::Path` could grow a settings-carrying
variant later. Recorded as open question Q1.

### 4.6 `DynamicBsnLoaderError`

```rust
/// Errors produced by [`DynamicBsnLoader`].
///
/// Messages are formatted as `path:line:column: message` so that editors and terminals can
/// jump to the offending location. Lines and columns are 1-based; columns count `char`s.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DynamicBsnLoaderError {
    /// The asset bytes could not be read.
    #[error("{path}: failed to read `.bsn` file: {source}")]
    Io { path: String, #[source] source: std::io::Error },

    /// The file is not valid UTF-8.
    #[error("{path}: `.bsn` files must be UTF-8; invalid byte sequence at offset {valid_up_to}")]
    InvalidUtf8 { path: String, valid_up_to: usize },

    /// The file begins with a UTF-16 byte order mark.
    #[error("{path}: `.bsn` files must be UTF-8, but this file starts with a UTF-16 byte order mark")]
    UnsupportedEncoding { path: String },

    /// The file could not be parsed.
    #[error("{path}:{line}:{column}: {message}")]
    Parse { path: String, line: u32, column: u32, message: String },

    /// The parsed document could not be turned into a scene (unregistered type, missing
    /// `ReflectDefault`, unknown field, un-coercible value, ...).
    #[error("{path}:{line}:{column}: {source}")]
    Build { path: String, line: u32, column: u32, #[source] source: DynamicSceneBuildError },

    /// As [`Self::Build`], for errors that carry no span.
    #[error("{path}: {source}")]
    BuildNoSpan { path: String, #[source] source: DynamicSceneBuildError },

    /// The document has more than one root entity (not supported in this release).
    #[error("{path}:{line}:{column}: a `.bsn` file must contain exactly one root entity, found \
             {count}. Wrap them in a single root, or split them into separate files.")]
    MultipleRoots { path: String, line: u32, column: u32, count: usize },

    /// The file inherits from itself (`a.bsn` containing `:"a.bsn"`).
    #[error("{path}:{line}:{column}: this `.bsn` file inherits from itself. A scene cannot be \
             its own base; remove the `:\"{path}\"` include.")]
    SelfInclude { path: String, line: u32, column: u32 },
}

impl DynamicBsnLoaderError {
    fn from_build_error(path: &str, source: &str, error: DynamicSceneBuildError) -> Self {
        match error.span() {
            Some(span) => {
                let LineCol { line, column } = span.line_col(source);
                Self::Build { path: path.to_string(), line, column, source: error }
            }
            None => Self::BuildNoSpan { path: path.to_string(), source: error },
        }
    }
}
```

`MultipleRoots` and `SelfInclude` are raised by the two document-level checks in §4.3.1, which
run after parsing and before `DynamicScene::from_document`.

**Note on `Parse`:** the variant stores a rendered `message: String` rather than
`#[source] BsnParseError`. That is deliberate — `bevy_bsn::BsnParseError` is a `no_std` error
type from a crate that knows nothing about asset paths, and flattening it here keeps the
whole diagnostic on one `path:line:column: …` line (the format editors and CI grep for). If a
consumer needs the structured error, it is reachable from the parse site; nothing downstream
of the loader inspects it. Same reasoning for `MultipleRoots`/`SelfInclude`, which are
loader-level (asset-layer) conditions the parser cannot express.

**Span → line/column conversion lives in `bevy_bsn`** (ratified SPEC-0 §3: the parser is a
standalone crate, and a third-party CLI/LSP/exporter needs this without Bevy):

```rust
// crates/bevy_bsn/src/span.rs   (SPEC-3 owns this; SPEC-5 only consumes it)
pub struct Span { pub start: u32, pub end: u32 }       // byte offsets into the source
pub struct LineCol { pub line: u32, pub column: u32 }  // both 1-based
impl Span {
    /// 1-based line and char-counted column of this span's start within `source`.
    pub fn line_col(&self, source: &str) -> LineCol;
}
```

Reference implementation, restated so SPEC-3 and SPEC-5 agree on the exact semantics
(clamping, char boundaries, char-counted columns) — cross-spec note X2:

```rust
impl Span {
    pub fn line_col(&self, source: &str) -> LineCol {
        let mut offset = (self.start as usize).min(source.len());
        while !source.is_char_boundary(offset) { offset -= 1; }
        let before = &source[..offset];
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        LineCol {
            line: before.matches('\n').count() as u32 + 1,
            column: source[line_start..offset].chars().count() as u32 + 1,
        }
    }
}
```

Because this now ships in `bevy_bsn`, SPEC-5 has **no fallback copy** — if `Span::line_col` is
missing, SPEC-5 does not compile, which is the desired coupling.

### 4.7 Plugin wiring

```rust
// crates/bevy_scene/src/lib.rs
#[cfg(feature = "bsn_asset")]
mod dynamic;
#[cfg(feature = "bsn_asset")]
pub use dynamic::*;

impl Plugin for ScenePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<QueuedScenes>()
            .init_resource::<WaitingScenes>()
            .init_asset::<ScenePatch>()
            .init_asset::<SceneListPatch>()
            .add_systems(/* unchanged */)
            .add_observer(on_add_scene_patch_instance);

        #[cfg(feature = "bsn_asset")]
        app.init_asset_loader::<DynamicBsnLoader>()
            .add_systems(
                SpawnScene,
                report_scene_patch_load_failures
                    .in_set(SceneSpawnerSystems::SceneSpawn)
                    .before(resolve_scene_patches),
            );
    }
}
```

**Ordering / prerequisites — no new constraints.** `ScenePlugin::build` already calls
`init_asset::<ScenePatch>()`, whose impl does `self.world().resource::<AssetServer>()`
(`bevy_asset/src/lib.rs:656-660`) and therefore **already panics if `AssetPlugin` has not been
added first**. `init_asset_loader` (`bevy_asset/src/lib.rs:650-653`) →
`register_asset_loader` → `world().resource::<AssetServer>()` (`lib.rs:610-614`) has exactly
the same requirement, so adding it changes nothing observable. Every existing test/bench
already adds `AssetPlugin` before `ScenePlugin` (`lib.rs:982-987`, `spawn.rs:880-884`,
`benches/.../spawn.rs:389-392`).

The only *new* prerequisite is `AppTypeRegistry`, present in any `App::new()`/`App::default()`
(`bevy_app/src/app.rs:113-119`). Add one line to the `ScenePlugin` doc comment:

> Requires [`AssetPlugin`] to be added first. When the `bsn_asset` feature is enabled, also
> requires the `AppTypeRegistry` resource (present in every `App::new()`).

`DynamicBsnLoader` is added to `bevy_scene::prelude` guarded by `#[cfg(feature = "bsn_asset")]`
so that users can `register_asset_loader` it manually in custom `App`s.

### 4.7.1 Load-failure reporting (ratified, was Q5)

Without this, a `.bsn` that fails to load is reported once by the asset server and then
`ScenePatchInstance` entities pointing at it sit in `WaitingScenes` forever with no indication
that *scene spawning* is what broke (§10.1). Add one system, feature-gated with the loader,
in `crates/bevy_scene/src/dynamic/loader.rs`:

```rust
/// Logs an error for every [`ScenePatch`] asset that fails to load, including `.bsn` parse and
/// resolution diagnostics.
///
/// Added by [`ScenePlugin`](crate::ScenePlugin) when the `bsn_asset` feature is enabled.
pub fn report_scene_patch_load_failures(
    mut failures: MessageReader<AssetLoadFailedEvent<ScenePatch>>,
) {
    for failure in failures.read() {
        error!(
            "Failed to load scene asset \"{}\": {}. Entities whose `ScenePatchInstance` points \
             at this asset will never be spawned.",
            failure.path, failure.error
        );
    }
}
```

- **Where**: `SpawnScene`, in `SceneSpawnerSystems::SceneSpawn`, `.before(resolve_scene_patches)`
  — the same schedule the rest of the pipeline runs in, so the log line appears adjacent to any
  resolve errors for the same frame. It reads only a `MessageReader`, so it has no ambiguity
  conflicts with `resolve_scene_patches`/`spawn_queued`; the explicit `.before` only fixes the
  log ordering.
- **Dedup**: `MessageReader` holds a per-system cursor, so **each failure event is logged
  exactly once**, not once per frame, even though `Messages` retains events for two frames.
  A retry (or a hot-reload attempt in SPEC-6) emits a *new* `AssetLoadFailedEvent`, which is
  logged again — that is intended, because the file changed.
- `AssetLoadFailedEvent<ScenePatch>` is written by the asset server for every typed asset
  (`bevy_asset/src/server/mod.rs:203-212`), so this also covers `ScenePatch`es produced by
  non-`.bsn` loaders. Gating it on `bsn_asset` is a scope choice, not a technical requirement.
- `error!` comes from `tracing`, already a `bevy_scene` dependency
  (`crates/bevy_scene/Cargo.toml:25`) and already used by `spawn.rs`.

### 4.8 Cargo feature plumbing

Since SPEC-0 §3 was ratified, the parser/AST/printer lives in the **standalone workspace crate
`crates/bevy_bsn`** (zero bevy dependencies, `no_std + alloc`, default `std` feature), so
`bsn_asset` is now an *optional-dependency* feature rather than a pure code gate.

**`crates/bevy_scene/Cargo.toml`** — new `[features]` section (the crate currently has none)
plus one optional dependency:

```toml
[features]
default = ["bsn_asset"]

# Enables the `.bsn` asset format: pulls in the standalone `bevy_bsn` parser crate and
# registers `DynamicBsnLoader` from `ScenePlugin`.
bsn_asset = ["dep:bevy_bsn"]

[dependencies]
bevy_bsn = { path = "../bevy_bsn", version = "0.20.0-dev", optional = true }
# ... existing unconditional deps unchanged
```

**Precedent** for exactly this shape (optional in-repo path dependency activated by a named
feature via `dep:`): `crates/bevy_picking/Cargo.toml:12` (`mesh_picking = ["dep:bevy_mesh",
"dep:crossbeam-channel"]`) with `crates/bevy_picking/Cargo.toml:22`
(`bevy_mesh = { path = "../bevy_mesh", version = "0.20.0-dev", optional = true }`).

No other dependency changes: `bevy_asset`, `bevy_reflect`, and `thiserror` are already
unconditional dependencies (`crates/bevy_scene/Cargo.toml:15,20,24`).

**Workspace membership — nothing to do.** The workspace `members` list already globs
`"crates/*"` (root `Cargo.toml:17-19`), so `crates/bevy_bsn` becomes a member as soon as
SPEC-3 creates the directory. (SPEC-3 owns the new crate's `[package]` metadata, lints table,
and `no_std` feature set.)

**`crates/bevy_internal/Cargo.toml`** — the new crate is reached **only transitively through
`bevy_scene`**; `bevy_internal` gets no direct `bevy_bsn` dependency. Two edits:

```toml
# line 571, add `default-features = false` so `bsn_asset` is opt-in at this level
bevy_scene = { path = "../bevy_scene", optional = true, version = "0.20.0-dev", default-features = false }
```

```toml
# in [features], mirroring `mesh_picking` at crates/bevy_internal/Cargo.toml:352
bsn_asset = ["bevy_scene", "bevy_scene/bsn_asset"]
```

**Root `Cargo.toml`** — mirroring `mesh_picking` at root `Cargo.toml:270`:

```toml
# next to the other "Provides ..." features (~line 326)
# Provides the `.bsn` scene asset format and its asset loader
bsn_asset = ["bevy_internal/bsn_asset"]
```

```toml
# COLLECTION: Features used to compose Bevy scenes.   (line 169)
scene = ["bevy_world_serialization", "bevy_scene", "bsn_asset"]
```

`bevy_internal`'s `bsn_asset` lists `bevy_scene` first, so enabling `bsn_asset` alone is never
a silent no-op (same construction as `mesh_picking = ["bevy_picking", "bevy_picking/mesh_picking"]`).
Because the `scene` collection is part of the `2d`, `3d`, and `ui` profiles (root
`Cargo.toml:139,142,150`), `.bsn` loading is on for every default user, while
`bevy = { default-features = false, features = ["bevy_scene"] }` gets the scene system
**without compiling `bevy_bsn` at all** — which is the point of making it an optional
dependency rather than a `#[cfg]` gate.

**`docs/cargo_features.md`** is generated — after editing the root manifest run:

```text
cargo run -p build-templated-pages -- update features
```

which appends the `bsn_asset` row (from the `# Provides ...` doc comment) and updates the
`scene` collection row's feature set.

---

## 5. Interoperability: exact user-facing behavior

### 5.1 Spawning a `.bsn` asset directly

```rust
fn setup(mut commands: Commands, assets: Res<AssetServer>) {
    commands.spawn(ScenePatchInstance(assets.load("scenes/player.bsn")));
}
```

Frame-by-frame trace:

1. `AssetServer::load` picks the loader by extension `bsn` → `DynamicBsnLoader`; the load runs
   on the async task pool and returns `ScenePatch { scene: Some(Box::new(DynamicScene)),
   dependencies: [...], resolved: None }`. Dependencies are (a) every asset-path string typed
   as a `Handle` in the document and (b) every `:"base.bsn"` include.
2. `ScenePatchInstance`'s `Add` observer `on_add_scene_patch_instance` (`spawn.rs:695-705`)
   pushes `(entity, handle)` into `QueuedScenes::new_scene_entities`.
3. In `SpawnScene`, `QueuedScenes::spawn_queued` (`spawn.rs:805-826`) finds `resolved == None`
   and parks the entity in `WaitingScenes::scene_entities[handle]`.
4. Once the asset *and all recursive dependencies* have loaded, the server emits
   `AssetEvent::LoadedWithDependencies` (`server/info.rs:489-496`).
5. `resolve_scene_patches` (`spawn.rs:615-627`) takes `patch.scene`, calls
   `ResolvedSceneRoot::resolve`, stores `Arc<ResolvedSceneRoot>` in `patch.resolved`. On error
   it logs `Failed to resolve scene {id}: {err}` and the entity stays parked forever.
   Per SPEC-2's ratified C-7, this system gains a `Res<AppTypeRegistry>` parameter and
   populates `ResolveContext::type_registry: Option<&TypeRegistry>` from it, so that typed
   `bsn!` patches layered over a dynamic base can recover the base's field values via
   `ReflectFromReflect` instead of resetting the slot to `Default`. SPEC-2 owns that change;
   SPEC-5 requires only that the `.bsn` load path always runs with `Some(registry)`.
6. `spawn_queued`, chained *after* `resolve_scene_patches` (`lib.rs:949-955`), reads the same
   event (`spawn.rs:719-737`), pops the waiting entities and calls `resolved.apply(...)` —
   **the same frame** as the resolve.
7. Entities added *after* the asset is loaded skip the waiting list: step 3 finds `resolved`
   and applies immediately (`spawn.rs:807-818`).

No new systems, no new events. `ScenePatchInstance` stays on the entity, which is what SPEC-6
uses to find instances to refresh.

### 5.2 `bsn! { :"player.bsn" }` inheritance

The macro lowers `:"player.bsn"` to `CachedSceneAsset("player.bsn")` (`scene.rs:404-442`).
Its `register_dependencies` registers a `ScenePatch` dependency at that path
(`scene.rs:439-441`); its `resolve` requires the handle to exist *and* the patch to be in
`Assets<ScenePatch>` (`scene.rs:428-436`), erroring with
`ResolveSceneError::MissingSceneDependency` otherwise.

Two supported call styles:

```rust
// Preferred: waits for the asset.
commands.queue_spawn_scene(bsn! {
    :"scenes/player.bsn"
    Health { current: 80.0 }
    Children [ Sword, Shield ]
});

// Immediate: only valid once "scenes/player.bsn" is fully loaded; otherwise returns
// Err(MissingSceneDependency).
world.spawn_scene(bsn! { :"scenes/player.bsn" Health { current: 80.0 } })?;
```

`queue_spawn_scene` (`spawn.rs:195-206`) calls `ScenePatch::load(assets, scene)` (registering
the `.bsn` dependency) and then `AssetServer::add`, which goes through
`LoadedAsset::new_with_dependencies` (`server/mod.rs:1014-1016`, `loader.rs:163`). The
manually-added patch therefore participates in dependency gating exactly like a loaded one and
receives `LoadedWithDependencies` when `player.bsn` finishes — the same steps 5-7 as §5.1.
*(Empirically verified; see §3.)*

**Path identity.** The string in `bsn! { :"scenes/player.bsn" }` and the string passed to
`assets.load("scenes/player.bsn")` must be byte-identical: `CachedSceneAsset::resolve` does an
`AssetServer::get_handle::<ScenePatch>(&path)` lookup keyed on `AssetPath`. Since nothing in
the stack rewrites relative paths (`loader_builders.rs:224-234`), paths are always
source-root-relative. Document this in the `.bsn` docs section: **`.bsn` include paths are
relative to the asset root, not to the including file.**

**Merge semantics.** With the base resolved, `ResolvedScene::get_or_insert_erased_template`
clones the base's template (copy-on-write, `resolved_scene.rs:467-491`) before applying the
derived patch, so unspecified fields keep the base's values and specified fields win. This is
exactly the behavior the empirical probe confirmed (`x` from the derived scene, `y` from the
base). It is also why resolve *ordering* (§5.3) matters. This is *the* case that needs SPEC-2's
ratified C-7: the cached slot holds a **dynamic** template built by the loader, and the
derived `bsn!` patch asks for it as a **typed** `T`; both `World::spawn_scene` and
`queue_spawn_scene`'s eventual `resolve_scene_patches` therefore populate
`ResolveContext::type_registry` from `AppTypeRegistry`, letting `get_or_insert_template::<T>`
recover the base's values through `ReflectFromReflect` rather than falling back to
`T::default()` with an `error!`. The apply half of the pipeline (`spawn_queued`,
`ResolvedSceneRoot::apply`) does not resolve anything and needs no registry.

### 5.3 Nested `.bsn` includes

`a.bsn` starting with `:"b.bsn"`, and `b.bsn` starting with `:"c.bsn"`:

- **Dependency chain**: `a`'s `ScenePatch::dependencies` contains `b`'s handle (registered by
  `DynamicScene::register_dependencies` via `ScenePatch::load_with`); `b`'s contains `c`'s.
- **Readiness**: `LoadedWithDependencies` uses *recursive* dependency state
  (`server/info.rs:489`), so `a`'s event cannot fire before `b`'s, which cannot fire before
  `c`'s.
- **Ordering**: `resolve_scene_patches` iterates `MessageReader<AssetEvent<ScenePatch>>` in
  emission order (`spawn.rs:615`), so `c` resolves, then `b` (whose `CachedSceneAsset` finds
  `c.resolved`), then `a`. Copy-on-write merging is therefore correct at every level, and the
  `apply`-time requirement that the cached patch be resolved (`resolved_scene.rs:242-248`) is
  satisfied.
- **Diamonds** (`a` → `b`, `a` → `c`, both → `d`) are fine: `d` is one asset, resolved once,
  and both `b` and `c` reference the same handle.
- **Self-include** (`a.bsn` containing `:"a.bsn"`) is rejected at load time with
  `DynamicBsnLoaderError::SelfInclude` (§4.3.1) — ratified in SPEC-0 §7 because it is the
  common accidental case and is cheap to detect from `load_context.path()`.
- **Multi-file cycles** (`a.bsn` → `b.bsn` → `a.bsn`) are *not* detected: recursive dependency
  loading never completes, so neither asset emits `LoadedWithDependencies`, neither resolves,
  and waiting entities silently never spawn. No hang, no panic, no crash — but no diagnostic
  either. Deferred; open question Q3 (a detector would walk `ScenePatch::dependencies`, which
  SPEC-6 already indexes for reload).
- **Ordering constraint inside a file**: `include_cached` errors with
  `CachedSceneError::LateCached` if templates were added before the include
  (`resolved_scene.rs:556-561`). SPEC-4 must emit the base include first (Contract E), so a
  `.bsn` grammar that only allows `:"..."` as the *first* entry (Contract D) keeps this
  impossible to violate. The loader does not need to check it, but the resulting error, if it
  ever occurs, surfaces through step 6 of §5.1 as `Failed to resolve scene {id}: ...`.

### 5.4 Labeled sub-assets and multi-root files: OUT OF SCOPE for v1

**A `.bsn` file maps to exactly one `ScenePatch`, with no labeled sub-assets.**

- `LoadContext::labeled_asset_scope` / `add_labeled_asset` / `get_label_handle`
  (`bevy_asset/src/loader.rs:459-475, 620-630`) are **not** used by `DynamicBsnLoader`.
- `player.bsn#Head` is not addressable. `AssetServer::load` on a labeled `.bsn` path fails
  with the standard "does not contain the labeled asset" error
  (`server/mod.rs:2422-2426`), which is an acceptable message.
- A document with `document.roots.len() > 1` is rejected with
  `DynamicBsnLoaderError::MultipleRoots` rather than silently loading the first root. (SPEC-3's
  grammar permits multi-root documents so the parser stays a superset; the *loader* imposes the
  restriction, so lifting it later is purely additive.)

Rationale: `ScenePatch::scene` is a single `Option<Box<dyn Scene>>` (`scene_patch.rs:22`); the
multi-entity asset type is `SceneListPatch` (`scene_patch.rs:113-125`), which needs its own
`extensions()` mapping or a labeled convention, and jackdaw's catalog work (#23648) will want
a *stable naming* scheme for sub-scenes rather than whatever `#Name` happens to appear. Picking
a label convention now would very likely be wrong. Recorded as open question Q2.

---

## 6. Example

### 6.1 `assets/scenes/dynamic_bsn_button.bsn` (the base, included by the macro)

```text
// A reusable button. Included from Rust with `bsn! { :"scenes/dynamic_bsn_button.bsn" }`
// and from other `.bsn` files with `:"scenes/dynamic_bsn_button.bsn"`.
bevy_ui::Node {
    width: bevy_ui::Val::Px(180.0),
    height: bevy_ui::Val::Px(56.0),
    justify_content: bevy_ui::JustifyContent::Center,
    align_items: bevy_ui::AlignItems::Center,
}
bevy_ui::BackgroundColor(bevy_color::Color::Srgba(bevy_color::Srgba {
    red: 0.15, green: 0.15, blue: 0.15, alpha: 1.0,
}))
Children [
    (#Label bevy_ui::widget::Text("Button"))
]
```

### 6.2 `assets/scenes/dynamic_bsn_example.bsn` (the root scene, loaded as an asset)

```text
// Loaded with `asset_server.load::<ScenePatch>("scenes/dynamic_bsn_example.bsn")`.
#Root
bevy_ui::Node {
    width: bevy_ui::Val::Percent(100.0),
    height: bevy_ui::Val::Percent(100.0),
    flex_direction: bevy_ui::FlexDirection::Column,
    align_items: bevy_ui::AlignItems::Center,
    justify_content: bevy_ui::JustifyContent::Center,
    row_gap: bevy_ui::Val::Px(16.0),
}
Children [
    (
        #Logo
        bevy_ui::widget::ImageNode { image: "branding/bevy_logo_dark.png" }
        bevy_ui::Node { width: bevy_ui::Val::Px(320.0) }
    ),
    (
        #Title
        bevy_ui::widget::Text("Loaded from dynamic_bsn_example.bsn")
        bevy_ui::widget::TextColor(bevy_color::Color::Srgba(bevy_color::Srgba {
            red: 0.95, green: 0.95, blue: 0.95, alpha: 1.0,
        }))
    ),
]
```

**No associated consts.** Per SPEC-0 §7, associated constants (`Color::WHITE`,
`BorderRadius::MAX`, `Val::ZERO`, …) are **not supported in `.bsn` in v1**: SPEC-4 resolves a
bare path to a unit struct or a unit enum variant only, and Rust consts are not reachable
through the type registry. Every color in the example is therefore written as an explicit
`Color::Srgba(Srgba { … })` struct literal with numeric fields — which is also a better
demonstration of nested value construction. Reviewers of these `.bsn` files must reject any
re-introduction of a const path.

Coverage checklist (each item is an acceptance requirement on SPEC-3/SPEC-4):

| Feature | Where |
| --- | --- |
| Fully-qualified component paths | every component above |
| Partial struct fields | `Node { width, height, ... }` (12+ fields left at default) |
| Enum unit variants | `AlignItems::Center`, `JustifyContent::Center`, `FlexDirection::Column` |
| Enum tuple variant with a struct payload | `Color::Srgba(Srgba { red: …, green: …, blue: …, alpha: … })` |
| Tuple struct patch | `BackgroundColor(...)`, `Text("...")`, `TextColor(...)` |
| Nested struct value inside a tuple patch | the `Srgba { … }` payloads |
| `Children` nesting with parenthesized entities | both files |
| `#Name` | `#Root`, `#Logo`, `#Title`, `#Label` |
| Asset-path `Handle` field (string → `HandleTemplate::Path`) | `ImageNode { image: "branding/bevy_logo_dark.png" }` |

The types used require SPEC-1 type data (`ReflectFromTemplate` / `ReflectTemplate` /
`ReflectRelationshipTarget`) on: `Node`, `BackgroundColor`, `Text`, `TextColor`, `ImageNode`,
`Children`; `Color` and `Srgba` must be registered with `ReflectDefault`. Listed here
explicitly so SPEC-1/SPEC-4 know the example's exact surface.

### 6.3 `examples/scene/dynamic_bsn.rs`

```rust
//! Demonstrates loading scenes from `.bsn` asset files.
//!
//! This example shows two ways to use a `.bsn` file:
//!
//! 1. Loading it directly as a [`ScenePatch`] asset and spawning it with
//!    [`ScenePatchInstance`].
//! 2. Inheriting from it inside a [`bsn!`] macro with `:"path.bsn"`, overriding individual
//!    fields.
//!
//! Note the difference in what the two forms accept: the `bsn!` macro below uses Rust
//! function calls (`px(24)`, `Color::srgb(...)`) and could use constants like `Color::WHITE`,
//! while a `.bsn` file — which is data, not code — supports neither, and spells the same
//! values out as struct literals (`Color::Srgba(Srgba { red: 0.95, ... })`).
//!
//! Requires the `bsn_asset` cargo feature (enabled by default).

use bevy::prelude::*;
use bevy::scene::ScenePatchInstance;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn(Camera2d);

    // 1. Spawn the `.bsn` file directly. The entity gets its components as soon as the
    //    asset and all of its dependencies (here: the logo image) have loaded.
    commands.spawn(ScenePatchInstance(
        asset_server.load("scenes/dynamic_bsn_example.bsn"),
    ));

    // 2. Inherit from a `.bsn` file in Rust, overriding the background color and the label.
    //    `queue_spawn_scene` waits until "scenes/dynamic_bsn_button.bsn" is loaded.
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
```

The contrast is deliberate and is called out in the example's doc comment: `px(24)`,
`Color::srgb(...)` (function calls) and associated consts (`Color::WHITE`) are legal in `bsn!`
and *illegal* in `.bsn`.

### 6.4 Root `Cargo.toml` registration (insert after the `bsn` example block, ~line 3147)

```toml
[[example]]
name = "dynamic_bsn"
path = "examples/scene/dynamic_bsn.rs"
doc-scrape-examples = true
required-features = ["bsn_asset"]

[package.metadata.example.dynamic_bsn]
name = "Dynamic BSN"
description = "Demonstrates loading scenes from `.bsn` asset files"
category = "Scene"
wasm = true
```

---

## 7. Documentation updates

### 7.1 `crates/bevy_scene/src/lib.rs:863-882` — replace the whole "## .bsn Asset Format" section

Release-notes-style replacement text:

> ## .bsn Asset Format
>
> Scenes can be authored on disk as `.bsn` files and loaded like any other asset:
>
> ```ignore
> commands.spawn(ScenePatchInstance(asset_server.load("scenes/player.bsn")));
> ```
>
> A `.bsn` file uses the same syntax as the [`bsn!`] macro, minus everything that requires
> compiling Rust: no `{ expr }` blocks, no `on(...)` observers, no `template(...)` closures,
> no function scene includes, and no Rust constants. What remains — components with full or
> registry-resolvable type paths, partial struct and tuple patches, enum variants, `#Name`,
> `Children [ ... ]` and other relationship targets, asset paths as `Handle` fields, and
> `:"other.bsn"` inheritance — behaves identically to the macro. A `.bsn` scene and a `bsn!`
> scene that patch the same component merge field by field, in either direction.
>
> Values in a `.bsn` file are literals and struct/enum literals only. Rust constants and
> associated constants (`Color::WHITE`, `Val::ZERO`) are **not** supported yet — write the
> value out instead (`Color::Srgba(Srgba { red: 1.0, green: 1.0, blue: 1.0, alpha: 1.0 })`).
>
> Every type named in a `.bsn` file must be registered in the type registry and must derive
> [`Reflect`] with `#[reflect(Component)]` (plus `#[reflect(Default)]` on any type whose
> fields you patch). Asset paths inside a `.bsn` file are relative to the asset root, not to
> the file that names them. Each `.bsn` file describes exactly one root entity and cannot
> inherit from itself; labeled sub-assets (`file.bsn#Label`) are not supported yet.
>
> Loading is provided by `DynamicBsnLoader`, registered by [`ScenePlugin`] when the
> `bsn_asset` cargo feature is enabled (it is on by default; it is included in the `scene`
> feature collection). Parse and resolution failures are reported as asset load errors with
> `file:line:column` locations and never panic; a failed scene asset is also logged once by
> `report_scene_patch_load_failures`, naming the entities that will never spawn as a result.
>
> The `.bsn` grammar itself — lexer, parser, AST, and printer — lives in the standalone
> [`bevy_bsn`] crate, which depends on no other Bevy crate and builds on `no_std`. Third-party
> tooling (asset pipelines, editor plugins, exporters from other DCC tools) can read and write
> `.bsn` files by depending on `bevy_bsn` alone, without pulling in the engine.
>
> See the `dynamic_bsn` example for a complete walkthrough.

Also update `lib.rs:388` — delete "Note that the `.bsn` file format is not yet released. (This
already works, assuming theres a loader for the asset format)".

### 7.2 Warning removals

- `crates/bevy_scene/macros/src/lib.rs:51`: drop
  `<div class="warning">Asset format not yet implemented!</div>` from the `:"scene.bsn"` row.
  **Keep** line 52's warning (caching for scene *function* includes really is unimplemented).
- `crates/bevy_scene/src/spawn.rs`: remove the nine "the `.bsn` file format is not yet
  released" notes at lines 23, 87, 109, 169, 238, 286, 321, 354, 466, 571 (doc comments and
  doc-example comments only; no code changes).

---

## 8. Step-by-step implementation plan

Each step compiles and passes `cargo test -p bevy_scene` on its own.

0. **Prerequisite:** `crates/bevy_bsn` exists (SPEC-3) and `cargo check -p bevy_bsn` passes.
   It joins the workspace automatically via the `"crates/*"` glob (root `Cargo.toml:17-19`).
1. **Feature scaffolding.** Add `[features] default = ["bsn_asset"]`,
   `bsn_asset = ["dep:bevy_bsn"]` and the optional `bevy_bsn` path dependency to
   `crates/bevy_scene/Cargo.toml` (§4.8). Add `crates/bevy_scene/src/dynamic/loader.rs`
   containing only the module doc comment; wire `#[cfg(feature = "bsn_asset")] mod dynamic;`
   in `lib.rs` (or extend SPEC-4's existing `dynamic/mod.rs`). Verify `cargo check -p bevy_scene`
   and `cargo check -p bevy_scene --no-default-features` both pass — the latter must not build
   `bevy_bsn` at all (check with `cargo tree -p bevy_scene --no-default-features | grep bevy_bsn`
   returning nothing).
2. **Error type + decoding.** Implement `DynamicBsnLoaderError` and `decode_bsn_source`. Add
   unit tests §9.1 items 1-3. No `AssetLoader` yet. (`Span::line_col` comes from `bevy_bsn`;
   SPEC-5 writes no line/column helper of its own.)
3. **Loader skeleton.** Implement `DynamicBsnLoader` + `FromWorld` + `AssetLoader` with a body
   that reads bytes, decodes, and returns `ScenePatch::load_with(load_context, bsn!())` (an
   empty scene). This compiles before SPEC-3/4 land and proves the trait/lifetime shape.
4. **Plugin registration + failure reporting.** Add the `#[cfg]`-gated
   `init_asset_loader::<DynamicBsnLoader>()` and `report_scene_patch_load_failures` (§4.7.1) to
   `ScenePlugin::build`, plus prelude export and the `ScenePlugin` doc note. Add integration
   test `loads_empty_bsn_file` (§9.2 item 1) — an empty `.bsn` now round-trips.
5. **Wire the parser (`bevy_bsn`).** Replace the stub body with `bevy_bsn::parse` plus the two
   document-level checks of §4.3.1 (`check_single_root`, `check_no_self_include`). Errors now
   report `path:line:column`. Add unit tests §9.1 items 4-7.
6. **Wire the builder (SPEC-4).** Replace the stub scene with
   `DynamicScene::from_document(&document, path.clone(), &self.type_registry)`. Add integration
   tests §9.2 items 2-6.
7. **Bevy-level feature plumbing.** `crates/bevy_internal/Cargo.toml` (`default-features =
   false` on `bevy_scene`, new `bsn_asset` feature), root `Cargo.toml` (`bsn_asset` feature,
   `scene` collection). Verify with
   `cargo check -p bevy --no-default-features --features bevy_scene` (loader absent) and
   `cargo check -p bevy --no-default-features --features scene` (loader present). Regenerate
   `docs/cargo_features.md`.
8. **Example.** Add the two `.bsn` files, `examples/scene/dynamic_bsn.rs`, and the root
   `Cargo.toml` blocks. `cargo run --example dynamic_bsn` must show the logo, the title, and
   the overridden button.
9. **Docs.** Apply §7.1 and §7.2. Run `cargo doc -p bevy_scene` and the doc tests.
10. **Bench.** Add the `.bsn` bench group (§9.4).

---

## 9. Test plan

### 9.1 Unit tests — `crates/bevy_scene/src/dynamic/loader.rs`, `mod tests`

These need no `App`: they exercise decoding and error rendering directly.

1. `decode_strips_utf8_bom` — `decode_bsn_source(b"\xEF\xBB\xBFHello", "a.bsn")` returns
   `"Hello"`.
2. `decode_rejects_utf16_bom` — `b"\xFF\xFENothing"` returns `UnsupportedEncoding`, and its
   `to_string()` contains "must be UTF-8".
3. `decode_rejects_invalid_utf8` — `b"ok\xC3"` returns `InvalidUtf8 { valid_up_to: 2, .. }`.
4. `parse_error_message_is_file_line_column` — construct
   `DynamicBsnLoaderError::Parse { path: "scenes/x.bsn".into(), line: 12, column: 5,
   message: "expected`}`".into() }` and assert
   `to_string() == "scenes/x.bsn:12:5: expected`}`"`.
5. `multiple_roots_error_mentions_count` — the `MultipleRoots` display contains "exactly one
   root entity" and the count.
6. `self_include_error_mentions_the_path` — the `SelfInclude` display contains "inherits from
   itself" and the offending path.
7. `loader_extensions` — `DynamicBsnLoader { type_registry: AppTypeRegistry::default() }
   .extensions() == &["bsn"]`.

`Span::line_col` correctness (1-based lines, char-counted columns, clamping at a non-char
boundary) is tested **in `bevy_bsn`** by SPEC-3, not here — SPEC-5 only asserts that the
rendered loader message carries the numbers through (item 4).

### 9.2 Integration tests — new file `crates/bevy_scene/tests/dynamic_bsn.rs`

All use the **in-memory asset source** pattern proven at `crates/bevy_scene/src/lib.rs:1113-1128`
and `benches/benches/bevy_scene/spawn.rs:399-404`:

```rust
fn test_app(dir: Dir) -> App {
    let mut app = App::new();
    let dir_clone = dir.clone();
    app.register_asset_source(
        AssetSourceId::Default,
        AssetSourceBuilder::new(move || Box::new(MemoryAssetReader { root: dir_clone.clone() })),
    );
    app.add_plugins((TaskPoolPlugin::default(), AssetPlugin::default(), ScenePlugin));
    app.register_type::<Position>();   // and any other test component
    app.finish();
    app.cleanup();
    app
}
// files are added with `dir.insert_asset_text(Path::new("a.bsn"), SOURCE)`
// (`crates/bevy_asset/src/io/memory.rs:53-55`)
// and the app is pumped with the `run_app_until` fork at `crates/bevy_scene/src/lib.rs:2524`.
```

Note: the asset source must be registered **before** `AssetPlugin`
(`bevy_asset/src/lib.rs:452-455` logs an error otherwise).

1. `loads_empty_bsn_file` — a comment-only file loads to a `ScenePatch` whose `resolved` is
   `Some` after `run_app_until(asset_server.is_loaded(&handle))`. (If SPEC-3 rejects empty
   documents, this asserts the parse error instead.)
2. `spawns_scene_patch_instance_from_bsn` — `Position { x: 1.0, y: 2.0 }`; spawn
   `ScenePatchInstance(handle)` *before* the load finishes; assert
   `Position { x: 1.0, y: 2.0, z: 0.0 }` (z proves partial patching).
3. `spawns_children_and_names_from_bsn` — `Children [ (#X), (#Y Position { x: 3. }) ]`; assert
   two children in document order with `Name` "X"/"Y" and the second's `x == 3.0`.
4. `bsn_macro_inherits_from_bsn_asset` — the analogue of `loaded_asset_cached_patching`
   (`lib.rs:1084-1190`) with **real `.bsn` text** instead of `FakeSceneLoader`: `a.bsn` =
   `Position { y: 2. } Children [ (#X) ]`, then
   `world.queue_spawn_scene(bsn! { :"a.bsn" Position { x: 1. } Children [ (#Y) ] })`; assert
   `(1.0, 2.0, 0.0)` and children `["X", "Y"]` in that order.
5. `nested_bsn_includes_resolve_base_first` — `c.bsn` = `Position { z: 3. }`, `b.bsn` =
   `:"c.bsn" Position { y: 2. }`, `a.bsn` = `:"b.bsn" Position { x: 1. }`; spawn
   `ScenePatchInstance(load("a.bsn"))`; assert `(1.0, 2.0, 3.0)`. Regression test for §5.3.
6. `asset_path_field_becomes_dependency` — a `Handle` field set to a string path; assert
   `ScenePatch::dependencies` is non-empty and `AssetServer::get_handle` for that path is `Some`.
7. `parse_error_reports_line_and_column` — malformed source; assert
   `asset_server.load_state(&handle)` is `Failed` and the error string contains `"bad.bsn:2:"`.
8. `unregistered_type_reports_error_with_location` — `.bsn` naming `my_game::NotRegistered`;
   assert failure, and that the message names the type path and a line:column.
9. `multiple_roots_rejected` — two top-level entities; failure message contains "exactly one
   root entity".
10. `spawn_after_load_applies_immediately` — pre-load and await, *then* spawn
    `ScenePatchInstance`; components present after one `app.update()` (covers the non-waiting
    branch at `spawn.rs:807-818`).
11. `self_include_rejected` — `a.bsn` whose first entry is `:"a.bsn"`; assert
    `load_state(&handle)` is `Failed` and the rendered error contains "inherits from itself".
    Also assert the app does not hang (bounded `run_app_until`), which is the behavior the
    check exists to guarantee.
12. `failed_load_is_reported_once` — the load-failure system of §4.7.1. Load a malformed
    `bad.bsn`, then pump the app for 10 frames with a test-local
    `MessageReader<AssetLoadFailedEvent<ScenePatch>>` draining into a `Vec`; assert **exactly
    one** event is observed across all frames and that its `error.to_string()` contains
    `"bad.bsn:"`. This pins the message-cursor semantics the system relies on (one log line per
    failure, not one per frame); the emitted `error!` text itself is not asserted, since
    `bevy_scene` has no log-capture harness.

### 9.3 Feature-matrix checks (CI)

- `cargo check -p bevy_scene --no-default-features` — crate compiles without the loader, and
  `DynamicBsnLoader` is absent from the public API. Additionally
  `cargo tree -p bevy_scene --no-default-features` must not list `bevy_bsn`: with
  `bsn_asset = ["dep:bevy_bsn"]` the parser crate is not merely `#[cfg]`-ed out, it is not
  compiled.
- `cargo test -p bevy_scene` — default features.
- `cargo check -p bevy --no-default-features --features bevy_scene` and
  `--features scene` (§8 step 7).

### 9.4 Bench addition — `benches/benches/bevy_scene/spawn.rs`

Add a `dynamic_bsn` group next to the existing scene benches, reusing
`in_memory_asset_source` (`:399-404`) and `bench_app` (`:381-397`):

- `dynamic_bsn/load` — time `AssetServer::load` + `run_app_until(is_loaded)` for a ~100-entity
  `.bsn` file (parse + build + resolve).
- `dynamic_bsn/spawn` — with the asset pre-loaded and resolved, time spawning N
  `ScenePatchInstance` entities, to compare against the existing `bsn!` spawn benches and prove
  the dynamic path costs nothing extra *at spawn time* (both go through the same
  `ResolvedSceneRoot::apply`).

The bench file needs **no `bevy_bsn` import**: it feeds `.bsn` *text* through the in-memory
asset source and the registered `DynamicBsnLoader`, so `bevy_bsn` arrives transitively through
`bevy_scene`'s `bsn_asset` feature. (A separate parser-only microbenchmark, if wanted, belongs
in `crates/bevy_bsn` and is SPEC-3's call.)

`FakeSceneLoader` (`:406-428`) stays: it is still the right tool for benches that want to
isolate resolve/spawn from parsing. The benches crate depends on `bevy_scene` with default
features (`benches/Cargo.toml:27`), so `bsn_asset` is available.

---

## 10. Edge cases, error handling, and failure UX

### 10.1 Malformed `.bsn` file

The loader returns `Err`. The asset server wraps it as
`AssetLoaderError { path, loader_name, error }` whose `Display` is
`Failed to load asset '{path}' with asset loader '{loader_name}': {error}`
(`server/mod.rs:2436`), logs it at `error!`, and emits `AssetLoadFailedEvent<ScenePatch>` plus
`UntypedAssetLoadFailedEvent` (`server/mod.rs:203-212`). The user sees, verbatim:

```text
ERROR bevy_asset::server: Failed to load asset 'scenes/player.bsn' with asset loader
'bevy_scene::dynamic::loader::DynamicBsnLoader': scenes/player.bsn:7:14: expected `}`, found `,`
```

The path appears twice (once from the asset server's wrapper, once from our `file:line:column`
prefix). That is intentional: the `path:line:column:` prefix is the grep/editor-jumpable form,
and terminals and CI logs are where this is read.

**Consequence for waiting entities:** a failed load never emits `LoadedWithDependencies`, so
entities with a `ScenePatchInstance` pointing at it stay in `WaitingScenes` forever and simply
never gain components. `report_scene_patch_load_failures` (§4.7.1, ratified) adds a second,
scene-specific line naming the consequence:

```text
ERROR bevy_scene: Failed to load scene asset "scenes/player.bsn": Failed to load asset
'scenes/player.bsn' with asset loader 'bevy_scene::dynamic::loader::DynamicBsnLoader':
scenes/player.bsn:7:14: expected `}`, found `,`. Entities whose `ScenePatchInstance` points at
this asset will never be spawned.
```

Also document it in the `ScenePatchInstance` docs, pointing users at
`AssetLoadFailedEvent<ScenePatch>` / `AssetServer::load_state` for programmatic handling.
Naming the specific stranded entities is still not done (it would require indexing
`WaitingScenes` by path); left as a follow-up, not an open question.

### 10.2 Unregistered / non-reflectable type

Produced by SPEC-4's builder (`ResolveSceneError::TypeNotRegistered` and friends, Contract C.5)
and surfaced as `DynamicBsnLoaderError::Build` with `file:line:column`. Required message
content (SPEC-4 owns the wording; SPEC-5 requires these elements):

```text
scenes/player.bsn:3:1: `my_game::Health` is not registered in the type registry. Add
`#[derive(Reflect)]` and `#[reflect(Component)]` to the type, and register it with
`app.register_type::<my_game::Health>()`.
```

### 10.3 Feature disabled

With `bsn_asset` off, no loader claims the `bsn` extension, so `AssetServer::load` fails with
`MissingAssetLoaderForExtensionError` — `no`AssetLoader`found for the following extension(s): bsn`
(`server/mod.rs:2465-2468`). The `bsn!` macro's `:"file.bsn"` include still compiles and still
registers the dependency, so the same message appears at load time rather than at spawn time.
Mention the feature name in the `.bsn` docs section (§7.1) so the search for that message lands
on the fix. We deliberately do **not** register a stub loader that returns "enable the
`bsn_asset` feature" — a `#[cfg(not(feature))]` stub would have to be `TypePath` + `Asset`-typed
and would confuse `AssetProcessor`; the standard message plus documentation is enough.

### 10.4 Other edge cases

| Case | Behavior |
| --- | --- |
| Zero-byte file | Whatever SPEC-3's grammar says for an empty document; if empty documents are legal, the result is a `ScenePatch` with no templates that applies nothing. |
| File with only comments/whitespace | Same as above. |
| Extension casing (`.BSN`) | `AssetServer` extension matching is case-sensitive; `.BSN` gets no loader. Not handled. |
| `.bsn` loaded as the wrong asset type (`load::<Image>`) | Standard asset-server type-mismatch error; no special handling. |
| A `.bsn` file whose `Handle` field points at a missing asset | The `.bsn` itself loads; the dependency fails, so recursive dependency state never becomes `Loaded`, so `LoadedWithDependencies` never fires and the scene never resolves. The missing dependency is logged by the asset server. Same behavior as `bsn!` today. |
| Very large file | No limits imposed; parsing happens once on the async task pool. |
| Concurrent loads of the same path | Handled by `AssetServer` (single load per path). |
| Registry lock poisoning | Owned by SPEC-4 now (`from_document` does the `read()`); bevy's `TypeRegistryArc::read` uses `PoisonError::into_inner`, so no handling is needed. |
| Associated const in a value (`Color::WHITE`) | Unsupported in v1: SPEC-4 resolves the path as a unit struct/variant, fails to find it, and returns a `TypeNotRegistered`-class error with `file:line:column`. |
| `a.bsn` containing `:"a.bsn"` | Load-time `SelfInclude` error (§4.3.1). |
| `a.bsn` containing `:"./a.bsn"` | *Not* caught (non-canonical spelling); degenerates into the deferred cycle case — never resolves, no diagnostic (Q3). |

---

## 11. Acceptance criteria

1. `cargo test -p bevy_scene` passes with all tests in §9.1 and §9.2.
2. `cargo check -p bevy_scene --no-default-features` passes with `DynamicBsnLoader` absent from
   the public API, and `cargo tree -p bevy_scene --no-default-features` does not list
   `bevy_bsn` (the parser crate is not compiled at all).
3. `cargo run --example dynamic_bsn` renders the logo image, the title text, and the
   overridden button, with no errors or warnings in the log.
4. Test 9.2.4 (`bsn_macro_inherits_from_bsn_asset`) demonstrates field-level merging between a
   `.bsn` asset and a `bsn!` macro scene in both directions of specificity.
5. Test 9.2.5 (`nested_bsn_includes_resolve_base_first`) passes, proving three-level include
   chains resolve in dependency order.
6. Every failure path in §10 produces a logged error containing the asset path, and no test
   run produces a panic from `bevy_scene`'s dynamic path.
7. A failed `.bsn` load produces exactly one `report_scene_patch_load_failures` line per
   failure event (test 9.2.12), and a self-including file fails fast rather than hanging
   (test 9.2.11).
8. Neither `.bsn` example file contains an associated const; the docs state the restriction.
9. `docs/cargo_features.md` is regenerated and contains the `bsn_asset` row.
10. `crates/bevy_scene/src/lib.rs`'s ".bsn Asset Format" section and
    `macros/src/lib.rs:51` no longer claim the format is unimplemented.

---

## 12. Cross-spec notes

- **X1 — build entry point — RESOLVED (SPEC-0 §7).** Ratified as
  `DynamicScene::from_document(document: &BsnDocument, source: impl Into<Arc<str>>,
  registry: &AppTypeRegistry) -> Result<DynamicScene, DynamicSceneBuildError>`, with no
  `LoadFromPath` parameter: dependencies accumulate inside the `DynamicScene` and reach the
  asset server exactly once, via `register_dependencies` → `ScenePatch::load_with`. §4.3 is
  written against this signature, and §4.2 changed the stored registry to `AppTypeRegistry` to
  match it. Remaining requirement on SPEC-4: `register_dependencies` must report **every** asset
  path in the document (handle-valued fields *and* `:"base.bsn"` includes), because
  `ScenePatch::dependencies` is both the readiness gate and the set SPEC-6's hot reload diffs.
- **X2 — public surface SPEC-5 consumes from `bevy_bsn` (to SPEC-3).** Exactly:
  `bevy_bsn::parse(&str) -> Result<BsnDocument, BsnParseError>`; `BsnParseError::span()` and
  `::message()`; `Span::line_col(&self, source: &str) -> LineCol` with 1-based line and
  char-counted 1-based column (reference implementation and clamping semantics in §4.6);
  `BsnDocument::roots`, `::nodes`, `::node(BsnNodeId)`; `BsnNode::span`, `::kind`;
  `BsnNodeKind::Entity { base: Option<String>, .. }`. All must be reachable with the crate's
  default features (SPEC-5 does not enable any `bevy_bsn` feature explicitly). Because the
  crate is now a hard dependency of the `bsn_asset` feature, SPEC-5 keeps **no fallback copy**
  of any of it.
- **X3 — build errors must carry spans (to SPEC-4).** `DynamicSceneBuildError` (or whatever
  SPEC-4 names it) must expose `fn span(&self) -> Option<Span>` pointing at the offending
  node, or every resolution error degrades to a whole-file diagnostic
  (`DynamicBsnLoaderError::BuildNoSpan`).
- **X4 — one root per file, and base spans (to SPEC-3).** The grammar may permit multi-root
  documents; the loader rejects them (§5.4). SPEC-3 must keep `BsnDocument::roots` a `Vec`,
  provide a span for `roots[1]`, and give each `Entity` node's `base` a reachable span — the
  self-include check (§4.3.1) reports the *entity* node's span, so an entity span is the
  minimum; a base-specific span (ratified in SPEC-0 §7 for Contract D) gives a better caret.
- **X5 — hot reload (to SPEC-6) — updated.** SPEC-0 §7 removed `reload_source`: the asset
  server replaces the whole `ScenePatch` on reload, so the reloaded value already carries a
  fresh `scene: Some(_)` and `LoadedWithDependencies` re-fires. SPEC-5 therefore stores nothing
  extra and needs no change for reload; the loader is simply re-run. The only property SPEC-6
  depends on from here is that `source` (the path string passed to `from_document`) is
  identical across reloads of the same file, keeping `SceneEntityReferenceSource::Asset`
  identities stable.
- **X6 — `ResolveContext::type_registry` (to SPEC-2).** SPEC-5's interop flows (§5.1, §5.2)
  assume `resolve_scene_patches` and the `World::spawn_scene` / `queue_spawn_scene` entry points
  populate `ResolveContext::type_registry` from `AppTypeRegistry` (ratified C-7). Without it,
  a typed `bsn!` patch over a dynamic `.bsn` base silently resets the slot to `T::default()`
  (with an `error!`), which would break test 9.2.4's merge assertions.

## 13. Open questions

- **Q1 — Loader settings (#24415).** v1 ships `type Settings = ()`. Do we eventually want
  per-`.bsn` loader settings (e.g. a "strict types" toggle, or a default asset source for
  relative paths)? And separately, how do we let a `.bsn` file specify *settings for the assets
  it references*? The latter probably belongs in `HandleTemplate`, not here.
- **Q2 — Labeled sub-assets / multi-root files (#23648, jackdaw catalogs).** jackdaw's editor
  work wants many named scenes per file (a "catalog"). Options: (a) `#Name`-derived labels,
  (b) an explicit label syntax, (c) load multi-root files as `SceneListPatch` under a reserved
  label. This must be settled before anyone ships tooling that assumes one file = one scene.
- **Q3 — Multi-file include cycles (partially resolved).** Self-include is now a load-time
  error (§4.3.1, ratified). Cycles spanning two or more files, and non-canonical spellings of
  the same path (`"./a.bsn"`), still deadlock silently (§5.3). Should a detector walk
  `ScenePatch::dependencies` (which SPEC-6 already indexes) and report the cycle?
- **~~Q4 — Associated consts~~ — RESOLVED (SPEC-0 §7):** unsupported in v1. Paths resolve to
  unit structs and unit enum variants only. The example files (§6.1, §6.2) use explicit struct
  literals, the docs state the restriction (§7.1), and the example's doc comment contrasts it
  with `bsn!` (§6.3). Revisit only if `bevy_reflect` gains const reflection.
- **~~Q5 — Stranded-entity diagnostics~~ — RATIFIED IN SCOPE:** implemented as
  `report_scene_patch_load_failures` (§4.7.1), tested by 9.2.12. The residual sub-question —
  naming the *specific* stranded entities, which needs `WaitingScenes` indexed by asset path —
  is a follow-up, not a blocker.
- **~~Q6 — Separate the parser from the loader~~ — RESOLVED (SPEC-0 §3):** the parser, AST and
  printer ship as the standalone `crates/bevy_bsn` workspace crate with zero bevy dependencies
  (Kneelawk's request in #23576, comment 4375795637), so external tooling depends on
  `bevy_bsn` directly and no second `bevy_scene` feature is needed. `bsn_asset` is now an
  optional-dependency feature (`["dep:bevy_bsn"]`), so disabling it removes the parser from the
  build entirely rather than `#[cfg]`-ing it out.
- **Q7 (new) — `bevy_bsn` release/publish plumbing.** A new published crate needs a
  `[package]` block, README, and a slot in whatever ordering the release process uses, plus
  possibly a `deny.toml`/CI crate-list entry. Workspace membership itself is automatic
  (`"crates/*"`, root `Cargo.toml:17-19`). SPEC-3 owns this; flagged here because SPEC-5's
  feature plumbing is what first makes the crate reachable from `bevy`.
