# SPEC-3: `bevy_bsn` — the `.bsn` text format, lexer, parser, AST and printer

**Status: NORMATIVE for Contract D.** Conforms to `SPEC-0-master.md`. Provides Contract D
(parser + AST + printer) consumed by SPEC-4 (resolution) and SPEC-5 (loader).

Deliverable: a **standalone workspace crate `crates/bevy_bsn`** with **zero dependencies on
any other bevy crate**, `no_std + alloc`, independently publishable to crates.io
(ratified in SPEC-0 §3 per Kneelawk's request in bevyengine/bevy#23576, comment
4375795637: "an official parsing library that does not bring in the rest of Bevy's
infrastructure as a dependency … tools that are separate from the Bevy engine that can read
and write BSN asset files, like a Blender plugin, or a Unity plugin"). `bevy_scene` is the
reference consumer, not the owner.

Target: `/home/tony/workspace/bevy`, Bevy `main`, `0.20.0-dev`.

---

## 1. Goals

1. Define the **complete `.bsn` text grammar** in EBNF, precise enough to implement without
   consulting the `bsn!` macro sources.
2. Define a **hand-written, allocation-free lexer** and a **recursive-descent parser** producing
   the Contract D plain-value AST with stable `BsnNodeId`s.
3. Guarantee the asset grammar is a **syntactic subset of `bsn!` with identical semantics**
   wherever the two overlap, and enumerate every deliberate deviation.
4. Reject every `bsn!` construct that cannot exist in an asset (expressions, closures,
   observers, function calls, consts, props) **with a specific, actionable diagnostic** —
   never a generic "unexpected token".
5. Ship it as a **standalone, bevy-free, `no_std + alloc`, independently publishable crate**
   (`core` + `alloc` + `thiserror` only) so non-Bevy tooling — a Blender exporter, a Unity
   importer, a language server — can depend on it without pulling in an ECS.
6. Ship the **write side** (`print_document`) as a first-class, tested deliverable, not a
   test helper: an external tool that cannot *emit* `.bsn` is only half a tool.

## 2. Non-goals

- Resolving symbols against the `TypeRegistry`. `Foo::Bar` is *not* classified as
  "enum variant" vs "unit struct" by this crate — see §7.6. That is SPEC-4's job. The crate
  never learns what a type path *means*; it only guarantees the string is well-formed.
- The ECS-backed AST projection (`BsnAst(World)`, pcwalton's `dynamic_bsn.rs:41-87`).
  Deferred to the editor track per SPEC-0 §6 decision 2, and it could not live here anyway
  (it needs `bevy_ecs`). `BsnNodeId` stability exists so it can be layered on top later.
- `use` imports / import versioning (SPEC-0 §2).
- Multi-error recovery. v1 is fail-fast (§8.3); logged as an open question.
- A **world→BSN** serializer (reflection → AST). That needs `bevy_reflect` and therefore
  lives above this crate (jackdaw #23639 territory, SPEC-0 §2). **AST→text printing is in
  scope and is a first-class deliverable** (§7.9).
- Full-fidelity source round-tripping (comments and original layout are not preserved; §7.8).
- Character literals, byte strings, lifetimes and const generic arguments in type paths.

## 3. Background (repo citations)

Everything below was read and is binding on this design.

| What | Where |
| --- | --- |
| Authoritative `bsn!` syntax table (all forms, values, scene lists) | `crates/bevy_scene/macros/src/lib.rs:11-130` |
| `bsn!` parser: entries, `:`/`#`/`@`/`~` prefixes, path classification | `crates/bevy_scene/macros/src/bsn/parse.rs:99-180` |
| `:` include restricted to `LitStr`; "Cannot use scene assets without caching" | `crates/bevy_scene/macros/src/bsn/parse.rs:208-241` |
| "Caching entries after the first is not supported" (base must be first entry) | `crates/bevy_scene/macros/src/bsn/parse.rs:67-73`, `77-83` |
| Struct/tuple field parsing, `@prop` fields, field shorthand | `crates/bevy_scene/macros/src/bsn/parse.rs:332-388` |
| `BsnValue` variants (Expr/Closure/Ident/Lit/Type/Tuple/Name) | `crates/bevy_scene/macros/src/bsn/types.rs:108-116` |
| `BsnEntry` variants (Name/…Patch/…Constructor/TemplateConst/Scene/RelatedSceneList) | `crates/bevy_scene/macros/src/bsn/types.rs:16-26` |
| Casing-based path classification (`Type`/`Enum`/`Const`/`TypeConst`/`TypeFunction`/`Function`) and `is_const` | `crates/bevy_macro_utils/src/path_type.rs:22-80` |
| `.bsn` asset format is a documented future feature, "broad syntactic compatibility with `bsn!`", "will not support expressions" | `crates/bevy_scene/src/lib.rs:861-880` |
| `#Name` semantics and scope rules | `crates/bevy_scene/src/lib.rs:175-253` |
| Patching semantics (multiple patches per component merge in order) | `crates/bevy_scene/src/lib.rs:255-282` |
| Prior art: pcwalton's AST node shapes (`BsnPatch`, `BsnExpr`, `BsnSymbol`) | `../dynamic_bsn.rs:41-98` |
| Prior art: symbol → type-or-enum-variant resolution (proves the parser must *not* split) | `../dynamic_bsn.rs:857-903` |
| Reflect generic type-path formatting: `Mod::Ident<A, B>`, args joined with `", "` | `crates/bevy_reflect/derive/src/derive_data.rs:1260-1290` |
| Example asset format (full type paths, `#Root`, `Children [..]`) | `../jackdaw-bsn-format.md:31-61` |

**pcwalton used LALRPOP** (`dynamic_bsn.rs:36` imports `dynamic_bsn_grammar::TopLevelPatchesParser`).
His grammar file was not available; this spec derives the grammar from his AST shapes, the
`bsn!` macro parser, and his example file. See §11.1 for the deviation rationale.

## 4. The `bevy_bsn` crate

### 4.1 Files

```text
crates/bevy_bsn/Cargo.toml
crates/bevy_bsn/README.md          // included as crate docs via #![doc = include_str!]
crates/bevy_bsn/LICENSE-MIT        // copy of the workspace file, as every bevy crate has
crates/bevy_bsn/LICENSE-APACHE
crates/bevy_bsn/src/lib.rs         // crate attrs, docs, module decls, public re-exports
crates/bevy_bsn/src/ast.rs         // Contract D types + builder + structural_eq
crates/bevy_bsn/src/lexer.rs       // Span, Token, TokenKind, Lexer, decode_* helpers
crates/bevy_bsn/src/parser.rs      // recursive-descent parser, pub fn parse
crates/bevy_bsn/src/error.rs       // BsnParseError, BsnParseErrorKind, unsupported::*, render
crates/bevy_bsn/src/printer.rs     // print_document / write_document (write side)
crates/bevy_bsn/src/tests.rs       // #[cfg(test)] corpus, table tests, round-trip tests
```

The workspace root manifest lists `members = ["crates/*"]` (`Cargo.toml:17-19`), so the
crate joins the workspace with no root-manifest edit.

### 4.2 `crates/bevy_bsn/Cargo.toml` (complete, modeled on `crates/bevy_ptr/Cargo.toml`)

```toml
[package]
name = "bevy_bsn"
version = "0.20.0-dev"
edition = "2024"
description = "Parser, AST and printer for BSN (Bevy Scene Notation) scene description files"
homepage = "https://bevy.org"
repository = "https://github.com/bevyengine/bevy"
license = "MIT OR Apache-2.0"
keywords = ["bevy", "bsn", "scene", "parser", "no_std"]
categories = ["parser-implementations", "game-development", "no-std::no-alloc"]
rust-version = "1.85.0"

[features]
default = ["std"]
## Allows access to the `std` crate. Disabling this keeps the crate `no_std` (the `alloc`
## crate is always required — the AST owns `String`s and `Vec`s).
std = ["thiserror/std"]

[dependencies]
thiserror = { version = "2", default-features = false }

[lints]
workspace = true

[package.metadata.docs.rs]
rustdoc-args = [
  "-Zunstable-options",
  "--generate-link-to-definition",
  "--generate-macro-expansion",
]
all-features = true
```

**Dependency policy (NORMATIVE):** the only permitted dependency is `thiserror`. Adding any
`bevy_*` dependency — including `bevy_platform` and `bevy_utils` — is forbidden and is
caught by the manifest test in §11.5. No `dev-dependencies` either: the test suite uses
only `core`/`alloc` and inline corpus strings (no `insta`, no `proptest`).

**Why keep `thiserror` rather than hand-writing `Display`/`Error`:** `thiserror` v2 with
`default-features = false` is already the workspace-wide no_std error idiom
(`crates/bevy_ecs/Cargo.toml:109`, `crates/bevy_reflect/Cargo.toml:114`,
`crates/bevy_math/Cargo.toml:14`); with default features off it implements
`core::error::Error` (which `std::error::Error` re-exports since Rust 1.81, below the
workspace MSRV of 1.85), so it is not an obstacle to `no_std`. It is a
compile-time-only, zero-runtime-cost, single-crate dependency that every downstream Bevy
user already compiles, and SPEC-0 §8 mandates "error enums via `thiserror`". Hand-writing
~20 `Display` arms would diverge from the rest of the engine for no gain. The `std` feature
forwards to `thiserror/std` purely so a downstream `std` build gets thiserror's `std`
configuration; nothing in `bevy_bsn` itself needs `std`.

### 4.3 `no_std` conformance

`src/lib.rs` header (house pattern, cf. `crates/bevy_math/src/lib.rs:10-27`,
`crates/bevy_ptr/src/lib.rs:1-8`):

```rust
#![doc = include_str!("../README.md")]
#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![doc(
    html_logo_url = "https://bevy.org/assets/icon.png",
    html_favicon_url = "https://bevy.org/assets/icon.png"
)]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;
```

`#![no_std]` is unconditional; `alloc` is unconditional (there is no `alloc` feature,
because the AST cannot exist without it — this is stated in the feature doc comment).
`extern crate std` exists only so `#[cfg(feature = "std")]` test helpers can use it; no
library item may reference `std`.

**Std-ism audit of the §6–§9 designs** (every item verified available in `core`/`alloc`):

| Design element | Provided by | Note |
| --- | --- | --- |
| `String`, `Vec`, `format!`, `vec!`, `ToString` | `alloc` | imported explicitly per file |
| `write!` into a `String` | `core::fmt::Write` | `alloc::string::String` implements it; the printer is written against `core::fmt::Write`, not `std::io::Write` |
| `f64::from_str` (`decode_float`) | `core` (`core::num::dec2flt`) | `impl FromStr for f64` lives in `core` |
| `f64` `Display`/`Debug`, incl. `inf`/`-inf`/`NaN` (§6.6, printer) | `core::fmt` | float formatting (`flt2dec`) is in `core`; the non-finite spellings are identical to `std` |
| `f64::INFINITY`, `f64::NAN`, `f64::to_bits` | `core` | |
| `i128::from_str_radix`, `checked_neg` | `core` | |
| `char::is_alphabetic`, `is_alphanumeric`, `is_whitespace`, `from_u32` | `core` | Unicode tables ship in `core` |
| `core::error::Error` for `BsnParseError` | `thiserror` v2 (no default features) | |
| **Hash maps** | **none — forbidden** | duplicate-field/name detection is a linear scan of the (tiny) `Vec` already being built. If a map is ever unavoidable, use `alloc::collections::BTreeMap` — never `std::collections::HashMap` or `hashbrown`: a hasher would add a dependency and, with a random seed, break the byte-determinism required by §9.4. |
| Floating-point math | none used | the crate does no arithmetic beyond `from_str`, so no `libm`/`std` float-intrinsic question arises |

CI: add one line to the `check-compiles-no-std` job in `.github/workflows/ci.yml`
(next to the existing `x86_64-unknown-none` check at `ci.yml:195`):

```yaml
- name: Check bevy_bsn compiles without std
  run: cargo check -p bevy_bsn --no-default-features --target x86_64-unknown-none
```

### 4.4 Module layout and public surface

`lib.rs` declares the modules and re-exports the whole public API at the crate root, so
consumers write `use bevy_bsn::{parse, BsnDocument};`:

```rust
mod ast;
mod error;
mod lexer;
mod parser;
mod printer;
#[cfg(test)]
mod tests;

pub use ast::{
    BsnDocument, BsnNode, BsnNodeId, BsnNodeKind, BsnPatchPrefix, BsnPath, BsnPathSegment,
    BsnValue, BsnValueId, BsnValueNode,
};
pub use error::{unsupported, BsnParseError, BsnParseErrorKind};
pub use lexer::{LexError, Lexer, Span, Token, TokenKind};
pub use parser::{parse, MAX_NESTING_DEPTH};
pub use printer::{print_document, write_document, write_document_with, PrintOptions};
```

`Lexer`/`Token`/`TokenKind`/`LexError` are public because syntax highlighters and language
servers are exactly the "separate tools" this crate exists for; they are documented as a
lower-level API whose token set may gain variants in a minor release (non-exhaustive:
`TokenKind` and `LexError` carry `#[non_exhaustive]`). Everything else — the `Parser`
struct, the printer's internal writer — stays private.

### 4.5 Crate docs headline (`README.md`, included as crate docs)

The README opens with (wording normative, prose may be polished):

```markdown
# `bevy_bsn`

Parser, abstract syntax tree, and printer for **BSN** (Bevy Scene Notation) — a compact,
human-readable, git-diffable text format for describing entities, their components and
their relationships in an ECS.

This crate is **engine-agnostic**: it depends on nothing but `core`, `alloc` and
`thiserror`, knows nothing about Bevy's `World`, reflection or asset systems, and works on
`no_std` targets. It reads `.bsn` text into a plain data AST and writes that AST back out
as canonical `.bsn` text, so external tooling — exporters, importers, editors, language
servers — can round-trip scene files without linking a game engine.

Bevy's own `bevy_scene` crate is the reference consumer: it resolves this AST against
`bevy_reflect`'s type registry to spawn entities. Nothing in this crate assumes that
consumer; the AST refers to types only by *type path string*, and what those strings mean
is entirely up to whoever resolves them.
```

Also required in the README (crates.io front page): a 15-line syntax sample (corpus §12.3),
a "reading" example (`parse`), a "writing" example (build an AST with the §7.10 builder and
`print_document` it), and the standard Bevy license footer.

### 4.6 Consumption contract for `bevy_scene` (SPEC-5 owns the wiring)

- `crates/bevy_scene/Cargo.toml` gains
  `bevy_bsn = { path = "../bevy_bsn", version = "0.20.0-dev", optional = true }`
  and a `[features]` section (the crate has none today, `Cargo.toml:1-29`):

  ```toml
  [features]
  default = ["bsn_asset"]
  ## Enables the `.bsn` asset format: the `bevy_bsn` parser, the reflection-driven
  ## resolver (SPEC-4) and `DynamicBsnLoader` (SPEC-5).
  bsn_asset = ["dep:bevy_bsn"]
  ```

- SPEC-4 and SPEC-5 import from the crate: `use bevy_bsn::{parse, BsnDocument, BsnNodeKind,
  BsnValue, BsnPath, BsnParseError, Span};`. They must **not** re-declare any of these types.
- `bevy_scene` re-exports the crate behind the feature so Bevy users need no extra
  dependency: `#[cfg(feature = "bsn_asset")] pub use bevy_bsn;` (usable as
  `bevy_scene::bevy_bsn::…`, and via `bevy::scene::bevy_bsn`).
- SPEC-5 plumbs `bsn_asset` through `bevy_internal`'s `bevy_scene` feature forwarding.

*(This supersedes the earlier open question about gating the parser behind a `bevy_scene`
cargo feature: the parser is a separate crate, so the only feature involved is
`bevy_scene/bsn_asset`, which gates the optional dependency plus SPEC-4/5's modules.)*

### 4.7 Release & publishing

- The crate is published to crates.io by Bevy's existing release tooling, which walks the
  workspace members in dependency order. `bevy_bsn` has no bevy dependencies, so it
  publishes **first**, before `bevy_ecs`.
- `version = "0.20.0-dev"` tracks the workspace version, as every other bevy crate does; it
  is bumped by the release tooling, not by hand.
- **No path-only dependencies anywhere in the public API.** `bevy_bsn` has no path deps at
  all; `bevy_scene`'s dependency on it carries both `path` and `version`, which is what
  `cargo publish` requires (same shape as `bevy_scene`'s existing entries,
  `crates/bevy_scene/Cargo.toml:12-21`).
- `publish` is **not** set to `false`. `README.md`, both LICENSE files, `description`,
  `keywords` (≤ 5, enforced by crates.io) and `categories` are present so the crates.io page
  is complete on first publish.
- The crate must build from a `cargo package` tarball: `README.md` is inside the crate
  directory (not a workspace-root symlink) so `include_str!("../README.md")` resolves, and
  no test reaches outside the crate directory (the manifest test in §11.5 uses
  `include_str!("../Cargo.toml")`, which is packaged).
- Semver: `BsnParseErrorKind`, `TokenKind` and `LexError` are `#[non_exhaustive]` (consumers
  should always have a fallback arm for a new diagnostic or token). `BsnValue`,
  `BsnNodeKind` and `BsnPatchPrefix` are deliberately **exhaustive**: a new value form is a
  format change that every resolver must consciously handle, and a compile error in SPEC-4
  is the desired outcome.

---

## 5. The grammar

### 5.1 Conventions

EBNF: `{ x }` = zero or more, `[ x ]` = optional, `|` = alternation, `"x"` = terminal,
UPPER = lexical token (§6). Whitespace and comments may appear between any two tokens and
are never significant (identical to `syn`, so `CompA (1)` and `CompA(1)` are the same).

### 5.2 Document and entities

```ebnf
document      = [ entity_list ] EOF ;

entity_list   = entity { "," entity } [ "," ] ;

entity        = "(" entity_body ")"
              | entity_body ;

entity_body   = [ base ] { entry } ;

base          = ":" STRING ;

entry         = name_entry
              | relation_entry
              | patch_entry ;

name_entry    = "#" IDENT ;

relation_entry= path "[" [ entity_list ] "]" ;

patch_entry   = [ "~" | "@" ] path [ struct_body | tuple_body ] ;
```

Notes, each traceable to the macro:

- A document is a **scene list without brackets** — exactly the body of `bsn_list![ … ]`
  (`macros/src/bsn/parse.rs:189-206`). One root needs no comma and may be written flat;
  two or more roots are comma-separated. An **empty document is legal** and yields zero roots.
- `entity_body` inside `( … )` matches `Bsn<ALLOW_FLAT>`'s parenthesized branch
  (`parse.rs:62-74`); the flat branch matches `parse.rs:75-90`, including the rule that a
  top-level comma terminates the current entity.
- `base` must be the **first** entry; a `:` entry anywhere else is an error (§8.2 E-BASE-POS),
  matching `parse.rs:67-73`.
- `base` accepts a **string literal only** — `:"player.bsn"`. `:foo()` / `:@Widget` (cacheable
  scene functions / scene components in the macro) are rejected (E-BASE-KIND), matching
  `parse.rs:229-241`.
- At most **one** `#Name` per entity (Contract D has `name: Option<String>`); a second is
  E-DUP-NAME.
- `relation_entry` vs `patch_entry` is decided by one token of lookahead after the path:
  `[` ⇒ relation, otherwise patch. Same as `parse.rs:122-128`.
- `~` = "this path is already a `Template`" (`BsnEntry::TemplatePatch`);
  `@` = "this path is a `SceneComponent`" (`BsnScene::SceneComponent`, `parse.rs:245-251`).
  Both are parsed; SPEC-4 decides what it can support.

### 5.3 Patch bodies and values

```ebnf
struct_body   = "{" [ field { "," field } [ "," ] ] "}" ;
field         = IDENT ":" value ;

tuple_body    = "(" [ value { "," value } [ "," ] ] ")" ;

value         = INT
              | FLOAT
              | STRING
              | "true" | "false"
              | "inf" | "NaN" | "nan"
              | "-" ( INT | FLOAT | "inf" )
              | entity_ref
              | list_value
              | paren_value
              | path_value ;

entity_ref    = "#" IDENT ;
list_value    = "[" [ value { "," value } [ "," ] ] "]" ;
paren_value   = "(" ")"                                   (* unit *)
              | "(" value ")"                             (* grouping, not a 1-tuple *)
              | "(" value "," ")"                         (* 1-tuple *)
              | "(" value "," value { "," value } [ "," ] ")" ;
path_value    = path [ struct_body | tuple_body ] ;
```

- Fields are **partial**: any subset of the type's fields, in any order. Unmentioned fields
  keep the value from earlier patches/defaults (`crates/bevy_scene/src/lib.rs:255-282`).
- Tuple bodies are **partial leading prefixes**: `Comp(1.0)` on a 3-field tuple struct sets
  field 0 and leaves 1 and 2 alone (pcwalton zips fields with field infos,
  `dynamic_bsn.rs:374`). Trailing "holes" cannot be expressed — use a struct body with
  numeric field names? **No**: numeric field names are *not* supported in v1 (§8.4 open q.).
- Struct bodies nest arbitrarily: `Transform { translation: glam::Vec3 { x: 1.0 } }`.
- Commas are **required** between fields, tuple elements, list items and entity-list items
  (the macro's comma-optional loop, `parse.rs:24-45`, exists only for rust-analyzer
  autocomplete). Trailing commas are always allowed. Requiring commas keeps `.bsn` a strict
  subset of what `bsn!` accepts.
- `(v)` is a **grouping**, `(v,)` is a 1-tuple — Rust semantics.
- `-` applies only to `INT`, `FLOAT` and `inf`; `- Foo` is E-NEG-OPERAND.

### 5.4 Paths

```ebnf
path          = path_segment { "::" path_segment } ;
path_segment  = IDENT [ "<" path { "," path } [ "," ] ">" ] ;
```

- Generic arguments are supported because `bevy_reflect` type paths contain them
  (`alloc::vec::Vec<f32>`, `bevy_asset::handle::Handle<bevy_image::image::Image>`).
  Only *type paths* are allowed as arguments — no lifetimes, no const generics, no
  `&T`, `[T; N]` or `(A, B)` type syntax (open question §11.5).
- A **leading `::`** is rejected (E-PATH-LEADING). Reflect type paths never have one.
- `BsnPath::to_type_path()` reconstructs the canonical string with `::` between segments and
  `", "` between generic arguments — byte-identical to what
  `crates/bevy_reflect/derive/src/derive_data.rs:1260-1290` generates, so
  `TypeRegistry::get_with_type_path` matches.

### 5.5 Rejected `bsn!` constructs

The parser detects each of these **structurally** and emits the exact message below.
`{kind}` in the diagnostics is filled from context. All messages end with a remedy.

| # | Construct | Detected when | Error code | Message |
| --- | --- | --- | --- | --- |
| 1 | `{ expr }` | `{` in value position, or `{` where an entry is expected | `E-EXPR` | ``Rust expressions (`{ ... }`) are not supported in `.bsn` assets. Only literal values are allowed here; move the computation into a `bsn!` macro or a scene function.`` |
| 2 | `const { .. }`, `unsafe { .. }` | IDENT `const`/`unsafe` followed by `{` | `E-EXPR` | same as #1 |
| 3 | closure `\|x\| { .. }` | `\|` anywhere | `E-CLOSURE` | ``Closures are not supported in `.bsn` assets. Observers and template functions must be written in Rust.`` |
| 4 | `on(...)` | entry position, path is exactly `on`, next token `(` | `E-OBSERVER` | ``Observers (`on(...)`) are not supported in `.bsn` assets. Attach them from Rust, e.g. with a `bsn!` scene or an `Observer` entity.`` |
| 5 | scene fn `my_scene()`, `my_scene` | entry position, last path segment starts lowercase | `E-FN` | ``Scene functions and function calls are not supported in `.bsn` assets. Expected a type path; type names start with an uppercase letter.`` |
| 6 | ctor `Type::from_str("x")` | last segment starts lowercase, previous starts uppercase | `E-CTOR` | ``Constructor calls (`Type::function(...)`) are not supported in `.bsn` assets. Write the resulting value out in full instead.`` |
| 7 | const `PI`, `foo::X_AXIS`, `Type::MAX` | last segment `is_const` (≥2 chars, contains no lowercase letter) — same rule as `crates/bevy_macro_utils/src/path_type.rs:71-80` | `E-CONST` | ``Constants are not supported in `.bsn` assets. Write the literal value instead (`.bsn` has no access to Rust items).`` |
| 8 | field shorthand `Comp { name }` | IDENT in a struct body not followed by `:` | `E-SHORTHAND` | ``Field shorthand (`{ name }`) is not supported in `.bsn` assets, because there are no variables to capture. Write `name: <value>` instead.`` |
| 9 | props `@Widget { @prop: 1 }` | `@` inside a struct body | `E-PROP` | ``Scene component props (`@prop: ...`) are not supported in `.bsn` assets. Props are evaluated by Rust code when the scene is included and cannot be expressed in an asset.`` |
| 10 | macro call `vec![1]` | `!` anywhere | `E-MACRO` | ``Macro invocations are not supported in `.bsn` assets. Use a list literal `[ ... ]` instead of `vec![ ... ]`.`` |
| 11 | `use foo::Bar;` | entry position, path is exactly `use` | `E-USE` | ```use` imports are not supported in `.bsn` assets. Write fully-qualified type paths, e.g. `bevy_transform::components::transform::Transform`.`` |
| 12 | char literal `'a'` | `'` anywhere | `E-CHAR` | ``Character literals are not supported in `.bsn` assets. Use a string literal instead.`` |
| 13 | numeric suffix `1.0f32`, `1u8` | ident char immediately after a number | `E-SUFFIX` | ``Numeric literal suffixes are not supported in `.bsn` assets. The field's declared type determines the literal's type.`` |
| 14 | raw ident `r#type` | `r#` not followed by `"` | `E-RAWIDENT` | ``Raw identifiers (`r#name`) are not supported in `.bsn` assets.`` |

Rules #5–#7 use Rust naming conventions exactly as the `bsn!` macro already does
(`PathType::new`), so **no expressiveness is lost relative to the macro**: a SCREAMING_CASE
enum variant is unusable in `bsn!` today for the same reason. This is documented, not fixed.
Rules #5–#7 apply in **entry position and value position**; #6/#7 messages are chosen by
casing, #5 only fires in entry position (in value position a lowercase path is E-PATH-CASE:
``Expected a value. Type paths start with an uppercase letter; `{path}` looks like a function or variable.``).

---

## 6. Lexer specification (`lexer.rs`)

### 6.1 Types

```rust
/// A half-open byte range into the source text.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Span { pub start: u32, pub end: u32 }

impl Span {
    pub fn new(start: u32, end: u32) -> Self;
    /// 1-based (line, column) of `self.start`; column counts `char`s, not bytes.
    pub fn line_col(&self, source: &str) -> (u32, u32);
    /// The exact source text, or `""` if the span is out of bounds.
    pub fn text<'s>(&self, source: &'s str) -> &'s str;
    pub fn join(self, other: Span) -> Span; // min(start), max(end)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Token { pub kind: TokenKind, pub span: Span }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    Ident,        // identifier or keyword-like word (`true`, `inf`, `use`, …)
    Int,          // 12, 0xFF, 1_000, 0b1010, 0o17
    Float,        // 1.0, 1., 1e5, 2.5E-3
    Str,          // "…" or r"…" or r#"…"# — span INCLUDES the delimiters
    ColonColon, Colon, Comma, Hash, At, Tilde, Minus,
    Lt, Gt,
    LParen, RParen, LBrace, RBrace, LBracket, RBracket,
    Eof,
    /// A lexical problem. The parser turns this into a `BsnParseError` verbatim.
    Error(LexError),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LexError {
    UnterminatedString, UnterminatedBlockComment, UnterminatedRawString,
    InvalidEscape, InvalidNumber, NumericSuffix, RawIdentifier,
    CharLiteral, Closure, Macro, Unknown,
}
```

`Token` is `Copy` and carries **no owned data**: string/number values are decoded on demand
from the span (§6.5). The lexer therefore performs **zero allocations**.

### 6.2 Driver

```rust
pub struct Lexer<'src> { source: &'src str, pos: usize }

impl<'src> Lexer<'src> {
    /// Skips a leading UTF-8 BOM (`\u{FEFF}`) if present.
    pub fn new(source: &'src str) -> Self;
    /// Never fails; returns `Eof` forever once exhausted.
    pub fn next_token(&mut self) -> Token;
    /// Convenience for tests and the parser: lex everything, ending with exactly one `Eof`.
    pub fn tokenize(source: &'src str) -> Vec<Token>;
}
```

The parser pre-lexes the whole file into a `Vec<Token>` (asset files are small; this buys
unlimited lookahead and trivial `peek`/`peek2`).

### 6.3 Token-matching rules (the state machine)

`next_token` runs `skip_trivia()` then dispatches on the first `char` `c` at `pos`:

**`skip_trivia()`** loops until no progress:

1. `c.is_whitespace()` ⇒ consume. (Covers ` `, `\t`, `\n`, `\r`, and Unicode spaces.)
2. `//` ⇒ consume through the next `\n` or EOF. (`///` and `//!` are ordinary comments; no
   doc capture in v1, open question §14.6.)
3. `/*` ⇒ consume with a **nesting depth counter** (Rust-compatible): `/*` increments, `*/`
   decrements, depth 0 ends. EOF at depth > 0 ⇒ return `Error(UnterminatedBlockComment)`
   spanning from the opening `/*` to EOF.
4. Anything else ⇒ stop.

**Dispatch table** (first match wins; `start = pos` before consuming):

| Input | Action | Token |
| --- | --- | --- |
| EOF | — | `Eof`, span `(len, len)` |
| `(` `)` `{` `}` `[` `]` `,` `#` `@` `~` `<` `>` | consume 1 | matching kind |
| `:` followed by `:` | consume 2 | `ColonColon` |
| `:` | consume 1 | `Colon` |
| `-` | consume 1 | `Minus` |
| `"` | §6.4 string | `Str` or `Error` |
| `r` followed by `"` or `#`+`"` | §6.4 raw string | `Str` or `Error(UnterminatedRawString)` |
| `r` followed by `#` not leading to `"` | consume `r#` | `Error(RawIdentifier)` |
| `'` | consume 1 | `Error(CharLiteral)` |
| `\|` | consume 1 | `Error(Closure)` |
| `!` | consume 1 | `Error(Macro)` |
| ident start: `c.is_alphabetic() \|\| c == '_'` | consume while `is_alphanumeric() \|\| '_'` | `Ident` |
| ASCII digit | §6.5 number | `Int` / `Float` / `Error` |
| anything else (`.`, `=`, `;`, `&`, `*`, `?`, `$`, …) | consume 1 `char` | `Error(Unknown)` |

An `Error` token **always consumes at least one `char`**, so the lexer cannot loop.

### 6.4 String literals

Normal string: after the opening `"`, consume until an unescaped `"`.

- `\` starts an escape; the lexer validates it immediately: one of
  `\\ \" \' \n \r \t \0`, `\xNN` (exactly 2 hex digits, value ≤ `0x7F`), or
  `\u{H…}` (1–6 hex digits, a valid `char`). Anything else ⇒ `Error(InvalidEscape)`
  spanning the backslash and the offending char.
- A raw `\n` inside the literal is allowed (multi-line strings, as in Rust).
- EOF before the closing quote ⇒ `Error(UnterminatedString)` spanning from the opening quote.

Raw string: `r` `#`×N `"` … `"` `#`×N, N ≥ 0. No escape processing. Unterminated ⇒
`Error(UnterminatedRawString)`.

The `Str` token's span **includes** all delimiters.

### 6.5 Numbers

```text
INT   = dec | "0x" hex+ | "0o" oct+ | "0b" bin+          (with `_` allowed after the 1st digit)
dec   = digit { digit | "_" }
FLOAT = dec "." [ dec ] [ exp ] | dec exp
exp   = ("e"|"E") ["+"|"-"] digit { digit | "_" }
```

Scanner: consume the integer part (with optional `0x`/`0o`/`0b` prefix — prefixed forms may
not be followed by `.` or an exponent). For a decimal integer, if the next char is `.` **and**
the char after it is not `.`, consume the `.` plus any following digits, then an optional
exponent, and emit `Float`. Otherwise emit `Int`.

- After the number, if the next char is an ident-start char ⇒ `Error(NumericSuffix)` spanning
  number+suffix (covers `1u8`, `1.0f32`, and `0x1g`).
- A prefix with no digits (`0x`) ⇒ `Error(InvalidNumber)`.
- `.5` is **not** a float: `.` lexes as `Error(Unknown)` (Rust-compatible).

Value decoding (parser-side helpers in `lexer.rs`, all returning `Result<_, BsnParseError>`):

```rust
pub fn decode_int(source: &str, span: Span) -> Result<i128, BsnParseError>;   // strips `_`, honors 0x/0o/0b
pub fn decode_float(source: &str, span: Span) -> Result<f64, BsnParseError>;  // strips `_`, core f64 FromStr
pub fn decode_string(source: &str, span: Span) -> Result<String, BsnParseError>; // unescapes; raw strings verbatim
```

`decode_int` returns `NumberOutOfRange` when the digits exceed `i128`; `decode_float`
returns `InvalidNumber` if `f64::from_str` fails (unreachable for well-formed tokens, but no
`unwrap`).

### 6.6 Non-finite floats

The three spellings `f64::Display` produces are accepted as float **values** (not paths):
`inf`, `-inf` (via `Minus`), `NaN`. Lowercase `nan` is accepted as a hand-authoring alias.
Recognition happens in the parser (value position only), so a type named `NaN` is still
usable in entry position. Canonical output of the printer is `inf`, `-inf`, `NaN`.

---

## 7. AST specification (`ast.rs`) — Contract D

### 7.1 IDs and arenas

```rust
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BsnNodeId(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct BsnValueId(pub u32);

/// A parsed `.bsn` document. Two flat arenas; ids are indices.
#[derive(Clone, Debug, Default)]
pub struct BsnDocument {
    /// Root entities, in source order.
    pub roots: Vec<BsnNodeId>,
    /// Entity / patch / relation nodes, indexed by `BsnNodeId`.
    pub nodes: Vec<BsnNode>,
    /// Value nodes, indexed by `BsnValueId`.
    pub values: Vec<BsnValueNode>,
}

#[derive(Clone, Debug)]
pub struct BsnNode { pub id: BsnNodeId, pub span: Span, pub kind: BsnNodeKind }

#[derive(Clone, Debug)]
pub struct BsnValueNode { pub id: BsnValueId, pub span: Span, pub value: BsnValue }

#[derive(Clone, Debug)]
pub enum BsnNodeKind {
    Entity {
        name: Option<String>,      // `#Name`, without the `#`
        name_span: Option<Span>,
        base: Option<String>,      // `:"path.bsn"`, unescaped, without the quotes
        base_span: Option<Span>,
        patches: Vec<BsnNodeId>,   // all `BsnNodeKind::Patch`, source order
        relations: Vec<BsnNodeId>, // all `BsnNodeKind::Relation`, source order
    },
    Patch { symbol: BsnPath, prefix: BsnPatchPrefix, value: BsnValueId },
    Relation { target_symbol: BsnPath, entities: Vec<BsnNodeId> },
}

/// Which sigil (if any) preceded a patch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BsnPatchPrefix {
    /// `Transform { … }` — the symbol is a component; resolve via `FromTemplate`.
    FromTemplate,
    /// `~MyTemplate { … }` — the symbol is already a `Template`.
    Template,
    /// `@MyWidget { … }` — the symbol is a `SceneComponent`.
    SceneComponent,
}
impl BsnPatchPrefix { pub fn is_template(self) -> bool { matches!(self, Self::Template) } }

#[derive(Clone, Debug)]
pub enum BsnValue {
    Unit,                                        // ()
    Bool(bool),
    Int(i128),
    Float(f64),
    String(String),                              // unescaped
    Path(BsnPath),                               // `Foo`, `a::b::Foo::Bar` — see §7.6
    Tuple(Vec<BsnValueId>),                      // (1, 2)
    List(Vec<BsnValueId>),                       // [1, 2]
    Struct(BsnPath, Vec<(String, BsnValueId)>),  // `a::B { x: 1 }`
    NamedTuple(BsnPath, Vec<BsnValueId>),        // `a::B(1, 2)`
    EntityRef(String),                           // `#Name`, without the `#`
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BsnPath { pub segments: Vec<BsnPathSegment>, pub span: Span }

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BsnPathSegment { pub ident: String, pub generics: Vec<BsnPath>, pub span: Span }
```

**Deviations from the Contract D sketch, all additive** (see §11.2–§11.3):
`BsnValue::Struct` gains the leading `BsnPath` (the sketch omitted it although
`NamedTuple` had one and nested struct values such as `glam::Vec3 { x: 0.0 }` require it);
`is_template: bool` becomes `BsnPatchPrefix` (a `bool` cannot represent `@`);
`BsnPath` is structured rather than a string so generics survive round-trip;
`name_span`/`base_span` are added for diagnostics.

### 7.2 Accessors

```rust
impl BsnDocument {
    pub fn parse(source: &str) -> Result<BsnDocument, BsnParseError>;   // == bevy_bsn::parse
    pub fn node(&self, id: BsnNodeId) -> Option<&BsnNode>;
    pub fn value(&self, id: BsnValueId) -> Option<&BsnValueNode>;
    /// Iterates every entity node in document order (roots then nested, i.e. ascending id).
    pub fn entities(&self) -> impl Iterator<Item = &BsnNode>;
    /// Deterministic, indented dump used by tests and `Debug`-style output (§9.3).
    pub fn debug_tree(&self) -> String;
    /// Canonical `.bsn` text (§7.9).
    pub fn to_bsn_string(&self) -> String;
}

impl BsnPath {
    /// `a::b::C<d::E, f::G>` — `::` between segments, `", "` between generic arguments.
    /// Byte-identical to `bevy_reflect`'s generated `TypePath::type_path()`.
    pub fn to_type_path(&self) -> String;
    /// Everything but the last segment, or `None` if there is only one.
    /// SPEC-4 uses this for the enum-variant fallback (`dynamic_bsn.rs:870-880`).
    pub fn parent_type_path(&self) -> Option<String>;
    pub fn last_ident(&self) -> &str;
    pub fn is_single_segment(&self) -> bool;
}
```

Index by id is never done with `[]` panics in library code; `node()`/`value()` return
`Option` and SPEC-4/5 treat `None` as an internal error.

### 7.3 Node-ID assignment (NORMATIVE — stability contract)

IDs are assigned in **pre-order of the source text**: a node reserves its id at the moment
the parser *starts* parsing it, before any of its children.

Implementation: `Parser::alloc_node()` pushes `None` onto an internal
`Vec<Option<BsnNode>>` and returns the index; `Parser::finish_node(id, span, kind)` writes
the slot. Values use the identical `alloc_value`/`finish_value` pair. At the end, `finish()`
converts `Vec<Option<_>>` → `Vec<_>`; a `None` slot is impossible by construction and yields
`BsnParseErrorKind::Internal` (never a panic).

Consequences, all required by Contract C item 4 (asset-based `SceneEntityReference`):

- IDs are a pure function of the token stream, hence **identical across re-parses of
  identical text** (no hashing, no ordering by name, no `HashMap` iteration anywhere).
- Root entity of a single-root document is always `BsnNodeId(0)`.
- Node ids and value ids are **independent** counters.
- Editing a file renumbers later nodes. That is acceptable: hot reload re-resolves the whole
  document (SPEC-0 §6 decision 7).

### 7.4 Ordering invariants

- `roots` is in source order.
- Inside an entity, `patches` and `relations` are each in source order. The *interleaving*
  of the two is not stored (Contract D splits them), but is recoverable by merging the two
  lists on `node.span.start`, which the printer (§7.9) does.
- `BsnValue::Struct` fields are in source order (not sorted) — patch order matters.

### 7.5 Patch/value invariant

For every `BsnNodeKind::Patch { symbol, value, .. }`, `document.values[value]` is exactly one
of:

| Source | `BsnValue` | Invariant |
| --- | --- | --- |
| `Foo` | `Path(p)` | `p == symbol` |
| `Foo { … }` | `Struct(p, fields)` | `p == symbol` |
| `Foo( … )` | `NamedTuple(p, items)` | `p == symbol` |

The parser clones the path into the value so SPEC-4 can handle patch values and nested field
values with one code path. A `debug_assert_eq!` guards this in the parser.

### 7.6 Path values are NOT disambiguated (boundary statement)

**The parser cannot and does not decide whether `a::b::Foo::Bar` is**

- a unit enum variant `Bar` of enum `a::b::Foo`, or
- a unit struct `Bar` in module `a::b::Foo` (unusual, but legal), or
- a struct/tuple **variant** when followed by `{ … }` / `( … )`.

It always stores the full path in `BsnPath` and leaves classification to SPEC-4, which
performs the registry lookup ladder ported from `dynamic_bsn.rs:857-903`:
`get_with_type_path(full)` first, then `get_with_type_path(parent)` with the last segment as
the variant name. This is why the AST has **no** `enum_variant: Option<Ident>` field, unlike
the macro's `BsnType` (`macros/src/bsn/types.rs:29-33`) — the macro can rely on casing
because it emits Rust code that the compiler checks, while the asset path must not guess.

The single place casing *is* consulted is diagnostics selection for rejected constructs
(§5.5 #5–#7); it never changes an accepted parse.

### 7.7 Depth limit

`MAX_NESTING_DEPTH = 128`, counted over `parse_entity` + `parse_value` + `parse_path`
recursion. Exceeding it is `BsnParseErrorKind::NestingTooDeep`. `.bsn` files are untrusted
input to an asset loader; the recursive-descent parser must not be able to blow the stack.

### 7.8 What the AST deliberately does not carry

Comments, blank lines and original formatting are discarded, as are lexical-only details of
literals: integer radix and digit underscores (`0xFF` and `255` both become `Int(255)`),
raw-string delimiters, and `1.` vs `1.0`. A parse→print cycle therefore *normalizes* a file;
it never changes its meaning. Full-fidelity source round-tripping (a CST with trivia) is a
later concern (§14.6).

### 7.9 The canonical printer — the write side (`printer.rs`)

This is a **first-class deliverable**, not a test helper: an external tool (Blender exporter,
Unity importer, editor, formatter) must be able to construct or modify a `BsnDocument` and
emit valid `.bsn` text without linking Bevy.

#### API

```rust
/// Formatting knobs. `Default` is the canonical style and is what `print_document` uses.
#[derive(Clone, Debug)]
pub struct PrintOptions {
    /// Spaces per indent level. Default 4.
    pub indent: u8,
    /// Soft line-width budget for inlining a struct/tuple/list body. Default 100.
    pub max_inline_width: u16,
    /// Emit a trailing comma on multi-line bodies. Default true.
    pub trailing_commas: bool,
    /// Blank line between top-level roots. Default true.
    pub blank_line_between_roots: bool,
}
impl Default for PrintOptions { /* 4, 100, true, true */ }

/// Canonical `.bsn` text for `document`. Never fails and never panics: a malformed
/// document (dangling id) prints a `/* <invalid node id N> */` marker instead of aborting.
pub fn print_document(document: &BsnDocument) -> String;

/// Streaming form — `core::fmt::Write`, so it works on `String`, a `no_std` sink, or
/// (via an adapter) `std::io::Write`.
pub fn write_document<W: core::fmt::Write>(document: &BsnDocument, out: &mut W) -> core::fmt::Result;
pub fn write_document_with<W: core::fmt::Write>(
    document: &BsnDocument, out: &mut W, options: &PrintOptions,
) -> core::fmt::Result;

impl BsnDocument {
    /// Convenience alias for `print_document(self)`.
    pub fn to_bsn_string(&self) -> String;
}
```

#### Formatting rules (normative — the output is a stable format, so tools diff cleanly)

1. **Roots**: printed in `roots` order, separated by `,\n` plus one blank line when
   `blank_line_between_roots`. No trailing comma after the last root. A root is printed
   *flat* (no parentheses) — parentheses are never needed at top level because the comma
   terminates the entity.
2. **Entity**: `:"base"` first (if any) on its own line; then `#Name` (if any) on its own
   line; then entries, one per line, at the current indent.
3. **Entry order**: merge `patches` and `relations` by `span.start`, ties broken by ascending
   `BsnNodeId`. A synthesized document (all spans `Span::NONE`, §7.10) therefore prints in
   id order — patches and relations interleave exactly as the builder created them.
4. **Patch**: `<prefix><type path><body>` where prefix is `""`, `"~"` or `"@"`.
   `Path` value ⇒ no body. `Struct` ⇒ `{ … }`. `NamedTuple` ⇒ `( … )` with no space.
5. **Bodies** (struct fields, tuple items, list items): rendered inline, comma+space
   separated, if the resulting line (indent + content) is ≤ `max_inline_width` **and**
   contains no nested multi-line body; otherwise one item per line at indent + 1, with a
   trailing comma when `trailing_commas`.
6. **Relation**: `<type path> [` then each child entity indented by one level and separated
   by `,\n`, then `]` at the entry's indent. An empty relation prints `Path []`.
7. **Strings**: minimal re-escaping — `\\`, `\"`, `\n`, `\r`, `\t`, `\0`; any other
   `char::is_control()` char as `\u{H…}`; everything else verbatim (UTF-8 is emitted as-is,
   never escaped). Raw strings are never emitted, so print output is always plain-quoted.
8. **Floats**: `{:?}` (which yields `1.0`, not `1`, so the value re-parses as `Float`), with
   `f64::INFINITY` ⇒ `inf`, `f64::NEG_INFINITY` ⇒ `-inf`, NaN ⇒ `NaN`. `-0.0` prints as
   `-0.0`.
9. **Ints**: decimal, `-` prefix for negatives. No underscores, no hex (input radix is not
   preserved — documented in §7.8).
10. **Paths**: `BsnPath::to_type_path()` verbatim (`::` separators, `", "` between generic
    arguments).
11. **`()`** prints as `()`, a 1-tuple as `(v,)`, `#Name` as `#Name`, bools as `true`/`false`.
12. **Empty document** prints `""` (the empty string), which re-parses to an empty document.
13. Output always ends with exactly one `\n` when non-empty. Line endings are always `\n`.

#### Round-trip property (NORMATIVE, tested in §11.3)

For every `BsnDocument` `d` that either came from `parse` or was built through the §7.10
builder:

```text
parse(&print_document(&d))  ==  d      // modulo spans, via BsnDocument::structural_eq
print_document(&parse(&print_document(&d)))  ==  print_document(&d)   // text-identical
```

Spans cannot survive printing (the text moves), so equality is `structural_eq` (§7.10).
For text sources the weaker but important property is
`parse(s)` ≡ `parse(print(parse(s)))`: printing is *semantics-preserving*, and printing
twice is a fixed point (the printer is idempotent, i.e. a formatter).

### 7.10 Building an AST programmatically (the exporter path)

Tools that generate `.bsn` do not parse first; they build. All AST fields are `pub`, and
these constructors make id allocation safe:

```rust
impl Span {
    /// The span of a synthesized node that has no source text. `Span { start: 0, end: 0 }`.
    pub const NONE: Span;
    pub fn is_none(&self) -> bool;
}

impl BsnDocument {
    pub fn new() -> Self;                                   // == Default
    /// Appends a value node and returns its id. Span defaults to `Span::NONE`.
    pub fn push_value(&mut self, value: BsnValue) -> BsnValueId;
    pub fn push_value_spanned(&mut self, value: BsnValue, span: Span) -> BsnValueId;
    /// Appends a node and returns its id.
    pub fn push_node(&mut self, kind: BsnNodeKind) -> BsnNodeId;
    pub fn push_node_spanned(&mut self, kind: BsnNodeKind, span: Span) -> BsnNodeId;
    /// Marks an existing node as a document root (appends to `roots`).
    pub fn push_root(&mut self, id: BsnNodeId);
    /// Convenience: `push_node(Patch { … })` with the §7.5 value invariant upheld —
    /// builds the `Path`/`Struct`/`NamedTuple` value for you.
    pub fn push_patch(&mut self, prefix: BsnPatchPrefix, path: BsnPath, body: PatchBody) -> BsnNodeId;
    /// Structural equality: ignores every `Span`, compares floats by `to_bits`
    /// (so `NaN == NaN` and `0.0 != -0.0`), compares ids by position rather than value.
    pub fn structural_eq(&self, other: &BsnDocument) -> bool;
}

/// What follows a patch's path when building one.
pub enum PatchBody { Unit, Struct(Vec<(String, BsnValueId)>), Tuple(Vec<BsnValueId>) }

impl BsnPath {
    /// `BsnPath::parse_type_path("glam::Vec3")` — the inverse of `to_type_path`, for tools
    /// that hold a type-path string (e.g. from a reflection dump). Returns `None` if the
    /// string is not a syntactically valid path.
    pub fn from_type_path(s: &str) -> Option<BsnPath>;
    pub fn from_segments(idents: impl IntoIterator<Item = impl Into<String>>) -> BsnPath;
}
```

Builder-produced ids are still document-order-stable (they are assigned sequentially), so a
tool that builds the same scene twice gets the same ids — the property SPEC-2's asset-based
`SceneEntityReference` depends on.

---

## 8. Errors (`error.rs`)

### 8.1 Types

```rust
#[derive(Clone, Debug, thiserror::Error)]
#[error("{kind}")]
pub struct BsnParseError {
    /// Byte range of the offending text.
    pub span: Span,
    pub kind: BsnParseErrorKind,
    /// Token descriptions that would have been accepted here, e.g. `["`,`", "`]`"]`.
    /// Empty for errors where an "expected" list is meaningless.
    pub expected: Vec<&'static str>,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum BsnParseErrorKind {
    #[error("unexpected {found}")]                       UnexpectedToken { found: &'static str },
    #[error("unexpected end of file")]                   UnexpectedEof,
    #[error("unterminated string literal")]              UnterminatedString,
    #[error("unterminated block comment")]               UnterminatedBlockComment,
    #[error("invalid escape sequence")]                  InvalidEscape,
    #[error("invalid numeric literal")]                  InvalidNumber,
    #[error("integer literal out of range for i128")]    NumberOutOfRange,
    #[error("unexpected character `{0}`")]               UnknownCharacter(char),
    #[error("a `:\"…\"` scene include must be the first entry of an entity")] BaseNotFirst,
    #[error("only scene assets can be included with `:`; expected a string literal, e.g. `:\"player.bsn\"`")] BaseNotString,
    #[error("duplicate entity name; an entity may have at most one `#Name`")] DuplicateName,
    #[error("duplicate field `{0}`")]                    DuplicateField(String),
    #[error("`-` may only be applied to a number or `inf`")] NegOperand,
    #[error("paths may not start with `::`")]            LeadingPathSeparator,
    #[error("nesting is too deep (limit {MAX_NESTING_DEPTH})")] NestingTooDeep,
    /// A `bsn!` construct that cannot exist in an asset. `.0` is one of the §5.5 messages.
    #[error("{0}")]                                      Unsupported(&'static str),
    #[error("internal parser error: {0}")]               Internal(&'static str),
}
```

Every §5.5 message is a `pub const &str` in the public `unsupported` module, re-exported at
the crate root (`bevy_bsn::unsupported::EXPR`, `::CLOSURE`, `::OBSERVER`, `::FN`, `::CTOR`,
`::CONST`, `::SHORTHAND`, `::PROP`, `::MACRO`, `::USE`, `::CHAR`, `::SUFFIX`, `::RAW_IDENT`,
`::PATH_CASE`), so tests — and downstream tools that want to special-case a diagnostic —
can compare against the exact constant rather than a string literal.

### 8.2 Rendering (used by SPEC-5's loader)

```rust
impl BsnParseError {
    /// rustc-style, e.g.
    /// error: unexpected `}`
    ///   --> assets/player.bsn:3:22
    ///    |
    ///  3 |     Transform { x: 1.0 }
    ///    |                      ^ expected `,` or `:`
    pub fn render(&self, source: &str, path: Option<&str>) -> String;
}
```

`render` clamps spans, handles EOF spans (caret after the last char), replaces tabs with a
single space when drawing the caret line, and truncates lines longer than 200 chars.

### 8.3 Recovery policy

**v1 is fail-fast**: the first error aborts and is returned. Rationale: the consumer is an
asset loader whose `Result` is a single error (Contract F), and every additional error after
the first is speculative. pcwalton's loader has the same limitation
(`dynamic_bsn.rs:113` `// TODO: Report multiple errors`). Multi-error recovery is
open question §11.4.

### 8.4 Edge cases (exhaustive list, each with defined behavior)

| Case | Behavior |
| --- | --- |
| Empty file / only comments | `Ok` with `roots: []` |
| Leading UTF-8 BOM | skipped |
| CRLF line endings | whitespace; spans stay byte-accurate |
| Trailing comma at any list level | accepted |
| `,` with nothing before it (`, Foo`) | `UnexpectedToken` expecting an entity |
| `()` as a whole entity | `Ok`: an entity with no name, no base, no patches |
| `[]` as a relation body | `Ok`: relation with zero entities |
| `Foo {}` / `Foo()` | `Ok`: empty struct/tuple body (all fields default) |
| Duplicate field name in one struct body | `DuplicateField` |
| Two patches of the same component | **accepted** — this is the patching feature |
| `#A` twice on one entity | `DuplicateName` |
| `:` include after another entry | `BaseNotFirst` |
| `:foo()`, `:@W`, `:Foo` | `BaseNotString` |
| Integer > `i128::MAX` | `NumberOutOfRange` (a `u128` field above `i128::MAX` is unsupported; §11.7) |
| Float overflow (`1e400`) | `f64::INFINITY` (Rust `FromStr` semantics), no error |
| `-0.0` | `Float(-0.0)`, sign preserved |
| Nesting > 128 | `NestingTooDeep` |
| Unpaired `)`/`]`/`}` | `UnexpectedToken` at the delimiter, `expected` lists what could follow |
| Non-UTF-8 bytes | not reachable — the loader passes `&str` (SPEC-5 handles `Utf8Error`) |
| Numeric field names (`Foo { 0: 1.0 }`) | `UnexpectedToken` expecting an identifier (§11.8) |

---

## 9. Parser specification (`parser.rs`)

### 9.1 Why recursive descent (deviation from pcwalton)

pcwalton used LALRPOP (`dynamic_bsn.rs:36`). This spec mandates a hand-written
recursive-descent parser instead:

1. **No build dependency / no build script.** LALRPOP adds a `build.rs`, a codegen step, and
   a new crate to Bevy's dependency tree — a hard sell upstream for a first-party loader.
2. **Diagnostics.** §5.5 requires 14 construct-specific messages keyed on *structure plus
   casing*. In an LR parser those are conflict-prone error productions; in recursive descent
   each is three lines at the point of decision.
3. **SPEC-0 §6 decision 1** already mandates a custom lexer + parser ("differences manifest;
   less runtime code" — cart, #23576).
4. **Contract D fixes only the AST**, not the parsing technique, so this deviation does not
   affect SPEC-4/5.
5. A junior engineer can implement, debug and extend recursive descent from this spec; an LR
   grammar requires understanding conflict reports.

Logged as open question §11.1 for upstream alignment with pcwalton's branch.

### 9.2 Driver

```rust
pub fn parse(source: &str) -> Result<BsnDocument, BsnParseError>;

struct Parser<'src> {
    source: &'src str,
    tokens: Vec<Token>,   // pre-lexed, ends with exactly one Eof
    pos: usize,
    depth: u32,
    nodes: Vec<Option<BsnNode>>,
    values: Vec<Option<BsnValueNode>>,
}
```

Primitives (all infallible except `expect`):

| Fn | Meaning |
| --- | --- |
| `peek() -> TokenKind` / `peek_at(n)` | lookahead without consuming (`Eof` past the end) |
| `peek_token() -> Token` | current token with span |
| `bump() -> Token` | consume and return |
| `eat(kind) -> bool` | consume iff `peek() == kind` |
| `expect(kind, expected: &'static [&'static str]) -> Result<Token, _>` | consume or `UnexpectedToken` |
| `err_here(kind, expected) -> BsnParseError` | error at the current token's span |
| `unsupported(msg, span) -> BsnParseError` | `Unsupported(msg)` |
| `check_error_token() -> Result<(), _>` | if `peek()` is `TokenKind::Error(e)`, map `e` to the corresponding `BsnParseErrorKind`/`Unsupported` and return it. **Called at the top of `bump`/`expect`** so lexical errors surface at their exact span. `LexError::Closure` → `unsupported::CLOSURE`, `Macro` → `MACRO`, `CharLiteral` → `CHAR`, `NumericSuffix` → `SUFFIX`, `RawIdentifier` → `RAW_IDENT`, others → the matching kind. |
| `enter()/leave()` | depth guard, `NestingTooDeep` |
| `token_desc(kind) -> &'static str` | `"`,`"`, `"identifier"`, `"number"`, `"end of file"`, … for messages |

### 9.3 Parse functions (one per production)

Each function is listed with its production, its entry condition and its exact behavior.

**`parse_document() -> Result<BsnDocument, _>`** — `document = [ entity_list ] EOF`

1. If `peek() == Eof` return the empty document.
2. `roots = parse_entity_list(Eof)?`.
3. `expect(Eof, &["`,`", "end of file"])`.
4. `finish()`.

**`parse_entity_list(term: TokenKind) -> Result<Vec<BsnNodeId>, _>`** —
`entity_list = entity { "," entity } [ "," ]`
Loop: if `peek() == term` break; push `parse_entity()?`; if `eat(Comma)` continue else break.
After the loop `expect`/`break` is left to the caller (the caller consumes `term`).
An empty list is legal (`[]`, empty file).

**`parse_entity() -> Result<BsnNodeId, _>`** — `entity = "(" entity_body ")" | entity_body`
`enter()`; reserve `id = alloc_node()`; if `eat(LParen)` then `parse_entity_body(id, RParen)`
and `expect(RParen, …)`, else `parse_entity_body(id, <caller terminator set>)`; span =
first token start .. last consumed token end; `leave()`.
The flat form stops at `Comma`, `RBracket`, `RParen` or `Eof` (mirrors `parse.rs:85-89`).

**`parse_entity_body(id, stop) -> Result<(), _>`** — `entity_body = [ base ] { entry }`

1. If `peek() == Colon`: consume; `expect(Str, &["string literal"])` — a non-`Str` token is
   `BaseNotString`; store `base`/`base_span`.
2. Loop until `peek()` is in the stop set: `parse_entry(id)?`.
3. `parse_entry` writes into local `name`, `patches`, `relations`, which `finish_node` stores
   as `BsnNodeKind::Entity`.

**`parse_entry(entity: &mut EntityBuilder) -> Result<(), _>`** — `entry = name_entry | relation_entry | patch_entry`
Dispatch on `peek()`:

- `Colon` ⇒ `BaseNotFirst`.
- `Hash` ⇒ consume, `expect(Ident)`; if `entity.name.is_some()` ⇒ `DuplicateName`; store.
- `LBrace` ⇒ `unsupported::EXPR`.
- `Tilde` ⇒ consume, `parse_patch(BsnPatchPrefix::Template)`.
- `At` ⇒ consume, `parse_patch(BsnPatchPrefix::SceneComponent)`.
- `Ident` ⇒ (a) if the ident is `use` ⇒ `unsupported::USE`; (b) if the ident is `on` and
  `peek_at(1) == LParen` ⇒ `unsupported::OBSERVER`; (c) otherwise `parse_patch(FromTemplate)`.
- anything else ⇒ `UnexpectedToken` with `expected = ["type path", "`#`", "`~`", "`@`"]`.

**`parse_patch(prefix) -> Result<(), _>`** — `patch_entry`, `relation_entry`

1. `path = parse_path()?`.
2. `classify_entry_path(&path)?` — §5.5 rules #5/#6/#7: last segment lowercase-initial ⇒
   `FN` (or `CTOR` if the previous segment is uppercase-initial and the next token is
   `LParen`); `is_const(last)` ⇒ `CONST`.
3. If `peek() == LBracket`: consume; `entities = parse_entity_list(RBracket)?`;
   `expect(RBracket)`; reserve+finish a `Relation` node; push to `entity.relations`.
   (A `~`/`@` prefix before a relation is `UnexpectedToken`: "relationships cannot be
   prefixed with `~` or `@`".)
4. Else: `value = parse_patch_value(path)?`; finish a `Patch` node; push to `entity.patches`.

**`parse_patch_value(path) -> Result<BsnValueId, _>`**
`LBrace` ⇒ `Struct(path, parse_struct_body()?)`; `LParen` ⇒ `NamedTuple(path, parse_tuple_body()?)`;
otherwise ⇒ `Path(path)`. Upholds §7.5.

**`parse_struct_body() -> Result<Vec<(String, BsnValueId)>, _>`** — `struct_body`
`expect(LBrace)`; loop until `RBrace`:

- `At` ⇒ `unsupported::PROP`.
- `expect(Ident)` → name; if `peek() != Colon` ⇒ `unsupported::SHORTHAND`;
  `expect(Colon)`; `value = parse_value()?`; reject a repeated name with `DuplicateField`.
- `eat(Comma)` or break.
`expect(RBrace)`.

**`parse_tuple_body() -> Result<Vec<BsnValueId>, _>`** — `tuple_body`
`expect(LParen)`; loop until `RParen`: `parse_value()`, then `eat(Comma)` or break;
`expect(RParen)`.

**`parse_value() -> Result<BsnValueId, _>`** — `value`
`enter()`; reserve `id = alloc_value()`; dispatch on `peek()`:

| Token | Result |
| --- | --- |
| `Int` | `Int(decode_int(..)?)` |
| `Float` | `Float(decode_float(..)?)` |
| `Str` | `String(decode_string(..)?)` |
| `Minus` | consume, then `Int`/`Float` ⇒ negated (`Int` uses `i128::checked_neg`), `Ident("inf")` ⇒ `Float(f64::NEG_INFINITY)`, else `NegOperand` |
| `Hash` | consume, `expect(Ident)` ⇒ `EntityRef(text)` |
| `LBracket` | `List(parse_value_list(RBracket)?)` |
| `LParen` | `parse_paren_value()` (below) |
| `LBrace` | `unsupported::EXPR` |
| `Ident` | `true`/`false` ⇒ `Bool`; `inf` ⇒ `Float(INFINITY)`; `NaN`/`nan` ⇒ `Float(NAN)`; `const`/`unsafe` with `peek_at(1)==LBrace` ⇒ `unsupported::EXPR`; otherwise `parse_path_value()` |
| anything else | `UnexpectedToken`, `expected = ["value"]` with the full list in `expected` |

`leave()`; finish the value with the joined span.

**`parse_paren_value()`** — `paren_value`
Consume `LParen`. If `eat(RParen)` ⇒ `Unit`. Parse one value `v`. If `eat(RParen)` ⇒ **the
value `v` itself** (grouping — the reserved id becomes an alias: `finish_value(id, span,
document.values[v].value.clone())`; simpler and cheaper: return `v` and mark the reserved id
unused — instead, **do not reserve before the dispatch for `LParen`**; `parse_paren_value`
reserves only once it knows it needs a node). Otherwise `expect(Comma)`, collect the rest
until `RParen` ⇒ `Tuple([v, …])`; `(v,)` ⇒ `Tuple([v])`.
*Implementation note:* to keep ids gap-free, `parse_value` reserves its id **after** the
`LParen` branch decides it is `Unit`/`Tuple`; the grouping case returns the inner id.

**`parse_path_value()`** — `path_value`
`path = parse_path()?`; `classify_value_path(&path)?` (§5.5 #6/#7/#5→`PATH_CASE`);
then `LBrace` ⇒ `Struct`, `LParen` ⇒ `NamedTuple`, else `Path`.

**`parse_path() -> Result<BsnPath, _>`** — `path`
`enter()`; reject a leading `ColonColon` with `LeadingPathSeparator`;
`segments = [parse_path_segment()?]`; while `eat(ColonColon)` push another segment.
`leave()`.

**`parse_path_segment()`** — `path_segment`
`expect(Ident)`; if `eat(Lt)` parse `path { "," path } [ "," ]` then `expect(Gt)`.
(There is no `>>` token, so `Vec<Vec<f32>>` lexes as two `Gt` — no splitting logic needed.)

**`parse_value_list(term)`** — comma-separated `value`s with optional trailing comma.

### 9.4 Determinism

The parser uses no hash maps (duplicate-field detection scans the small `Vec` of already-seen
names) and no iteration over unordered collections, so output is byte-identical for identical
input. This is asserted by a test (§11.5, `parse_is_deterministic`).

---

## 10. Step-by-step implementation plan

Each step compiles and its tests pass before the next begins.

1. **Crate skeleton.** `crates/bevy_bsn/` with the §4.2 manifest, the two LICENSE files, a
   stub `README.md`, `src/lib.rs` with the §4.3 header and the §4.4 module/re-export block,
   and empty modules. `cargo check -p bevy_bsn` and
   `cargo check -p bevy_bsn --no-default-features --target x86_64-unknown-none` both pass
   from this step onward (add the CI line now, so `no_std` breakage is caught immediately —
   retrofitting `no_std` is far more expensive than maintaining it).
2. **`Span` + `error.rs`.** `Span::{new,NONE,line_col,text,join,is_none}`, `BsnParseError`,
   `BsnParseErrorKind`, the `unsupported` message constants, `render`. Tests: `span_line_col`,
   `render_points_at_the_right_column`.
3. **`lexer.rs` — trivia + punctuation + identifiers.** Table tests for §6.3 rows 1–10.
4. **`lexer.rs` — strings** (normal, raw, escapes, unterminated) and `decode_string`.
5. **`lexer.rs` — numbers** and `decode_int`/`decode_float`, including suffix rejection.
6. **`ast.rs`.** All types, `to_type_path`, `from_type_path`, `parent_type_path`,
   `debug_tree`, `structural_eq`, and the §7.10 builder methods. Unit tests for
   `to_type_path`/`from_type_path` with generics and for `structural_eq` float handling.
7. **`parser.rs` — paths and values only.** A temporary `pub(crate) fn parse_value_str` used
   by tests. Covers §9.3 value functions.
8. **`parser.rs` — entities, patches, relations, document.** All §12 corpus examples parse.
9. **`parser.rs` — rejection diagnostics.** All 14 §5.5 rules with their exact constants.
10. **`printer.rs`** — `write_document_with`, `PrintOptions`, `print_document`; then the
    §11.3 round-trip suite, including printing a *builder-constructed* document.
11. **README.md** (§4.5) with the read and write examples, wired via
    `#![doc = include_str!("../README.md")]`; `cargo test -p bevy_bsn --doc` must pass, so the
    README examples are real, compiling code.
12. **Hygiene + determinism tests** (§11.5), doc comments on every public item,
    `cargo clippy -p bevy_bsn --all-features -- -D warnings`, `cargo doc -p bevy_bsn`.
13. **Consumer wiring** (belongs to SPEC-5, listed here for ordering): add the optional
    `bevy_bsn` dependency and `bsn_asset` feature to `bevy_scene` per §4.6.

---

## 11. Test plan (`crates/bevy_bsn/src/tests.rs`, `#[cfg(test)]`)

All tests live in `crates/bevy_bsn/src/tests.rs` unless noted; lexer-internal tests may live
at the bottom of `lexer.rs`. Tests use only `core`/`alloc` plus `std` for the harness —
no dev-dependencies (§4.2).

### 11.1 Table-driven lexer tests

```rust
fn lex_kinds(src: &str) -> Vec<TokenKind>;                 // helper, drops Eof
fn lex_spanned(src: &str) -> Vec<(TokenKind, u32, u32)>;   // helper
```

| Test | Asserts |
| --- | --- |
| `lex_punctuation_table` | table of 16 rows, one per punctuation token, each `"x"` → `[Kind]` with span `(0, n)` |
| `lex_colon_vs_coloncolon` | `":: :"` → `[ColonColon, Colon]`; `"a::b"` → `[Ident, ColonColon, Ident]` |
| `lex_idents_table` | `_a`, `A1`, `étoile`, `r` (bare) → `Ident`; spans exact |
| `lex_int_table` | `0`, `12`, `1_000`, `0xFF`, `0b1010`, `0o17` → `Int`; `decode_int` values `0,12,1000,255,10,15` |
| `lex_float_table` | `1.0`, `1.`, `1e5`, `2.5E-3`, `1_0.5` → `Float`; decoded values |
| `lex_number_suffix_rejected` | `1u8`, `1.0f32`, `0x1g` → `Error(NumericSuffix)` spanning the whole token |
| `lex_leading_dot_is_not_float` | `.5` → `[Error(Unknown), Int]` |
| `lex_string_table` | `"a"`, `"a\nb"`, `"\u{1F600}"`, `"\x41"`, `r"a\b"`, `r#"a"b"#` → `Str`; `decode_string` values `a`, `a\nb`, `😀`, `A`, `a\\b`, `a"b` |
| `lex_string_errors` | `"abc` → `UnterminatedString`; `"\q"` → `InvalidEscape`; `r#"x"` → `UnterminatedRawString`; spans exact |
| `lex_comments_table` | `// x\nA`, `/* x */A`, `/* /* */ */A`, `///doc\nA` all → `[Ident]` with the `A` span; `/*` alone → `UnterminatedBlockComment` |
| `lex_trivia_and_bom` | `"\u{FEFF}A"` → `[Ident]` at span `(3,4)`; CRLF, tabs skipped |
| `lex_rejected_chars_table` | `'`→`CharLiteral`, `\|`→`Closure`, `!`→`Macro`, `r#x`→`RawIdentifier`, `=`,`;`,`&`,`.`→`Unknown`; each consumes ≥1 char |
| `lex_never_loops` | fuzz-ish: for every `char` in a fixed 200-char sample string, `tokenize` terminates and the last token is `Eof` |
| `lex_eof_span_is_end_of_file` | `Eof.span == (len, len)` |

### 11.2 Parser corpus / snapshot tests

One test per corpus document in §12, named `parse_corpus_<n>_<slug>`, each asserting
`doc.debug_tree() == EXPECTED` against the inline string given in §12. `debug_tree` format
(also specified here because tests depend on it byte-for-byte):

```text
Entity#<id> name=<"Name"|-> base=<"path"|->
  Patch#<id> <patch|template|scene> `<type path>` value=$<id>
  Relation#<id> `<type path>`
    <nested Entity lines, indented by 2>
$<id> <value>
```

Value rendering: `Unit`, `Bool(true)`, `Int(-3)`, `Float(1.0)` (`{:?}`, with `inf`/`-inf`/`NaN`),
`Str("…")` (re-escaped), `Path(a::B)`, `EntityRef(Name)`, `Tuple`, `List`,
`Struct(a::B)`, `NamedTuple(a::B)`; children of a value are printed on following lines
indented by 2, struct fields prefixed `field <name> = $<id>`. Values are printed in a flat
`values:` section after the node section, in ascending id.

Additional structural tests:

| Test | Asserts |
| --- | --- |
| `parse_empty_document` | `""` and `"// only a comment"` → `roots.is_empty()` |
| `parse_node_ids_are_preorder` | for corpus 3, `roots == [BsnNodeId(0)]` and the child entity ids ascend in source order |
| `parse_node_ids_stable_across_reparse` | parse the same text twice → `debug_tree()` equal, and each entity id equal |
| `parse_patch_value_invariant` | for every corpus doc, every `Patch`'s value is `Path`/`Struct`/`NamedTuple` with a path equal to `symbol` |
| `parse_multiple_patches_same_component` | `A A { x: 1 }` → two `Patch` nodes, both symbol `A` |
| `parse_spans_cover_source` | every node/value span is within `0..src.len()` and `start <= end`; entity spans contain their children's spans |
| `parse_relation_and_patch_source_order` | merging `patches` + `relations` by `span.start` reproduces source order for corpus 3 |

### 11.3 Printer and round-trip tests (the write side)

Because external tools depend on the printer as much as on the parser, this suite is as
large as the parser suite.

| Test | Asserts |
| --- | --- |
| `roundtrip_corpus_structural` | for each §12 corpus doc `d`: `parse(&print_document(&d)).structural_eq(&d)` — the normative property of §7.9 |
| `roundtrip_corpus_debug_tree` | same, but compares `debug_tree()` strings, so a failure diff is readable |
| `print_is_idempotent` | `print(parse(print(parse(s)))) == print(parse(s))` byte-for-byte, for every corpus doc — the printer is a formatter with a fixed point |
| `print_snapshot_corpus` | each corpus doc's canonical text equals an inline expected string (locks the §7.9 formatting rules; the one test that must be updated deliberately if the style changes) |
| `print_builder_document` | a document built **only** through the §7.10 builder (all spans `Span::NONE`) prints, re-parses, and is `structural_eq` to the builder output — the Blender/Unity exporter path, exercised end-to-end |
| `print_entry_order_from_spans` | for a parsed doc where a relation sits *between* two patches, the printed order matches the source order (span-merge rule §7.9.3) |
| `print_entry_order_from_ids` | same document built with `Span::NONE` prints in builder (id) order |
| `print_inline_vs_multiline` | a body under `max_inline_width` prints on one line; one over it prints one item per line with a trailing comma; `PrintOptions { max_inline_width: 0, .. }` forces multi-line everywhere |
| `print_string_escapes` | `"a\"b\\c\n\t\0\u{7}\u{1F600}"` prints with minimal escapes (emoji verbatim, `\u{7}` escaped) and re-parses identically |
| `print_non_finite_floats` | `inf`, `-inf`, `NaN` print as `inf`/`-inf`/`NaN` and re-parse; `NaN` compared with `is_nan()`; `-0.0` keeps its sign bit |
| `print_float_keeps_decimal_point` | `Float(1.0)` prints `1.0` (not `1`), so it re-parses as `Float`, not `Int` |
| `print_int_normalizes_radix` | `0xFF` prints as `255`; `1_000` prints as `1000` (documented normalization, §7.8) |
| `print_generic_paths` | `alloc::vec::Vec<f32>` and `A<B<C>, D>` survive; `to_type_path()` is `"A<B<C>, D>"`; `BsnPath::from_type_path` inverts it |
| `print_empty_document` | prints `""`, which re-parses to zero roots |
| `print_multi_root` | corpus §12.8 prints with `,` + blank line between roots and no trailing comma |
| `print_nested_relations` | 3 levels of `Children [ … ]` indent by 4 per level and re-parse |
| `print_never_panics_on_dangling_id` | a hand-built document with a `BsnValueId` past the end prints an `/* <invalid node id N> */` marker instead of panicking |
| `print_output_ends_with_newline` | non-empty output ends with exactly one `\n`; no `\r` anywhere |

### 11.4 Rejected-construct diagnostics

`fn err(src: &str) -> BsnParseError` helper. One test per §5.5 rule, named
`reject_<code>`, asserting **the exact `BsnParseErrorKind::Unsupported` constant** and the
span:

| Test | Input | Expects |
| --- | --- | --- |
| `reject_expr_entry` | `{ my_scene() }` | `unsupported::EXPR`, span of `{` |
| `reject_expr_value` | `A { x: { 1 + 2 } }` | `unsupported::EXPR` |
| `reject_expr_const_block` | `A { x: const { 1 } }` | `unsupported::EXPR` |
| `reject_closure` | `A { x: \|c\| { 1 } }` | `unsupported::CLOSURE` |
| `reject_observer` | `on(my_obs)` | `unsupported::OBSERVER` |
| `reject_scene_fn` | `my_scene()` and `my_scene` | `unsupported::FN` |
| `reject_ctor` | `A { x: Color::srgb(1.0, 0.0, 0.0) }` | `unsupported::CTOR` |
| `reject_const_bare` | `A { x: PI }` | `unsupported::CONST` |
| `reject_const_assoc` | `A { x: f32::MAX }` | `unsupported::CONST` (note: `f32::MAX` also trips `FN`-casing; `is_const` on the *last* segment wins — assert `CONST`) |
| `reject_shorthand` | `A { width }` | `unsupported::SHORTHAND` |
| `reject_prop` | `@W { @prop: 1 }` | `unsupported::PROP` |
| `reject_macro` | `A { x: vec![1] }` | `unsupported::MACRO` |
| `reject_use` | `use a::B;` | `unsupported::USE` |
| `reject_char` | `A { x: 'c' }` | `unsupported::CHAR` |
| `reject_suffix` | `A { x: 1.0f32 }` | `unsupported::SUFFIX` |
| `reject_raw_ident` | `r#type` | `unsupported::RAW_IDENT` |
| `reject_lowercase_value_path` | `A { x: foo::bar }` | `unsupported::PATH_CASE` |
| `reject_base_not_first` | `A :"b.bsn"` | `BaseNotFirst` |
| `reject_base_not_string` | `:enemy()` | `BaseNotString` |
| `reject_duplicate_name` | `#A #B` | `DuplicateName` |
| `reject_duplicate_field` | `A { x: 1, x: 2 }` | `DuplicateField("x")` |
| `reject_neg_operand` | `A { x: -B }` | `NegOperand` |
| `reject_leading_path_sep` | `::a::B` | `LeadingPathSeparator` |
| `reject_int_out_of_range` | 40 nines | `NumberOutOfRange` |
| `reject_nesting_too_deep` | 200 nested `[` | `NestingTooDeep` (and the test must not overflow the stack) |
| `reject_unclosed_brace` | `A { x: 1` | `UnexpectedEof`, `expected` contains `"`,`"` and `"`}`"` |
| `reject_tilde_relation` | `~Children [ ]` | `UnexpectedToken` mentioning `~`/`@` |
| `error_messages_end_with_a_remedy` | (all constants) | every `unsupported::*` constant contains "`.bsn`" and is ≥ 40 chars |

### 11.5 Hygiene tests (crate-independence, `no_std`, determinism)

| Test | Asserts |
| --- | --- |
| `manifest_has_no_bevy_dependencies` | `include_str!("../Cargo.toml")` contains no `bevy_` outside the `[package] name` line, and lists exactly one dependency (`thiserror`). This is the machine-checked form of the §4.2 dependency policy. |
| `sources_have_no_bevy_or_std_references` | `include_str!` each of the six source files; none contains `"bevy_"`, `"std::"` or `"use std"` (the `extern crate std` line in `lib.rs` is the sole allowed occurrence and is matched exactly) |
| `parse_is_deterministic` | parsing corpus §12.8 ten times yields identical `debug_tree()` |
| `parse_never_panics_on_truncation` | for corpus §12.8, every prefix `&src[..n]` (n on char boundaries) either parses or returns `Err` — never panics |
| `parse_never_panics_on_byte_flip` | for a fixed seed, 500 random single-char substitutions in corpus §12.8 never panic |
| `print_never_panics_on_fuzzed_ast` | for a fixed seed, 200 randomly-mutated ASTs (swapped ids, cleared vecs) print without panicking |

`no_std` itself is verified by the CI command in §4.3, not by a unit test: a `no_std` build
failure is a compile error, which no in-crate test can observe.

---

## 12. Worked corpus

Each example is a complete `.bsn` document. `debug_tree` output is given in the format of
§11.2 (values section abbreviated with `…` only where explicitly noted).

### 12.1 Minimal entity

```bsn
bevy_transform::components::transform::Transform
```

```text
Entity#0 name=- base=-
  Patch#1 patch `bevy_transform::components::transform::Transform` value=$0
values:
$0 Path(bevy_transform::components::transform::Transform)
```

### 12.2 Struct patch with nested struct value, negatives, partial fields

```bsn
#Camera
bevy_camera::components::Camera3d
bevy_transform::components::transform::Transform {
    translation: glam::Vec3 { x: 0.0, y: 6.0, z: -12.5 },
    scale: glam::Vec3 { x: 1.0 },
}
```

```text
Entity#0 name="Camera" base=-
  Patch#1 patch `bevy_camera::components::Camera3d` value=$0
  Patch#2 patch `bevy_transform::components::transform::Transform` value=$1
values:
$0 Path(bevy_camera::components::Camera3d)
$1 Struct(bevy_transform::components::transform::Transform)
  field translation = $2
  field scale = $6
$2 Struct(glam::Vec3)
  field x = $3
  field y = $4
  field z = $5
$3 Float(0.0)
$4 Float(6.0)
$5 Float(-12.5)
$6 Struct(glam::Vec3)
  field x = $7
$7 Float(1.0)
```

Note `scale` sets only `x`; `y`/`z` keep the value from earlier patches or `Default`
(SPEC-4). `#Camera` sets `name`, not a `Name` patch — SPEC-4 emits the `Name` component.

### 12.3 Nested children, relations, entity references

```bsn
#Root
bevy_ui::ui_node::Node
bevy_ecs::hierarchy::Children [
    #Label
    bevy_ui::widget::text::Text("hello"),
    my_game::Follower { target: #Root }
]
```

```text
Entity#0 name="Root" base=-
  Patch#1 patch `bevy_ui::ui_node::Node` value=$0
  Relation#2 `bevy_ecs::hierarchy::Children`
    Entity#3 name="Label" base=-
      Patch#4 patch `bevy_ui::widget::text::Text` value=$1
    Entity#5 name=- base=-
      Patch#6 patch `my_game::Follower` value=$3
values:
$0 Path(bevy_ui::ui_node::Node)
$1 NamedTuple(bevy_ui::widget::text::Text)
  $2
$2 Str("hello")
$3 Struct(my_game::Follower)
  field target = $4
$4 EntityRef(Root)
```

Ids show the pre-order rule: the relation (#2) precedes its children (#3, #5), and the second
child's id follows the first child's whole subtree. `#Root` is visible to descendants
(`crates/bevy_scene/src/lib.rs:193-201`); resolving the reference is SPEC-4's job.

### 12.4 Inheritance (`:base`) plus overriding patches

```bsn
:"enemies/orc.bsn"
my_game::Health { max: 200 }
bevy_ecs::hierarchy::Children [
    my_game::Weapon
]
```

```text
Entity#0 name=- base="enemies/orc.bsn"
  Patch#1 patch `my_game::Health` value=$0
  Relation#2 `bevy_ecs::hierarchy::Children`
    Entity#3 name=- base=-
      Patch#4 patch `my_game::Weapon` value=$2
values:
$0 Struct(my_game::Health)
  field max = $1
$1 Int(200)
$2 Path(my_game::Weapon)
```

Semantics (SPEC-4): the base is included **first** as a `CachedSceneAsset`, then the patches
apply on top — identical to `bsn! { :"enemies/orc.bsn" Health { max: 200 } }`
(`macros/src/bsn/parse.rs:101-102`, `crates/bevy_scene/src/lib.rs:388-395`).

### 12.5 Enum variants — all three kinds

```bsn
bevy_camera::visibility::Visibility::Visible
my_game::Shape::Circle { radius: 2.5 }
my_game::Shape::Rect(1.0, 2.0)
```

```text
Entity#0 name=- base=-
  Patch#1 patch `bevy_camera::visibility::Visibility::Visible` value=$0
  Patch#2 patch `my_game::Shape::Circle` value=$1
  Patch#3 patch `my_game::Shape::Rect` value=$3
values:
$0 Path(bevy_camera::visibility::Visibility::Visible)
$1 Struct(my_game::Shape::Circle)
  field radius = $2
$2 Float(2.5)
$3 NamedTuple(my_game::Shape::Rect)
  $4
  $5
$4 Float(1.0)
$5 Float(2.0)
```

**The parser does not know these are enum variants** (§7.6). SPEC-4 tries
`get_with_type_path("my_game::Shape::Circle")`, fails, then
`get_with_type_path("my_game::Shape")` + variant `Circle`
(`dynamic_bsn.rs:863-881`). Note that patches #2 and #3 target the same component and the
later one wins for the whole value — enum patching replaces the variant.

### 12.6 Partial tuple patch, list value, string→Handle, unit and tuple values

```bsn
my_game::Rgba(1.0, 0.5)
bevy_render::mesh::components::Mesh3d("models/tree.gltf#Mesh0/Primitive0")
my_game::Waypoints([1.0, 2.0, 3.0])
my_game::Marker(())
my_game::Pair((1, 2))
```

```text
Entity#0 name=- base=-
  Patch#1 patch `my_game::Rgba` value=$0
  Patch#2 patch `bevy_render::mesh::components::Mesh3d` value=$3
  Patch#3 patch `my_game::Waypoints` value=$5
  Patch#4 patch `my_game::Marker` value=$10
  Patch#5 patch `my_game::Pair` value=$12
values:
$0 NamedTuple(my_game::Rgba)
  $1
  $2
$1 Float(1.0)
$2 Float(0.5)
$3 NamedTuple(bevy_render::mesh::components::Mesh3d)
  $4
$4 Str("models/tree.gltf#Mesh0/Primitive0")
$5 NamedTuple(my_game::Waypoints)
  $6
$6 List
  $7
  $8
  $9
$7 Float(1.0)
$8 Float(2.0)
$9 Float(3.0)
$10 NamedTuple(my_game::Marker)
  $11
$11 Unit
$12 NamedTuple(my_game::Pair)
  $13
$13 Tuple
  $14
  $15
$14 Int(1)
$15 Int(2)
```

`Rgba(1.0, 0.5)` sets tuple fields 0 and 1 and leaves the rest defaulted (partial leading
prefix, §5.3). The `Mesh3d` string becomes a `Handle` in SPEC-4 via `ReflectConvert`
(SPEC-0 §6 decision 8); `#Mesh0/Primitive0` inside the string is *not* an entity reference —
it is inside a string literal. **`[ … ]` as a value is the one deliberate superset over
`bsn!`** (the macro spells this `{ vec![1.0, 2.0, 3.0] }`); see §11.3 open question.

### 12.7 Template (`~`) and scene-component (`@`) patches

```bsn
~bevy_asset::handle::HandleTemplate<bevy_image::image::Image>("icon.png")
@my_game::HealthBar { width: 100 }
```

```text
Entity#0 name=- base=-
  Patch#1 template `bevy_asset::handle::HandleTemplate<bevy_image::image::Image>` value=$0
  Patch#2 scene `my_game::HealthBar` value=$2
values:
$0 NamedTuple(bevy_asset::handle::HandleTemplate<bevy_image::image::Image>)
  $1
$1 Str("icon.png")
$2 Struct(my_game::HealthBar)
  field width = $3
$3 Int(100)
```

`~` means "this path is already a `Template`, do not go through `FromTemplate`"
(`macros/src/bsn/parse.rs:109-112`, consumed by SPEC-4 as pcwalton's `is_template` flag,
`dynamic_bsn.rs:1010-1033`). `@` names a `SceneComponent`; the grammar accepts it, but
*invoking* `SceneComponent::scene(props)` from an asset is a SPEC-0 non-goal — SPEC-4
decides whether to support the component-only part or return an error. Note the generic
argument in the path and that `>>`-style nesting needs no special lexing.

### 12.8 Multi-root document, parentheses, comments, non-finite floats, all literal forms

```bsn
// Two roots. The first is parenthesized, the second is flat.
(
    #Left
    my_game::Link { other: #Right }
),
#Right
my_game::Link { other: #Left }
my_game::Sensor {
    enabled: true,            /* block comment */
    threshold: inf,
    bias: -inf,
    seed: 0xFF,
    label: r#"raw "quoted" text"#,
    fallback: NaN,
}
```

```text
Entity#0 name="Left" base=-
  Patch#1 patch `my_game::Link` value=$0
Entity#2 name="Right" base=-
  Patch#3 patch `my_game::Link` value=$2
  Patch#4 patch `my_game::Sensor` value=$4
values:
$0 Struct(my_game::Link)
  field other = $1
$1 EntityRef(Right)
$2 Struct(my_game::Link)
  field other = $3
$3 EntityRef(Left)
$4 Struct(my_game::Sensor)
  field enabled = $5
  field threshold = $6
  field bias = $7
  field seed = $8
  field label = $9
  field fallback = $10
$5 Bool(true)
$6 Float(inf)
$7 Float(-inf)
$8 Int(255)
$9 Str("raw \"quoted\" text")
$10 Float(NaN)
```

`roots == [BsnNodeId(0), BsnNodeId(2)]`. Sibling roots share one name scope, mirroring
`bsn_list!` (`crates/bevy_scene/src/lib.rs:225-236`), so the mutual `#Left`/`#Right`
references are valid — SPEC-4 enforces that.

### 12.9 Error-case documents

The full set of one-line error documents is the §11.4 table: each row's *input* column is a
complete `.bsn` document, and its *expects* column is the exact error kind (message text in
§5.5). They are not repeated here.

Rendered example — `render("A { width }", Some("assets/player.bsn"))`:

```text
error: Field shorthand (`{ name }`) is not supported in `.bsn` assets, because there are no
variables to capture. Write `name: <value>` instead.
  --> assets/player.bsn:1:5
   |
 1 | A { width }
   |     ^^^^^
```

---

## 13. Acceptance criteria

1. `cargo test -p bevy_bsn` passes; every test in §11 exists with the given name and
   assertion. `cargo test -p bevy_bsn --doc` passes (the README examples compile and run).
2. `cargo check -p bevy_bsn --no-default-features --target x86_64-unknown-none` compiles —
   the crate is genuinely `no_std`, and the CI job in §4.3 keeps it that way.
3. `crates/bevy_bsn/Cargo.toml` lists exactly one dependency (`thiserror`) and no
   `bevy_*` dependency, dev-dependency or build-dependency
   (`manifest_has_no_bevy_dependencies`); no source file references `bevy_` or `std::`
   (`sources_have_no_bevy_or_std_references`).
4. `cargo clippy -p bevy_bsn --all-features -- -D warnings` is clean; no `unwrap`/`expect`/
   `panic!`/`unreachable!`/`todo!` outside `#[cfg(test)]`; `#![forbid(unsafe_code)]`.
5. Every public item has a doc comment; `cargo doc -p bevy_bsn` emits no `missing_docs`
   warnings; the crate-level docs come from `README.md`.
6. All eight corpus documents in §12.1–§12.8 parse to exactly the trees shown, and every
   §11.4 input produces exactly the error shown.
7. The printer satisfies the §7.9 round-trip property for every corpus document **and** for a
   document built only through the §7.10 builder API (`print_builder_document`), and is
   idempotent (`print_is_idempotent`).
8. Node ids are pre-order and byte-stable across re-parses (`parse_node_ids_are_preorder`,
   `parse_node_ids_stable_across_reparse`).
9. Fuzz-lite tests (§11.5) show no panic on truncated or corrupted input, or on a malformed
   AST handed to the printer.
10. `cargo package -p bevy_bsn` succeeds and the resulting tarball builds standalone (no
    path-only deps, `README.md` and both LICENSE files included).
11. Every `.bsn` document accepted by this parser is also accepted by the `bsn!` macro with
    identical meaning, **except** list values `[ … ]` (§12.6) — reviewed manually against
    `macros/src/lib.rs:11-130` and recorded in the PR description.
12. `bevy_scene` compiles both with and without its `bsn_asset` feature once SPEC-5 lands the
    §4.6 wiring; nothing in `bevy_bsn` needs to change for that.

---

## 14. Open questions

1. **Recursive descent vs LALRPOP.** pcwalton's branch uses LALRPOP + a custom lexer.
   This spec mandates recursive descent (§9.1). If upstream keeps the LALRPOP grammar, the
   AST (Contract D) is unaffected and only `parser.rs` is replaced. *Owner: whoever merges
   with #23576.*
2. **`BsnValue::Struct` needs a path.** Contract D's sketch has
   `Struct(Vec<(String, BsnValueId)>)` with no path, which cannot represent
   `glam::Vec3 { x: 0.0 }`. SPEC-3 uses `Struct(BsnPath, Vec<(String, BsnValueId)>)`.
   Requires a one-line Contract D amendment.
3. **`is_template: bool` cannot represent `@`.** SPEC-3 uses `BsnPatchPrefix` with three
   variants and an `is_template()` accessor. Contract D amendment requested.
   Related: should `@Type` in an asset be a *parse* error instead, since SPEC-4 cannot invoke
   `SceneComponent::scene`? SPEC-0 §2 says the grammar accepts it, so the error lands in
   SPEC-4 — confirm the message wording there.
4. **List values are a superset of `bsn!`.** `.bsn` needs `[1, 2, 3]` because it has no
   `{ vec![…] }`. Proposal: teach the `bsn!` macro to accept `[ … ]` as a list value too, so
   the subset property becomes exact. Needs a separate upstream PR.
5. **Type-path syntax coverage.** `&str`, `[T; N]`, `(A, B)` and const generics
   (`Foo<3>`) are valid `bevy_reflect` type paths but are not expressible as `BsnPath`.
   They are unreachable for *component* symbols; they could appear as nested value type
   paths. Deferred until a real case appears.
6. **Comment/format fidelity.** The AST discards comments and layout, so an editor that
   round-trips a hand-written file will reformat it. Attaching leading/trailing trivia to
   nodes (or a green-tree/CST design) should be revisited when the editor track lands —
   it also interacts with the deferred `BsnAst(World)` projection.
7. **`u128` fields above `i128::MAX`** cannot be written (Contract D fixes `Int(i128)`).
   Fix would be `Int { value: u128, negative: bool }`. Unlikely to matter; recorded.
8. **Numeric tuple-field syntax** (`Foo { 1: x }`) would let authors patch a *later* tuple
   field without specifying earlier ones. `bsn!` cannot express it either. Deferred.
9. **Multi-error recovery.** v1 is fail-fast (§8.3). A future version could recover at
   entry/field/entity boundaries and return `Vec<BsnParseError>`; that changes Contract D's
   `parse` signature, so it should be decided before 0.20 ships if at all.
10. **Short type paths.** `Transform` (single segment) is accepted syntactically; whether
    SPEC-4 resolves it via `get_with_short_type_path` (ambiguity risk) or requires fully
    qualified paths is SPEC-4's call. The grammar deliberately does not decide.
11. **Format-version marker (conformance-review addition).** Discussion #14437 makes
    versioned imports (`use bevy@0.13::sprite::Sprite;`) a load-bearing part of the asset
    format's migration story; this series defers imports entirely, which leaves `.bsn`
    files with **no version anchor at all** — files written today cannot be auto-migrated
    by future tooling that needs to know what schema they were written against.
    Recommendation for upstream discussion: accept (and ignore) an optional leading
    `bsn <semver>;` pragma now, so v1 files self-describe and the eventual `use`-imports
    grammar has a reserved anchor point. Cheap to lex, zero semantic cost.
12. **Reserved syntax for descendant patching (conformance-review addition).** PR #23413's
    "near future" roadmap includes descendant patching (reaching into an inherited scene to
    patch its descendants). This grammar does not reserve any syntax for it. When upstream
    picks a form (e.g. `#Name > Component { … }` or nested selector blocks), it must not
    collide with `#Name` entity references or relation blocks; flagging so the eventual
    grammar extension is checked against §5's ambiguity rules.
13. **`@` symbol divergence vs pcwalton's draft (conformance-review finding).** pcwalton's
    `.bsn` format uses `@Type` for *template patches*; the `bsn!` macro (and this grammar)
    uses `~Type` for template patches and `@Type` for scene components. This spec follows
    the macro — the more defensible choice given #23413's macro↔asset cross-compatibility
    goal — but upstream must consciously pick one before both formats exist in the wild.
14. **Crate name.** `bevy_bsn` follows the workspace convention and signals provenance, but
    the crate is deliberately engine-agnostic, and a Blender plugin author depending on
    something called `bevy_*` may reasonably expect an engine. Alternatives: `bsn` (likely
    contested on crates.io), `bsn_format`, `bevy_bsn` + a thin `bsn` alias crate. This spec
    assumes `bevy_bsn`; the name must be settled **before the first publish**, since renaming
    a published crate is not possible. *Owner: the release/maintainer team.*
15. **Public lexer surface.** `Lexer`/`Token`/`TokenKind` are public (§4.4) for syntax
    highlighters and language servers, which locks token-level details into semver
    (mitigated by `#[non_exhaustive]`). If maintainers prefer a smaller API, the alternative
    is to gate them behind a non-default `lexer` feature. Decide at review time.
16. **`std`-only conveniences.** The `std` feature currently only forwards to
    `thiserror/std`. Candidates that would give it real content: `parse_reader(impl
    std::io::Read)`, `write_document_io(impl std::io::Write)`, and
    `impl From<BsnParseError> for std::io::Error`. Deliberately omitted from v1 to keep the
    API surface minimal; add on demand.

*(Resolved and removed: "should the parser be gated behind a `bevy_scene` cargo feature?"
— moot now that it is a separate crate. The only feature involved is `bevy_scene/bsn_asset`,
which gates the optional dependency and SPEC-4/5's modules, per §4.6.)*
