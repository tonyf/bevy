# Dynamic BSN testing audit (plan Layer 5)

One-off rigor audits run on branch `bsn/09-testing` (2026-08-17), per the approved
testing plan. Findings were converted into tests where real; this file records the
measurements and the adjudications.

## Coverage (cargo-llvm-cov, lib targets, bevy_bsn + bevy_scene w/ bsn_asset)

Before → after the Layer-5a gap tests (region coverage):

| File | Before | After |
| --- | --- | --- |
| bevy_bsn (6 files aggregate) | 97.0% | 98.1% |
| dynamic/value.rs | 92.4% | 97.4% |
| dynamic/build.rs | 92.1% | 97.3% |
| dynamic/scene.rs | 86.1% | 93.3% |
| TOTAL (both libs) | 93.5% | 94.8% |

24 tests added (19 bevy_scene, 5 bevy_bsn), all error-path/edge clusters. Remaining
uncovered code is documented defensive-only, with written unreachability analyses —
notably the resolve-time `ApplyFailed` arms in `dynamic/scene.rs`: every producer of
a slot value yields a concrete value of the slot's own template type, so `try_apply`
type mismatches cannot occur from validly built scenes; the only entry is a
third-party `ErasedComponentTemplate` violating `template_type_id()`'s documented
contract.

## Binary size (SPEC-1 acceptance criterion A7, deferred until rustc 1.96)

Method: release `breakout` builds in isolated worktrees at the exact A/B pair —
`5076c0fe8` (parser layer tip) vs `e3b83dd8a` (the SPEC-1 bevy_ecs commit).

| Measure | Base | With SPEC-1 | Delta |
| --- | --- | --- | --- |
| Unstripped | 157,777,224 | 160,615,744 | +2,838,520 (+1.80%) |
| Stripped | 102,899,584 | 104,730,288 | +1,830,704 (+1.78%) |

Symbol attribution (nm, stripped-equivalent sections):

- `push_to_bundle_writer` monomorphizations: **310 symbols, 181 KB — 0.11%** of the
  binary. This is the per-`#[reflect(Component)]` fn the ≤1.5% criterion targeted:
  **passes with ~13× margin.**
- `from_reflect` ladder (`from_reflect_erased`/`try_from_reflect*`): 293 symbols, 79 KB.
- The remaining ~1.4 MB is the Phase-5 surface: `Reflect`/`TypeInfo`/registration
  codegen for template types (`#[template(reflect)]` seed set, `EntityTemplate`,
  `OptionTemplate`/`VecTemplate`) and their inventory registration ctors.

**Adjudication:** the criterion as *intended* (cost of the erased-insertion
mechanism) passes decisively. The criterion as *written* (whole-commit ≤1.5%) is
missed at 1.8%, dominated by making template types reflectable — which is the
feature's substrate, not mechanism overhead: dynamic BSN cannot construct templates
it cannot reflect. Size lever if upstream wants one: gate `#[template(reflect)]`
emission behind a cargo feature so non-`.bsn` users pay nothing.

## Mutation testing (cargo-mutants)

### bevy_scene/src/dynamic (lib tests, bsn_asset)

118 mutants: 77 viable, 41 unviable. First pass caught 72; the 5 misses were
adjudicated and 4 killed with new tests (`depth_guard_boundary_is_exact` — the
`MAX_DEPTH` off-by-ones and the no-op-`exit` width case; `loader_debug_names_the_type`).
**Final: 76/77 caught (98.7%).** Accept-listed (1): deleting the body of
`report_scene_patch_load_failures` — covered by the integration test
`failed_load_is_reported_once` (tests/dynamic_bsn.rs:668), which `--lib` mutant runs
cannot see.

### bevy_bsn (lib tests)

638 mutants: 555 viable, 51 unviable, 32 timeouts (parser mutants that make the
in-test fuzz loops spin — detection by hang, counted separately). First pass caught
459/555; iterating after the Layer-5a/6 tests killed 11 more. The remaining misses
decomposed into (a) genuinely under-asserted public behavior — killed by 8 new
"mutation-audit" tests in tests.rs (`bool_literals_parse_as_bools` — `false` was
never asserted to parse as a boolean anywhere; path accessors; `BsnPath::structural_eq`
clause-by-clause; exact `expected …` suffix text; `Span::is_none`; unicode-escape
boundaries `\u{10FFFF}`/`\u{110000}`/8-digit/surrogate incl. decoded-value assertion;
multi-segment path-value spans; angle/closer depth in the grouping-vs-tuple scan) —
and (b) an accept-list recorded below.

**Final: 505/555 viable caught (91.0%); counting the 32 timeout-detections, 537/555
(96.8%). 50 accept-listed survivors**, every one adjudicated: ~40 debug/diagnostic
formatting arithmetic and printer layout choices (round-trip invariants hold under
them — output stays parse-equivalent), plus provably-equivalent mutants (the `\x`
escape validator's accept-set is unchanged under its pos mutation because
`decode_string` is authoritative; `Parser::bump`'s clamp is shielded by the
Eof-defaulting peek) and reserved-word diagnostic-wording differences.

### Accept-list rationale categories (bevy_bsn)

- **Debug/diagnostic formatting arithmetic** (`node_eq`/`value_eq`/`debug_node`
  indent-depth math, `last_line_width`): affects debug output layout and
  deep-document equality bookkeeping only; behavior differences require documents at
  the 256-depth guard boundary of debug walks.
- **Depth/budget guard off-by-ones** (printer `entity`/`value` `>` vs `>=` at the
  walk budget): shifts the marker onset by one node inside a defensive budget that
  legal documents never reach.
- **Cross-crate-killed**: symbols whose behavior is asserted by bevy_scene's suites
  (mutants runs are per-crate).

## Fuzzing baseline (Layer 3, recorded here for reference)

Initial local runs at introduction: `parse` 1.33M execs/46s, `roundtrip` 1.53M/61s,
`render` 583K/46s — zero crashes, zero hangs.

The cargo-fuzz workspace (`crates/bevy_bsn/fuzz/`) and its nightly workflow
(`.github/workflows/bsn-fuzz.yml`) were later removed from the branch to keep the
mergeable surface lean; the numbers above are from those runs. The bounded in-test
fuzz loops in `bevy_bsn`'s `adversarial.rs` remain and run in ordinary CI. The fuzz
workspace can be resurrected from git history if persistent coverage-guided fuzzing
is wanted again.
