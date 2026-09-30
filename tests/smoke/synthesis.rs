use super::support::{
    api_index, assert_harness_compiles_with_kani, kani_is_available, run_varies_on_fixture,
};

#[test]
fn cargo_varies_focuses_synthesis_on_unsafe_wrapper_targets() {
    let run = run_varies_on_fixture("unsafe-wrapper-lib", "unsafe-wrapper-harness", true, &[]);
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_lib");
    let generated_modules = run.generated_modules();

    let bump_index = api_index(&api_metadata, "Counter::bump");
    let constructor_index = api_index(&api_metadata, "Counter::new");
    let basic_sequences = sequence_metadata
        .iter()
        .filter(|seq| seq.is_basic)
        .collect::<Vec<_>>();
    let successor_count = sequence_metadata.iter().filter(|seq| !seq.is_basic).count();

    assert!(
        !sequence_metadata.is_empty(),
        "expected at least one wrapper-focused sequence"
    );
    assert!(
        sequence_metadata.iter().all(|seq| seq.target == bump_index),
        "expected all synthesized targets to focus on the unsafe wrapper method"
    );
    assert!(
        basic_sequences.len() == 1,
        "expected exactly one basic constructor-plus-wrapper sequence"
    );
    assert!(
        basic_sequences[0].functions == vec![constructor_index, bump_index],
        "expected the base sequence to be constructor followed by the wrapper target"
    );
    assert!(
        successor_count >= 1,
        "expected at least one mutator-extended successor sequence"
    );
    assert!(
        sequence_metadata.iter().any(|seq| {
            !seq.is_basic && seq.functions == vec![constructor_index, bump_index, bump_index]
        }),
        "expected a successor sequence that extends the base wrapper sequence with another bump"
    );
    assert!(
        generated_modules
            .iter()
            .all(|module| module.contains("extern crate unsafe_wrapper_lib;")),
        "expected generated wrapper harnesses to use original-style extern crate imports"
    );
    assert!(
        generated_modules
            .iter()
            .any(|module| module.contains("fn test_function")),
        "expected generated wrapper harnesses to define Kani helper test functions"
    );
    assert!(
        generated_modules
            .iter()
            .any(|module| module.contains("-> Option<()>")),
        "expected generated wrapper harnesses to return Option<()> like original varies"
    );
    assert!(
        generated_modules
            .iter()
            .any(|module| module.contains("#[cfg_attr(kani, kani::proof)]")),
        "expected generated wrapper harnesses to expose Kani proof entrypoints"
    );
    assert!(
        generated_modules
            .iter()
            .any(|module| module.contains("kani::any::<u32>()")),
        "expected generated wrapper harnesses to use symbolic Kani inputs for scalar seeds"
    );
    assert!(
        generated_modules
            .iter()
            .all(|module| !module.contains("#[test]")),
        "expected wrapper harnesses to stop using plain unit-test entrypoints"
    );
    assert!(
        generated_modules
            .iter()
            .any(|module| module.contains("unsafe_wrapper_lib::Counter::new(")),
        "expected the generated harness to construct the wrapper state before calling the target"
    );
    assert!(
        generated_modules
            .iter()
            .all(|module| module.contains("unsafe_wrapper_lib::Counter::bump(")),
        "expected each generated wrapper-focused suite to call the unsafe wrapper target"
    );
    assert!(
        generated_modules
            .iter()
            .all(|module| !module.contains("unsafe_wrapper_lib::Counter::get(")),
        "expected observer-only APIs to stay out of wrapper-target suites"
    );
}

#[test]
fn cargo_varies_reports_unsafe_block_wrapper_targets() {
    let run = run_varies_on_fixture("unsafe-wrapper-lib", "unsafe-wrapper-harness", true, &[]);
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_lib");
    assert!(
        api_metadata
            .iter()
            .any(|api| api.full_name == "Counter::bump" && api.api_safety == "unsafe_block"),
        "expected wrapper method metadata to recover original varies-style unsafe_block targets"
    );
    assert!(
        api_metadata.iter().any(|api| {
            api.full_name == "Counter::bump"
                && api
                    .unsafe_wrappers
                    .iter()
                    .any(|function| function.ends_with("Counter::bump"))
        }),
        "expected api metadata to record the function owning the unsafe code"
    );
    assert!(
        sequence_metadata.iter().any(|sequence| {
            sequence
                .unsafe_wrappers
                .iter()
                .any(|function| function.ends_with("Counter::bump"))
        }),
        "expected sequence metadata to record the unsafe-function target for synthesized suites"
    );
}

#[test]
fn cargo_varies_enumerates_constructor_rooted_wrapper_suites() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-multi-lib",
        "unsafe-wrapper-multi-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_multi_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_multi_lib");
    let generated_modules = run.generated_modules();

    let bump_index = api_index(&api_metadata, "Counter::bump");
    let new_index = api_index(&api_metadata, "Counter::new");
    let recover_index = api_index(&api_metadata, "Counter::recover");
    let restore_index = api_index(&api_metadata, "Counter::restore");
    let scrub_index = api_index(&api_metadata, "Counter::scrub");
    let basic_sequences = sequence_metadata
        .iter()
        .filter(|seq| seq.is_basic)
        .collect::<Vec<_>>();

    assert!(
        basic_sequences.len() == 3,
        "expected one basic sequence per constructor-materialized path"
    );
    assert!(
        basic_sequences.iter().all(|seq| seq.target == bump_index),
        "expected all basic sequences to stay focused on the unsafe wrapper target"
    );
    assert!(
        basic_sequences
            .iter()
            .any(|seq| seq.functions == vec![new_index, bump_index]),
        "expected a base sequence using Counter::new"
    );
    assert!(
        basic_sequences
            .iter()
            .any(|seq| seq.functions == vec![recover_index, bump_index]),
        "expected a base sequence using Counter::recover"
    );
    assert!(
        basic_sequences
            .iter()
            .any(|seq| seq.functions == vec![restore_index, bump_index]),
        "expected a base sequence using Counter::restore"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| !seq.is_basic && seq.functions.contains(&scrub_index)),
        "expected at least one mutator-extended successor sequence"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_multi_lib::Counter::recover(")
                && module.contains("Result::Ok(value)")
        }),
        "expected generated harnesses to unwrap Result-backed constructor paths"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_multi_lib::Counter::restore(")
                && module.contains("Option::Some(value)")
        }),
        "expected generated harnesses to unwrap Option-backed constructor paths"
    );
    assert!(
        generated_modules
            .iter()
            .all(|module| !module.contains("unsafe_wrapper_multi_lib::Counter::peek(")),
        "expected observer-only APIs to stay out of wrapper-target suites"
    );
}

#[test]
fn cargo_varies_expands_mutators_round_by_round() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-multi-lib",
        "unsafe-wrapper-multi-round-harness",
        true,
        &[],
    );
    run.assert_success();

    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_multi_lib");
    let non_basic_sequences = sequence_metadata
        .iter()
        .filter(|seq| !seq.is_basic)
        .collect::<Vec<_>>();

    assert!(
        non_basic_sequences.len() > 2,
        "expected round-based mutator expansion to emit more than two non-basic sequences"
    );
    assert!(
        non_basic_sequences
            .iter()
            .any(|seq| seq.functions.len() >= 4),
        "expected at least one second-round sequence with two mutator steps"
    );
}

#[test]
fn cargo_varies_mutates_shared_symbolic_inputs_before_wrapper_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-mutator-lib",
        "unsafe-wrapper-symbolic-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_symbolic_mutator_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_symbolic_mutator_lib");
    let generated_modules = run.generated_modules();

    let bump_index = api_index(&api_metadata, "bump");
    let tweak_index = api_index(&api_metadata, "tweak");
    let basic_sequences = sequence_metadata
        .iter()
        .filter(|seq| seq.is_basic)
        .collect::<Vec<_>>();

    assert!(
        basic_sequences.len() == 1 && basic_sequences[0].functions == vec![bump_index],
        "expected a single symbolic-input basic sequence for the unsafe wrapper target"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| !seq.is_basic && seq.functions == vec![tweak_index, bump_index]),
        "expected a successor sequence that mutates the shared symbolic input before the target"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("let mut symbolic_param_0 = _param0;")
                && module.contains("unsafe_wrapper_symbolic_mutator_lib::tweak(")
                && module.contains("unsafe_wrapper_symbolic_mutator_lib::bump(")
        }),
        "expected generated harnesses to materialize the symbolic input and mutate it before the target"
    );
    assert!(
        generated_modules
            .iter()
            .all(|module| !module.contains("unsafe_wrapper_symbolic_mutator_lib::read(")),
        "expected non-target observer APIs to stay out of the wrapper suite"
    );
}

#[test]
fn cargo_varies_renders_sequences_as_afl_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-lib",
        "unsafe-wrapper-fuzz-harness",
        true,
        &["--backend", "fuzz"],
    );
    run.assert_success();

    let manifest = run.manifest();
    assert!(
        manifest.contains("afl = \"0.18.1\"")
            && manifest.contains("[[bin]]")
            && manifest.contains("path = \"fuzz_targets/fuzz_target_0.rs\""),
        "expected the fuzz backend to emit AFL binary targets:\n{manifest}"
    );
    let cargo_config = run.cargo_config();
    assert!(
        cargo_config.contains("[env]")
            && cargo_config.contains("AFL_EXIT_WHEN_DONE = \"1\"")
            && cargo_config.contains("AFL_NO_AFFINITY = \"1\""),
        "expected fuzz backend to emit AFL runtime environment config:\n{cargo_config}"
    );

    let fuzz_targets = run.generated_fuzz_targets();
    assert!(
        !fuzz_targets.is_empty(),
        "expected fuzz backend to write per-sequence fuzz target files"
    );
    let seed_lengths = run.generated_fuzz_seed_lengths();
    assert_eq!(
        seed_lengths.len(),
        fuzz_targets.len(),
        "expected fuzz backend to write one seed corpus file per fuzz target"
    );
    assert!(
        seed_lengths.iter().all(|len| *len > 0) && seed_lengths.iter().any(|len| *len == 4),
        "expected fuzz backend to write non-empty minimal seed corpus files"
    );
    assert!(
        fuzz_targets.iter().any(|target| {
            target.contains("fn main()")
                && target.contains("afl::fuzz!(|data: &[u8]|")
                && target.contains("use varies_test::*;")
                && target.contains("fn test_function")
                && target.contains("if data.len() < 4")
                && target.contains("let _param0 = _to_u32(data, 0);")
                && target.contains("unsafe_wrapper_lib::Counter::new(")
                && target.contains("unsafe_wrapper_lib::Counter::bump(")
                && !target.contains("fn _to_u32")
                && !target.contains("kani::any")
        }),
        "expected fuzz target to decode byte data into test_function parameters and reuse the sequence body"
    );
}

#[test]
fn cargo_varies_keeps_backend_output_counts_aligned() {
    let mut expected_sequence_count = None;

    for (backend, harness_name) in [
        ("kani", "unsafe-wrapper-count-kani-harness"),
        ("tests", "unsafe-wrapper-count-tests-harness"),
        ("fuzz", "unsafe-wrapper-count-fuzz-harness"),
    ] {
        let run = run_varies_on_fixture(
            "unsafe-wrapper-lib",
            harness_name,
            false,
            &["--backend", backend],
        );
        run.assert_success();

        let sequence_count = run.sequence_metadata("unsafe_wrapper_lib").len();
        let generated_file_count = if backend == "fuzz" {
            run.generated_fuzz_targets().len()
        } else {
            run.generated_modules().len()
        };

        assert_eq!(
            generated_file_count, sequence_count,
            "expected {backend} to emit one generated file per synthesized sequence"
        );

        if let Some(expected_sequence_count) = expected_sequence_count {
            assert_eq!(
                sequence_count, expected_sequence_count,
                "expected {backend} to preserve the same synthesized sequence count as other backends"
            );
        } else {
            assert!(
                sequence_count > 0,
                "expected backend count fixture to synthesize at least one sequence"
            );
            expected_sequence_count = Some(sequence_count);
        }
    }
}

#[test]
fn cargo_varies_literal_tests_preserve_shared_param_state_across_steps() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-mutator-lib",
        "unsafe-wrapper-symbolic-literal-test-harness",
        true,
        &["--backend", "tests"],
    );
    run.assert_success();

    let generated_modules = run.generated_modules();

    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_symbolic_mutator_lib::tweak(&mut literal_param_0")
                && module.contains("unsafe_wrapper_symbolic_mutator_lib::bump(&mut literal_param_0")
                && !module.contains("_fresh_")
        }),
        "expected literal tests to reuse one mutable parameter binding across mutator and target steps"
    );
}

#[test]
fn cargo_varies_keeps_whole_object_mutators_available() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-whole-object-mutator-lib",
        "unsafe-wrapper-whole-object-mutator-harness",
        false,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_whole_object_mutator_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_whole_object_mutator_lib");
    let new_index = api_index(&api_metadata, "Counter::new");
    let rewrite_index = api_index(&api_metadata, "Counter::rewrite");
    let bump_index = api_index(&api_metadata, "Counter::bump");

    assert!(
        sequence_metadata.iter().any(|seq| {
            !seq.is_basic
                && seq.target == bump_index
                && seq.functions == vec![new_index, rewrite_index, bump_index]
        }),
        "expected whole-object writes like `*self = ...` to remain valid mutator steps before unsafe targets"
    );
}

#[test]
fn cargo_varies_requires_full_trait_object_bounds_for_producers() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-trait-object-bound-lib",
        "unsafe-wrapper-trait-object-bound-harness",
        false,
        &[],
    );
    run.assert_success();

    let generated_modules = run.generated_modules();

    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_trait_object_bound_lib::cursor_reader()")
                && module.contains("unsafe_wrapper_trait_object_bound_lib::touch(")
        }),
        "expected a constructor that satisfies the full `dyn Read + Send` object bounds to remain usable"
    );
    assert!(
        generated_modules.iter().all(|module| {
            !module.contains("unsafe_wrapper_trait_object_bound_lib::rc_reader()")
        }),
        "expected producers that satisfy only the principal trait to be excluded when auto-trait object bounds are not met"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_trait_object_bound_lib::boxed_cursor()")
                && module.contains("unsafe_wrapper_trait_object_bound_lib::boxed_touch(")
        }),
        "expected smart-pointer trait-object inputs to consider coercible constructor outputs rather than only exact Box<dyn ...> buckets"
    );
}

#[test]
fn cargo_varies_materializes_cursor_symbolic_inputs() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-trait-object-bound-lib",
        "unsafe-wrapper-trait-object-cursor-harness",
        false,
        &[],
    );
    run.assert_success();

    let generated_modules = run.generated_modules();

    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_trait_object_bound_lib::touch_cursor(")
                && module.contains("kani::any::<u8>()")
                && module.contains("::std::io::Cursor::new(symbolic_param_0)")
        }),
        "expected Cursor<T> inputs to reconstruct the wrapper from the symbolic inner seed before calling the target"
    );
}

#[test]
fn cargo_varies_rejects_trait_object_borrows_with_stronger_explicit_lifetimes() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-trait-object-bound-lib",
        "unsafe-wrapper-trait-object-static-harness",
        false,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_trait_object_bound_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_trait_object_bound_lib");
    let generated_modules = run.generated_modules();
    let touch_static_index = api_index(&api_metadata, "touch_static");

    assert!(
        sequence_metadata
            .iter()
            .all(|seq| seq.target != touch_static_index),
        "expected targets that require stronger explicit reference lifetimes than a local borrow can satisfy to synthesize no sequences"
    );
    assert!(
        generated_modules.iter().all(|module| {
            !module.contains("unsafe_wrapper_trait_object_bound_lib::touch_static(")
        }),
        "expected generated harnesses to avoid emitting trait-object calls whose explicit lifetime requirements are not satisfiable from local borrows"
    );
}

#[test]
fn cargo_varies_keeps_generic_cursor_monos_aligned_with_backend_inputs() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-generic-cursor-lib",
        "unsafe-wrapper-generic-cursor-harness",
        false,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_generic_cursor_lib");
    let owned_touch = api_metadata
        .iter()
        .find(|api| {
            api.full_name.starts_with("cursor_touch::<")
                && api.is_mono
                && api.api_safety == "unsafe_block"
                && api.concrete_args.len() == 1
                && api.concrete_args[0].contains("Cursor")
                && api.concrete_args[0].contains("Vec")
        })
        .expect("expected an owned Cursor mono instance that matches backend symbolic adapters");

    assert!(
        owned_touch
            .concrete_args
            .first()
            .is_some_and(|arg| arg.contains("Vec")),
        "expected the owned Cursor mono instance to survive the crowded generic candidate set"
    );
}

#[test]
fn cargo_varies_fuzz_backend_qualifies_generic_cursor_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-generic-cursor-lib",
        "unsafe-wrapper-generic-cursor-fuzz-harness",
        true,
        &["--backend", "fuzz"],
    );
    run.assert_success();

    let fuzz_targets = run.generated_fuzz_targets();
    assert!(
        fuzz_targets.iter().any(|target| {
            target.contains(
                "unsafe_wrapper_generic_cursor_lib::cursor_touch::<unsafe_wrapper_generic_cursor_lib::Reader06>("
            ) || target.contains(
                "unsafe_wrapper_generic_cursor_lib::cursor_touch::<&mut unsafe_wrapper_generic_cursor_lib::Reader06>("
            )
        }),
        "expected fuzz targets to use crate-qualified generic type arguments for local mono instances"
    );
}

#[test]
fn cargo_varies_fuzz_backend_renders_local_trait_monos_standalone() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-local-trait-bound-lib",
        "unsafe-wrapper-local-trait-fuzz-harness",
        true,
        &["--backend", "fuzz"],
    );
    run.assert_success();

    let fuzz_targets = run.generated_fuzz_targets();
    assert!(
        fuzz_targets
            .iter()
            .any(|target| target.contains("Wrapper::<i16>(")),
        "expected generic tuple struct constructors to use expression-path turbofish syntax"
    );
    assert!(
        fuzz_targets.iter().all(|target| {
            !target.contains("Wrapper<i16>(")
                && !target.contains("Wrapper<unsafe_wrapper_local_trait_bound_lib::Seed>(")
                && !target.contains("Wrapper<std::vec::Vec>")
                && !target.contains("Cursor<std::vec::Vec>")
        }),
        "expected fuzz targets to avoid non-standalone generic constructor and type paths"
    );
}

#[test]
fn cargo_varies_unsafe_wrapper_strategy_keeps_direct_targets_only() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-indirect-field-lib",
        "unsafe-wrapper-direct-only-harness",
        false,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_indirect_field_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_indirect_field_lib");
    let bump_core_index = api_index(&api_metadata, "Counter::bump_core");
    let bump_twice_index = api_index(&api_metadata, "Counter::bump_twice");
    let bump_alias_index = api_index(&api_metadata, "Counter::bump_alias");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.target == bump_core_index),
        "expected the original unsafe-wrapper strategy to keep direct unsafe targets"
    );
    assert!(
        sequence_metadata
            .iter()
            .all(|seq| seq.target != bump_twice_index),
        "expected indirect-only targets to be excluded from the original unsafe-wrapper strategy"
    );
    assert!(
        sequence_metadata
            .iter()
            .all(|seq| seq.target != bump_alias_index),
        "expected propagated aliases to be excluded from the original unsafe-wrapper strategy"
    );
}

#[test]
fn cargo_varies_generated_symbolic_mutator_harness_compiles_under_kani() {
    if !kani_is_available() {
        eprintln!("skipping Kani syntax check because `cargo varies-kani` is unavailable");
        return;
    }

    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-mutator-lib",
        "unsafe-wrapper-symbolic-kani-harness",
        false,
        &[],
    );
    run.assert_success();

    assert_harness_compiles_with_kani(&run.harness_dir, &run.target_dir);
}

#[test]
fn cargo_varies_applies_unsafe_wrapper_basic_sequence_selection_limits() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-many-basic-lib",
        "unsafe-wrapper-many-basic-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_many_basic_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_many_basic_lib");
    let pair_touch_index = api_index(&api_metadata, "pair_touch");

    assert!(
        sequence_metadata.len() == 32,
        "expected unsafe-wrapper basic-sequence selection to cap the 7x7 constructor cross-product at 32"
    );
    assert!(
        sequence_metadata
            .iter()
            .all(|seq| seq.is_basic && seq.target == pair_touch_index),
        "expected only basic target-focused sequences for the many-constructor fixture"
    );
    assert!(
        sequence_metadata.iter().all(|seq| seq.functions.len() == 3),
        "expected selected sequences to keep the two-constructor-plus-target shape"
    );
    assert!(
        sequence_metadata.iter().all(|seq| {
            api_metadata[seq.functions[0]]
                .full_name
                .starts_with("Handle::")
                && api_metadata[seq.functions[1]]
                    .full_name
                    .starts_with("Handle::")
                && seq.functions[2] == pair_touch_index
        }),
        "expected selected sequences to consist only of constructors followed by the target"
    );
}

#[test]
fn cargo_varies_does_not_add_mutators_after_target_by_default() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-return-lib",
        "unsafe-wrapper-return-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_return_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_return_lib");
    let wrap_index = api_index(&api_metadata, "wrap");
    let scrub_index = api_index(&api_metadata, "scrub");

    assert!(
        sequence_metadata.len() == 1,
        "expected only the basic wrapper sequence when post-target mutators are disabled"
    );
    assert!(
        sequence_metadata[0].is_basic && sequence_metadata[0].functions == vec![wrap_index],
        "expected the emitted suite to stop at the unsafe wrapper target"
    );
    assert!(
        !sequence_metadata[0].functions.contains(&scrub_index),
        "expected no mutator after the target output by default"
    );
}

#[test]
fn cargo_varies_uses_public_reexport_paths_in_generated_harnesses() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-reexport-lib",
        "unsafe-wrapper-reexport-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_reexport_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_reexport_lib");
    let generated_modules = run.generated_modules();

    let bump_index = api_index(&api_metadata, "bump");
    let constructor_index = api_index(&api_metadata, "new_counter");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.functions == vec![constructor_index, bump_index]),
        "expected the suite to use the re-exported constructor and target"
    );
    assert!(
        generated_modules.iter().all(|module| {
            module.contains("unsafe_wrapper_reexport_lib::new_counter(")
                && module.contains("unsafe_wrapper_reexport_lib::bump(")
        }),
        "expected generated harnesses to call re-exported root paths"
    );
    assert!(
        generated_modules
            .iter()
            .all(|module| !module.contains("unsafe_wrapper_reexport_lib::inner::")),
        "expected generated harnesses to avoid private module paths"
    );
}

#[test]
fn cargo_varies_uses_public_reexport_aliases_in_generated_harnesses() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-reexport-alias-lib",
        "unsafe-wrapper-reexport-alias-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_reexport_alias_lib");
    let generated_modules = run.generated_modules();

    assert!(
        api_metadata
            .iter()
            .any(|api| api.full_name == "PublicCounter::new"),
        "expected aliased public type methods to use the re-exported type path in metadata"
    );
    assert!(
        api_metadata
            .iter()
            .any(|api| api.full_name == "PublicCounter::bump"),
        "expected aliased target methods to use the re-exported type path in metadata"
    );
    assert!(
        api_metadata.iter().any(|api| api.full_name == "poke"),
        "expected aliased free functions to use the public alias in metadata"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_reexport_alias_lib::PublicCounter::new(")
                && module.contains("unsafe_wrapper_reexport_alias_lib::PublicCounter::bump(")
        }),
        "expected generated harnesses to call methods through the aliased public type path"
    );
    assert!(
        generated_modules
            .iter()
            .any(|module| module.contains("unsafe_wrapper_reexport_alias_lib::poke(")),
        "expected generated harnesses to call aliased free functions through the public alias"
    );
    assert!(
        generated_modules.iter().all(|module| {
            !module.contains("unsafe_wrapper_reexport_alias_lib::inner::Counter::")
                && !module.contains("unsafe_wrapper_reexport_alias_lib::inner::wrap_bump(")
        }),
        "expected generated harnesses to avoid private definition paths when aliases are exported"
    );
}

#[test]
fn cargo_varies_supports_trait_default_method_constructor_chains() {
    let run = run_varies_on_fixture(
        "trait-default-slice-lib",
        "trait-default-slice-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("trait_default_slice_lib");
    let sequence_metadata = run.sequence_metadata("trait_default_slice_lib");
    let generated_modules = run.generated_modules();

    let cursor_index = api_index(&api_metadata, "<[u8] as ByteCursor>::cursor");
    let next_index = api_metadata
        .iter()
        .find(|api| {
            api.full_name == "<Cursor<'a> as std::iter::Iterator>::next"
                || api.full_name == "<Cursor<'_> as std::iter::Iterator>::next"
                || api.full_name == "<Cursor::<'_> as std::iter::Iterator>::next"
        })
        .expect("expected lifetime-parameterized iterator target in metadata")
        .index;

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.target == next_index && seq.functions == vec![cursor_index, next_index]),
        "expected a constructor chain from the trait default method into the unsafe iterator target"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("kani::any::<([u8; 8], usize)>()")
                && module.contains("<[u8] as trait_default_slice_lib::ByteCursor>::cursor(")
                && (module.contains(
                    "<trait_default_slice_lib::Cursor<'a> as std::iter::Iterator>::next(",
                ) || module.contains(
                    "<trait_default_slice_lib::Cursor<'_> as std::iter::Iterator>::next(",
                ) || module.contains(
                    "<trait_default_slice_lib::Cursor::<'_> as std::iter::Iterator>::next(",
                ))
        }),
        "expected generated harnesses to use a symbolic slice seed and UFCS calls for the trait default constructor chain"
    );
}

#[test]
fn cargo_varies_resolves_multi_generic_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-multi-generic-lib",
        "unsafe-wrapper-multi-generic-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_multi_generic_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_multi_generic_lib");
    let generated_modules = run.generated_modules();

    let join_touch = api_metadata
        .iter()
        .find(|api| {
            api.full_name.starts_with("join_touch::<")
                && api.is_mono
                && api.api_safety == "unsafe_block"
                && api.concrete_args.len() == 2
                && api.concrete_args[0].contains("String")
                && api.concrete_args[1].contains("Vec")
        })
        .expect("expected a two-generic mono instance for join_touch");
    let append_touch = api_metadata
        .iter()
        .find(|api| {
            api.full_name == "<[u8] as ByteBridge>::append_touch"
                && api.is_mono
                && api.api_safety == "unsafe_block"
                && api.concrete_args.len() == 2
                && api.instantiated_path.contains("append_touch::<")
        })
        .expect("expected a trait-self plus method-generic mono instance for append_touch");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![join_touch.index]),
        "expected a direct basic suite for the multi-generic free-function wrapper target"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![append_touch.index]),
        "expected a direct basic suite for the trait default wrapper target with extra generics"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_multi_generic_lib::join_touch::<")
                && module.contains("kani::any::<([u8; 8], usize)>()")
                && module.contains("String::from_utf8_lossy")
                && module.contains("values.into_iter().take(len)")
        }),
        "expected the generated harness to reconstruct symbolic inputs for both free-function generic argumentsfunction generic arguments"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("kani::any::<([u8; 8], usize)>()")
                && module.contains(
                    "<[u8] as unsafe_wrapper_multi_generic_lib::ByteBridge>::append_touch(",
                )
        }),
        "expected the generated harness to use UFCS for the trait default target after resolving Self plus method generics"
    );
}

#[test]
fn cargo_varies_filters_local_trait_candidates_with_solver_checks() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-local-trait-bound-lib",
        "unsafe-wrapper-local-trait-bound-harness",
        false,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_local_trait_bound_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_local_trait_bound_lib");
    let generated_modules = run.generated_modules();

    let local_touch = api_metadata
        .iter()
        .find(|api| {
            api.full_name.starts_with("local_into_vec_touch::<")
                && api.is_mono
                && api.api_safety == "unsafe_block"
                && api.concrete_args.len() == 2
                && api.concrete_args[0].contains("String")
                && api.concrete_args[1] == "u8"
        })
        .expect("expected a valid local-trait mono instance that satisfies the impl bound");

    assert!(
        api_metadata.iter().all(|api| {
            !(api.full_name.starts_with("local_into_vec_touch::<")
                && api.concrete_args.len() == 2
                && api.concrete_args[0].contains("String")
                && api.concrete_args[1] == "std::string::String")
        }),
        "expected local_into_vec_touch to reject String rhs because the local impl requires Copy"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![local_touch.index]),
        "expected a direct basic suite for the valid local-trait mono instance"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_local_trait_bound_lib::local_into_vec_touch::<")
                && module.contains("String::from_utf8_lossy")
                && module.contains("local_into_vec_touch::<std::string::String, u8>(")
        }),
        "expected the generated harness to keep the valid local-trait instantiation with String and u8 inputs"
    );
}

#[test]
fn cargo_varies_accepts_into_string_candidates_via_solver_checks() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-into-string-bound-lib",
        "unsafe-wrapper-into-string-bound-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_into_string_bound_lib");
    assert!(
        api_metadata.iter().any(|api| {
            api.full_name.starts_with("into_string_touch::<")
                && api.is_mono
                && api.api_safety == "unsafe_block"
                && api.concrete_args.len() == 1
                && api.concrete_args[0] == "&str"
        }),
        "expected Into<String> bounds to keep the &str mono instance"
    );

    assert!(
        api_metadata.iter().any(|api| {
            api.full_name
                .starts_with("into_static_cow_touch::<std::string::String>")
                && api.is_mono
                && api.api_safety == "unsafe_block"
        }),
        "expected Into<Cow<'static, str>> bounds to keep the owned String mono instance"
    );
    assert!(
        api_metadata.iter().all(|api| {
            !api.full_name.starts_with("into_static_cow_touch::<")
                || api
                    .concrete_args
                    .first()
                    .is_some_and(|arg| !arg.starts_with('&'))
        }),
        "expected Into<Cow<'static, str>> bounds to reject borrowed mono candidates that cannot be rendered from local symbolic values"
    );
}

#[test]
fn cargo_varies_seeds_public_local_types_for_generic_candidates() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-local-public-seed-lib",
        "unsafe-wrapper-local-public-seed-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_local_public_seed_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_local_public_seed_lib");
    let generated_modules = run.generated_modules();

    let local_copy = api_metadata
        .iter()
        .find(|api| {
            api.full_name.starts_with("local_copy_touch::<")
                && api.is_mono
                && api.api_safety == "unsafe_block"
                && api.concrete_args.len() == 1
                && api.concrete_args[0].contains("Seed")
        })
        .expect("expected a public local Copy type to enter the generic candidate pool");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.target == local_copy.index),
        "expected a synthesized suite for the seeded public local type"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_local_public_seed_lib::local_copy_touch::<")
                && module.contains("Seed")
        }),
        "expected the generated harness to keep the seeded public local type instantiation"
    );
}

#[test]
fn cargo_varies_seeds_free_function_local_trait_bounds_from_impls() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-local-trait-bound-lib",
        "unsafe-wrapper-local-trait-free-seed-harness",
        false,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_local_trait_bound_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_local_trait_bound_lib");
    let generated_modules = run.generated_modules();

    let local_marker = api_metadata
        .iter()
        .find(|api| {
            api.full_name.starts_with("local_marker_touch::<")
                && api.is_mono
                && api.api_safety == "unsafe_block"
                && api.concrete_args.len() == 1
                && api.concrete_args[0].contains("Seed")
        })
        .expect(
            "expected free-function local trait bounds to seed concrete generic args from impls",
        );

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![local_marker.index]),
        "expected a direct basic suite for the free-function local-trait seed"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_local_trait_bound_lib::local_marker_touch::<")
                && module.contains("Seed")
        }),
        "expected the generated harness to keep the free-function local-trait seeded instantiation"
    );
}

#[test]
fn cargo_varies_seeds_generic_local_trait_impl_bounds() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-local-trait-bound-lib",
        "unsafe-wrapper-local-trait-generic-impl-seed-harness",
        false,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_local_trait_bound_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_local_trait_bound_lib");
    let generated_modules = run.generated_modules();

    let local_marker = api_metadata
        .iter()
        .find(|api| {
            api.full_name.starts_with("local_marker_wrap_touch::<")
                && api.is_mono
                && api.api_safety == "unsafe_block"
                && api.concrete_args.len() == 1
                && api.concrete_args[0].contains("Wrapper")
                && api.concrete_args[0].contains("Seed")
        })
        .expect("expected generic local-trait impl seeding to instantiate Wrapper<Seed>");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![local_marker.index]),
        "expected a direct basic suite for the generic local-trait impl seed"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_local_trait_bound_lib::local_marker_wrap_touch::<")
                && module.contains("Wrapper::<")
                && module.contains("Seed")
        }),
        "expected the generated harness to keep the generic local-trait impl seeded instantiation"
    );
}

#[test]
fn cargo_varies_seeds_trait_method_self_from_generic_impls() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-local-trait-bound-lib",
        "unsafe-wrapper-trait-self-generic-impl-seed-harness",
        false,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_local_trait_bound_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_local_trait_bound_lib");
    let generated_modules = run.generated_modules();

    let wrap_touch = api_metadata
        .iter()
        .find(|api| {
            (api.full_name == "<Wrapper<Seed> as LocalBridge>::wrap_touch"
                || api.full_name == "<Wrapper::<Seed> as LocalBridge>::wrap_touch")
                && api.is_mono
                && api.api_safety == "unsafe_block"
        })
        .expect("expected trait-method self seeding to instantiate Wrapper<Seed>");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.target == wrap_touch.index),
        "expected a synthesized suite for the generic-impl trait-method self seed"
    );
    assert!(
        generated_modules.iter().any(|module| {
            (module.contains("unsafe_wrapper_local_trait_bound_lib::Wrapper<")
                || module.contains("unsafe_wrapper_local_trait_bound_lib::Wrapper::<"))
                && module.contains("unsafe_wrapper_local_trait_bound_lib::Seed")
                && module
                    .contains("unsafe_wrapper_local_trait_bound_lib::LocalBridge>::wrap_touch(")
        }),
        "expected the generated harness to keep the trait-method self instantiation for Wrapper<Seed>"
    );
}

#[test]
fn cargo_varies_filters_copy_clone_candidates_with_concrete_trait_checks() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-copy-clone-bound-lib",
        "unsafe-wrapper-copy-clone-bound-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_copy_clone_bound_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_copy_clone_bound_lib");
    let generated_modules = run.generated_modules();

    let copy_touch = api_metadata
        .iter()
        .find(|api| {
            api.full_name.starts_with("copy_touch::<")
                && api.is_mono
                && api.api_safety == "unsafe_block"
                && api.concrete_args.len() == 1
                && api.concrete_args[0] == "u8"
        })
        .expect("expected a valid Copy mono instance for copy_touch");
    let clone_touch = api_metadata
        .iter()
        .find(|api| {
            api.full_name.starts_with("clone_touch::<")
                && api.is_mono
                && api.api_safety == "unsafe_block"
                && api.concrete_args.len() == 1
                && api.concrete_args[0].contains("String")
        })
        .expect("expected a valid Clone mono instance for clone_touch");

    assert!(
        api_metadata.iter().all(|api| {
            !(api.full_name.starts_with("copy_touch::<")
                && api
                    .concrete_args
                    .iter()
                    .any(|arg| arg == "std::string::String" || arg == "Seed"))
        }),
        "expected copy_touch to reject non-Copy candidates such as String and the owned local Seed type"
    );
    assert!(
        api_metadata.iter().all(|api| {
            !(api.full_name.starts_with("clone_touch::<")
                && api.concrete_args.iter().any(|arg| arg == "Seed"))
        }),
        "expected clone_touch to reject the owned local Seed type because it does not implement Clone"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![copy_touch.index]),
        "expected a direct basic suite for the valid Copy mono instance"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![clone_touch.index]),
        "expected a direct basic suite for the valid Clone mono instance"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_copy_clone_bound_lib::copy_touch::<u8>(")
        }),
        "expected the generated harness to keep the valid Copy instantiation"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_copy_clone_bound_lib::clone_touch::<")
                && module.contains("String::from_utf8_lossy")
        }),
        "expected the generated harness to keep a valid Clone instantiation for String"
    );
}

#[test]
fn cargo_varies_filters_common_std_trait_candidates_with_concrete_checks() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-copy-clone-bound-lib",
        "unsafe-wrapper-common-trait-bound-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_copy_clone_bound_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_copy_clone_bound_lib");

    let debug_touch = api_metadata
        .iter()
        .find(|api| {
            api.full_name
                .starts_with("debug_touch::<std::string::String>")
        })
        .expect("expected a valid Debug mono instance for String");
    let default_touch = api_metadata
        .iter()
        .find(|api| {
            api.full_name
                .starts_with("default_touch::<std::string::String>")
        })
        .expect("expected a valid Default mono instance for String");
    let partial_eq_touch = api_metadata
        .iter()
        .find(|api| api.full_name.starts_with("partial_eq_touch::<f32>"))
        .expect("expected a valid PartialEq mono instance for f32");
    let eq_touch = api_metadata
        .iter()
        .find(|api| api.full_name.starts_with("eq_touch::<std::string::String>"))
        .expect("expected a valid Eq mono instance for String");
    let partial_ord_touch = api_metadata
        .iter()
        .find(|api| api.full_name.starts_with("partial_ord_touch::<f32>"))
        .expect("expected a valid PartialOrd mono instance for f32");
    let ord_touch = api_metadata
        .iter()
        .find(|api| {
            api.full_name
                .starts_with("ord_touch::<std::string::String>")
        })
        .expect("expected a valid Ord mono instance for String");
    let hash_touch = api_metadata
        .iter()
        .find(|api| {
            api.full_name
                .starts_with("hash_touch::<std::string::String>")
        })
        .expect("expected a valid Hash mono instance for String");

    for (prefix, invalid) in [
        ("debug_touch::<", "Seed"),
        ("default_touch::<", "Seed"),
        ("partial_eq_touch::<", "Seed"),
        ("partial_ord_touch::<", "Seed"),
        ("hash_touch::<", "Seed"),
    ] {
        assert!(
            api_metadata.iter().all(|api| {
                !(api.full_name.starts_with(prefix)
                    && api.concrete_args.iter().any(|arg| arg.contains(invalid)))
            }),
            "expected {prefix} to reject local Seed candidates that do not satisfy the trait"
        );
    }

    for prefix in ["eq_touch::<", "ord_touch::<"] {
        assert!(
            api_metadata.iter().all(|api| {
                !(api.full_name.starts_with(prefix)
                    && api.concrete_args.iter().any(|arg| arg == "f32"))
            }),
            "expected {prefix} to reject f32 because it does not satisfy the stronger total-order bound"
        );
    }

    for index in [
        debug_touch.index,
        default_touch.index,
        partial_eq_touch.index,
        eq_touch.index,
        partial_ord_touch.index,
        ord_touch.index,
        hash_touch.index,
    ] {
        assert!(
            sequence_metadata
                .iter()
                .any(|seq| seq.is_basic && seq.functions == vec![index]),
            "expected a direct basic suite for each valid common-trait mono instance"
        );
    }
}
