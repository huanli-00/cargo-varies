# cargo-varies

`cargo varies` synthesizes target-focused verification suites for Rust library
crates. It builds a RAPx-derived API dependency graph through rustc callbacks,
finds public safe APIs containing user-written unsafe blocks, constructs call
sequences, and renders Kani harnesses, ordinary tests, or AFL targets.

## Install

Requires rustup and the pinned `nightly-2026-03-26` toolchain with `rustc-dev`,
`rust-src`, LLVM tools, and rustfmt. The binary links against that toolchain;
keep it installed after building.

```bash
git clone https://github.com/huanli-00/cargo-varies.git
cd cargo-varies
rustup show
cargo install --locked --path .
cargo varies --help
```

For development, use `cargo build --locked` and `target/debug/cargo-varies varies ...`.
Analysis uses the rustup toolchain that launched the tool, including when the
target crate has its own directory-local toolchain.
Outside this checkout, select it explicitly with
`cargo +nightly-2026-03-26 varies ...`. The examples below assume that toolchain
is active.

## Usage and Configuration

Generate the default Kani suite:

```bash
cargo varies -C /path/to/crate
cargo varies -m /path/to/workspace/Cargo.toml -p my-lib
```

Configure synthesis and enable Cargo features:

```bash
cargo varies -C /path/to/crate -F feature-a,feature-b \
  -d 8 -M 2 -t 300 -o /tmp/my-suite
```

Configuration is supplied through CLI flags; there is no synthesis config file.

| Option | Meaning | Default |
| --- | --- | --- |
| `-C, --dir` | Target directory | Current directory |
| `-m, --manifest-path` | Target manifest; excludes `-C` | Unset |
| `-p, --package` | Workspace member | Inferred if unambiguous |
| `-o, --out` | Generated suite directory | `varies_test` under target directory |
| `-b, --backend` | `kani`, `tests`, or `fuzz`; repeatable/comma-separated | `kani` |
| `-d, --max-depth` | Maximum calls per sequence; 0 is unlimited | `0` |
| `-M, --max-mutators` | Additional mutator expansion rounds | `4` |
| `-t, --timeout` | Seconds allowed for each check/validation command | No timeout |
| `-c, --check-harness` | Run backend validation after generation | Off |
| `-F, --features` | Dependency features | None added |
| `--all-features`, `--no-default-features` | Cargo feature selection | Cargo defaults |
| `--log-level` | `error`, `warn`, `info`, `debug`, `trace` | `info` |

Pass additional `cargo check` flags after `--`, for example `-- --offline`.
Depth bounds total calls, including constructors. Mutator limits count rounds,
not total emitted sequences. Generation replaces generated source and metadata
directories in the output location.

Render the same sequence set in all three formats:

```bash
cargo varies -C /path/to/crate -b kani,tests,fuzz -o /tmp/suites
```

Multiple backends write to `/tmp/suites/kani`, `tests`, and `fuzz`.
A single backend writes directly to the selected output directory.

## Run Generated Suites

### Kani

Build/install [varies-kani](https://github.com/huanli-00/varies-kani) and put its
launchers on PATH. The commands coexist with official Kani.

```bash
cargo varies -C /path/to/crate -b kani -o /tmp/my-kani -c
cd /tmp/my-kani
cargo varies-kani -Z unstable-options --synth-pruning failure-only \
  --global-timeout 30m --target-timeout 10m --html-output
```

`-c` checks compilation with `cargo varies-kani --only-codegen`; it does not
run proofs. Run verification from the generated crate so `varies_meta/` is
discovered, or set `KANI_SYNTHESIZED_META_DIR` to its absolute path.
Coverage and pruning controls are documented in the fork's README.
The verifier uses its own pinned compiler; target crates must compile with it.

### Ordinary Tests

```bash
cargo varies -C /path/to/crate -b tests -o /tmp/my-tests -c
cargo test --manifest-path /tmp/my-tests/Cargo.toml
```

These use concrete inputs and can also run under Miri or supported sanitizers.
Passing concrete tests does not establish correctness for all inputs.

### AFL

Install `cargo-afl` and its AFL runtime, then:

```bash
cargo varies -C /path/to/crate -b fuzz -o /tmp/my-fuzz
cd /tmp/my-fuzz
cargo afl build --bin fuzz_target_0
cargo afl fuzz -i corpus/fuzz_target_0 -o afl-out/fuzz_target_0 \
  -V 300 -- target/debug/fuzz_target_0
```

Each sequence gets a target and minimal seed corpus. The generated Cargo config
sets `AFL_EXIT_WHEN_DONE=1` and `AFL_NO_AFFINITY=1`.
For this backend, `-c` runs `cargo check --bins`; it does not execute AFL.

## Output and Limits

Each suite is a standalone Cargo crate with `varies_meta/` containing
`<crate>_api_functions.json` and `<crate>_api_sequences.json`.
Kani/tests use `src/test_<crate>N.rs`; AFL uses `fuzz_targets/fuzz_target_N.rs`
and `corpus/fuzz_target_N/seed`. Sequence metadata records API calls, target,
root/predecessor/successor relationships, and the latest mutator.

The retained synthesis strategy is `unsafe-wrapper`: constructor chains end
at direct unsafe wrapper targets, with bounded mutator expansion before the
target. Indirect-only wrappers are excluded.

Symbolic inputs support scalars, composites, selected standard containers, and
accessible local ADTs. Generic resolution is bounded; const generics, complex
associated-type predicates, and broader external impl reasoning remain limited.
Unsafe contract analysis does not establish complete API preconditions.
Generated suites can expose invalid input assumptions as well as target bugs.

## Development

```bash
cargo fmt -- --check
cargo test --locked
```

Smoke fixtures cover CLI selection, metadata, generics, reconstruction, and
backend execution. Build/install varies-kani and put its launchers on PATH
before running the full smoke suite. Use `cargo test --locked --lib` for unit
tests that do not require the verifier.

See [AGENTS.md](AGENTS.md) for code boundaries and invariants.
Optional batch execution/container tooling is described in
[scripts/README.md](scripts/README.md).

## License

Licensed under [MPL-2.0](LICENSE-MPL). API graph and generic resolution code
derives from [RAPx](https://github.com/Artisan-Lab/RAPx); see [NOTICE](NOTICE)
for source attribution and modifications.
