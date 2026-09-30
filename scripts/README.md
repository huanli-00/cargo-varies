# Experiment Tools

These tools are optional and separate from the `cargo varies` CLI.

`paper_backend_execution.py` consumes generation-result CSVs and runs generated
`tests`, `fuzz`, or `kani` suites. Use `--help` for the CSV inputs, filters,
budgets, and output paths. Default files are under `tmp/paper-experiments/`.
The historical crate lists in `misc/` are research inputs.

```bash
python3 scripts/paper_backend_execution.py \
  --generation-results /path/to/all_results.csv --backend kani \
  --varies-kani-dir /path/to/varies-kani --build-varies-kani
```

Kani command options are stored in `config/kani-ab-coverage.json`; those defaults
include `--assert-as-assume` and `--skip-unsupported-check`, which weaken proof
obligations. Review them before interpreting verification results.

## Container

The container reuses host rustup toolchains, cargo-afl, CBMC tools, and a built
varies-kani checkout. Docker and the two pinned nightly toolchains are required.

```bash
export VARIES_KANI_HOST_DIR=/path/to/varies-kani
scripts/build_experiment_container.sh
scripts/run_in_experiment_container.sh python3 scripts/paper_backend_execution.py --help
```

Optional overrides: `IMAGE`, `DOCKER_CPUS`, `DOCKER_MEMORY`,
`DOCKER_MEMORY_SWAP`, `DOCKER_SHM_SIZE`, `POPULAR_CRATES_HOST_DIR`,
`RUSTUP_HOST_DIR`, `CARGO_HOST_DIR`, and `CONTAINER_WORKDIR`.
The container runs with the host user ID; mounted source and tool caches remain
writable by that user.
