# cargo-varies

## Purpose and Flow

`cargo varies` analyzes Rust library crates through a rustc wrapper, builds a
RAPx-derived API dependency graph, and emits suites plus sequence metadata.
The sole synthesis strategy is `unsafe-wrapper`; backends are `kani` (default),
`tests`, and `fuzz`.

## Code Map

- `src/cargo_phase.rs`, `cli.rs`, `workspace.rs`, `toolchain.rs`: CLI,
  Cargo target discovery, rustup selection, wrapper setup, and validation.
- `src/driver.rs`, `pipeline.rs`: primary-library callbacks and orchestration.
- `src/rapx_graph.rs`, `rapx_resolve.rs`, `rapx_mono.rs`, `type_relations.rs`:
  graph construction and bounded generic resolution.
- `src/unsafe_analysis.rs`: MIR unsafe propagation and field/contract facts.
- `src/synth/api.rs`, `synth/unsafe_wrapper/`: API classification,
  constructor planning, and mutator expansion.
- `src/std_adapters.rs`, `backend/`: shared reconstruction, rendering,
  and metadata emission.
- `tests/smoke/`, `fixtures/`: focused workflow and synthesis regression tests.

## Invariants

- Keep analysis rustc-direct. Do not restore rustdoc type parsing.
- Target public safe APIs classified `UnsafeBlock`. Basic sequences end at
  that target; mutator expansion preserves predecessor/successor relationships.
- Keep shared capabilities consistent across resolution, planning, and rendering.
  Fix causes in those layers; avoid fixture-specific branches, output patches,
  degradation paths, or heuristic bandages.
- Generic support remains bounded. Add focused fixtures before extending it.
- Coordinate metadata changes with varies-kani. Root sequences carry
  `is_basic: true`; emitted predecessor zero is a legacy sentinel.
- Keep all backends on the same synthesized sequence set. Kani validation calls
  `cargo varies-kani --only-codegen`.
- Preserve feature propagation and the active rustup toolchain for Cargo children.

## Validation

Run `cargo fmt` after Rust edits and `cargo test --locked` for shared changes.
Use targeted fixture tests during development, then verify affected generated
backends. Prefer metadata assertions for sequence semantics and executable
checks for rendering. Remove temporary diagnostics and unrelated formatting.
