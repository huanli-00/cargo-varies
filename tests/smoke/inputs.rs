use super::support::{
    api_index, assert_harness_compiles_with_kani, kani_is_available, run_varies_on_fixture,
};

fn normalize_std_core_paths(value: &str) -> String {
    value
        .replace("::std::option::Option", "__VARIES_OPTION__")
        .replace("::core::option::Option", "__VARIES_OPTION__")
        .replace("std::option::Option", "__VARIES_OPTION__")
        .replace("core::option::Option", "__VARIES_OPTION__")
        .replace("::std::result::Result", "__VARIES_RESULT__")
        .replace("::core::result::Result", "__VARIES_RESULT__")
        .replace("std::result::Result", "__VARIES_RESULT__")
        .replace("core::result::Result", "__VARIES_RESULT__")
}

fn contains_normalized(haystack: &str, needle: &str) -> bool {
    normalize_std_core_paths(haystack).contains(&normalize_std_core_paths(needle))
}

#[test]
fn cargo_varies_supports_composite_symbolic_inputs_for_wrapper_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-std-lib",
        "unsafe-wrapper-composite-input-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_symbolic_std_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_symbolic_std_lib");
    let generated_modules = run.generated_modules();

    let tuple_wrap_index = api_index(&api_metadata, "tuple_wrap");
    let array_wrap_index = api_index(&api_metadata, "array_wrap");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![tuple_wrap_index]),
        "expected a direct symbolic basic sequence for the tuple-based wrapper target"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![array_wrap_index]),
        "expected a direct symbolic basic sequence for the array-based wrapper target"
    );
    assert!(
        generated_modules
            .iter()
            .any(|module| module.contains("kani::any::<(u8, u8)>()")),
        "expected generated harnesses to seed tuple inputs symbolically"
    );
    assert!(
        generated_modules
            .iter()
            .any(|module| module.contains("kani::any::<[u8; 4]>()")),
        "expected generated harnesses to seed fixed-size array inputs symbolically"
    );
}

#[test]
fn cargo_varies_enumerates_constructor_alternatives_for_mutator_inputs() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-mutator-constructors-lib",
        "unsafe-wrapper-mutator-constructors-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_mutator_constructors_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_mutator_constructors_lib");

    let counter_new_index = api_index(&api_metadata, "Counter::new");
    let bump_index = api_index(&api_metadata, "Counter::bump");
    let tune_index = api_index(&api_metadata, "tune");
    let alpha_index = api_index(&api_metadata, "Token::alpha");
    let beta_index = api_index(&api_metadata, "Token::beta");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![counter_new_index, bump_index]),
        "expected a basic constructor-to-target sequence"
    );
    assert!(
        sequence_metadata.iter().any(|seq| {
            !seq.is_basic
                && seq.functions == vec![counter_new_index, alpha_index, tune_index, bump_index]
        }),
        "expected mutator expansion to keep the alpha constructor path for the mutator input"
    );
    assert!(
        sequence_metadata.iter().any(|seq| {
            !seq.is_basic
                && seq.functions == vec![counter_new_index, beta_index, tune_index, bump_index]
        }),
        "expected mutator expansion to keep the beta constructor path for the mutator input"
    );
}

#[test]
fn cargo_varies_adapts_raw_pointer_wrapper_inputs() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-std-lib",
        "unsafe-wrapper-raw-pointer-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_symbolic_std_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_symbolic_std_lib");
    let generated_modules = run.generated_modules();

    let counter_new_index = api_index(&api_metadata, "Counter::new");
    let write_counter_index = api_index(&api_metadata, "write_counter");
    let bump_scalar_index = api_index(&api_metadata, "bump_scalar");

    assert!(
        sequence_metadata.iter().any(
            |seq| seq.is_basic && seq.functions == vec![counter_new_index, write_counter_index]
        ),
        "expected raw-pointer wrapper target to accept a constructed pointee"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![bump_scalar_index]),
        "expected raw-pointer scalar wrapper target to accept a direct symbolic pointee"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_symbolic_std_lib::write_counter(")
                && module.contains("&mut value_0 as *mut _")
        }),
        "expected generated harnesses to cast constructed values to mutable raw pointers"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("unsafe_wrapper_symbolic_std_lib::bump_scalar(")
                && module.contains("&mut symbolic_param_0 as *mut _")
        }),
        "expected generated harnesses to cast symbolic scalars to mutable raw pointers"
    );
}

#[test]
fn cargo_varies_supports_std_enum_symbolic_inputs_for_wrapper_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-std-lib",
        "unsafe-wrapper-std-enum-input-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_symbolic_std_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_symbolic_std_lib");
    let generated_modules = run.generated_modules();

    let option_wrap_index = api_index(&api_metadata, "option_wrap");
    let result_wrap_index = api_index(&api_metadata, "result_wrap");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![option_wrap_index]),
        "expected a direct symbolic basic sequence for Option-based wrapper targets"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![result_wrap_index]),
        "expected a direct symbolic basic sequence for Result-based wrapper targets"
    );
    assert!(
        generated_modules
            .iter()
            .any(|module| module.contains("Option<u8>")),
        "expected generated harnesses to seed Option inputs symbolically"
    );
    assert!(
        generated_modules
            .iter()
            .any(|module| module.contains("Result<u8, bool>")),
        "expected generated harnesses to seed Result inputs symbolically"
    );
}

#[test]
fn cargo_varies_supports_owned_std_symbolic_inputs_for_wrapper_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-std-lib",
        "unsafe-wrapper-owned-std-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_symbolic_std_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_symbolic_std_lib");
    let generated_modules = run.generated_modules();

    let string_wrap_index = api_index(&api_metadata, "string_wrap");
    let vec_wrap_index = api_index(&api_metadata, "vec_wrap");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![string_wrap_index]),
        "expected a direct symbolic basic sequence for String-based wrapper targets"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![vec_wrap_index]),
        "expected a direct symbolic basic sequence for Vec-based wrapper targets"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("String::from_utf8_lossy")
                && module.contains("kani::any::<([u8; 8], usize)>()")
                && module.contains("match symbolic_param_0 { (value, len) =>")
        }),
        "expected generated harnesses to seed String inputs from symbolic bytes"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("kani::any::<([u8; 8], usize)>()")
                && module.contains("values.into_iter().take(len)")
        }),
        "expected generated harnesses to seed Vec inputs from symbolic arrays"
    );
}

#[test]
fn cargo_varies_supports_borrowed_owned_std_symbolic_inputs_for_wrapper_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-std-lib",
        "unsafe-wrapper-borrowed-std-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_symbolic_std_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_symbolic_std_lib");
    let generated_modules = run.generated_modules();

    let borrow_string_wrap_index = api_index(&api_metadata, "borrow_string_wrap");
    let borrow_vec_wrap_index = api_index(&api_metadata, "borrow_vec_wrap");
    let borrow_pair_vec_wrap_index = api_index(&api_metadata, "borrow_pair_vec_wrap");
    let mutate_string_wrap_index = api_index(&api_metadata, "mutate_string_wrap");
    let mutate_vec_wrap_index = api_index(&api_metadata, "mutate_vec_wrap");

    for target_index in [
        borrow_string_wrap_index,
        borrow_vec_wrap_index,
        borrow_pair_vec_wrap_index,
        mutate_string_wrap_index,
        mutate_vec_wrap_index,
    ] {
        assert!(
            sequence_metadata
                .iter()
                .any(|seq| seq.is_basic && seq.functions == vec![target_index]),
            "expected a direct symbolic basic sequence for each borrowed owned-std wrapper target"
        );
    }
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("String::from_utf8_lossy")
                && module.contains(
                    "unsafe_wrapper_symbolic_std_lib::borrow_string_wrap(&*symbolic_param_0_value_",
                )
        }),
        "expected generated harnesses to reconstruct borrowed String inputs before shared borrows"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("String::from_utf8_lossy")
                && module.contains(
                    "unsafe_wrapper_symbolic_std_lib::mutate_string_wrap(&mut *symbolic_param_0_value_",
                )
        }),
        "expected generated harnesses to reconstruct borrowed String inputs before mutable borrows"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("values.into_iter().take(len)")
                && module.contains(
                    "unsafe_wrapper_symbolic_std_lib::borrow_vec_wrap(&*symbolic_param_0_value_",
                )
        }),
        "expected generated harnesses to reconstruct borrowed Vec inputs before shared borrows"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("kani::any::<([(u8, bool); 8], usize)>()")
                && module.contains(
                    "unsafe_wrapper_symbolic_std_lib::borrow_pair_vec_wrap(&*symbolic_param_0_value_",
                )
        }),
        "expected borrowed Vec<T> support to generalize over symbolic element types"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("values.into_iter().take(len)")
                && module.contains(
                    "unsafe_wrapper_symbolic_std_lib::mutate_vec_wrap(&mut *symbolic_param_0_value_",
                )
        }),
        "expected generated harnesses to reconstruct borrowed Vec inputs before mutable borrows"
    );

    if kani_is_available() {
        assert_harness_compiles_with_kani(&run.harness_dir, &run.target_dir);
    }
}

#[test]
fn cargo_varies_supports_boxed_std_symbolic_inputs_for_wrapper_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-std-lib",
        "unsafe-wrapper-boxed-std-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_symbolic_std_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_symbolic_std_lib");
    let generated_modules = run.generated_modules();

    let box_wrap_index = api_index(&api_metadata, "box_wrap");
    let borrow_box_string_wrap_index = api_index(&api_metadata, "borrow_box_string_wrap");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![box_wrap_index]),
        "expected a direct symbolic basic sequence for Box<u32> wrapper targets"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![borrow_box_string_wrap_index]),
        "expected a direct symbolic basic sequence for borrowed Box<String> wrapper targets"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("kani::any::<u32>()")
                && module.contains("::std::boxed::Box::new(symbolic_param_0)")
                && module.contains("unsafe_wrapper_symbolic_std_lib::box_wrap(")
        }),
        "expected generated harnesses to construct Box<u32> from a symbolic inner seed"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("kani::any::<([u8; 8], usize)>()")
                && module.contains("::std::boxed::Box::new(match symbolic_param_0")
                && module.contains("String::from_utf8_lossy(&value[..len])")
                && module.contains("unsafe_wrapper_symbolic_std_lib::borrow_box_string_wrap(&*symbolic_param_0_value_")
        }),
        "expected generated harnesses to reconstruct borrowed Box<String> inputs recursively from symbolic bytes"
    );

    if kani_is_available() {
        assert_harness_compiles_with_kani(&run.harness_dir, &run.target_dir);
    }
}

#[test]
fn cargo_varies_supports_nested_std_symbolic_inputs_for_wrapper_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-std-lib",
        "unsafe-wrapper-nested-std-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_symbolic_std_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_symbolic_std_lib");
    let generated_modules = run.generated_modules();

    let option_box_string_wrap_index = api_index(&api_metadata, "option_box_string_wrap");
    let result_box_string_wrap_index = api_index(&api_metadata, "result_box_string_wrap");
    let box_option_string_wrap_index = api_index(&api_metadata, "box_option_string_wrap");
    let box_result_string_wrap_index = api_index(&api_metadata, "box_result_string_wrap");

    for target_index in [
        option_box_string_wrap_index,
        result_box_string_wrap_index,
        box_option_string_wrap_index,
        box_result_string_wrap_index,
    ] {
        assert!(
            sequence_metadata
                .iter()
                .any(|seq| seq.is_basic && seq.functions == vec![target_index]),
            "expected a direct symbolic basic sequence for each nested std wrapper target"
        );
    }
    assert!(
        generated_modules.iter().any(|module| {
            contains_normalized(
                module,
                "kani::any::<std::option::Option<([u8; 8], usize)>>()",
            ) && module.contains("symbolic_param_0.map(|value| ::std::boxed::Box::new(match value")
                && module.contains("unsafe_wrapper_symbolic_std_lib::option_box_string_wrap(")
        }),
        "expected Option<Box<String>> wrappers to use seed types that avoid requiring `String: Arbitrary`"
    );
    assert!(
        generated_modules.iter().any(|module| {
            contains_normalized(
                module,
                "kani::any::<std::result::Result<([u8; 8], usize), bool>>()",
            ) && module.contains("::std::boxed::Box::new(match value")
                && module.contains("unsafe_wrapper_symbolic_std_lib::result_box_string_wrap(")
        }),
        "expected Result<Box<String>, bool> wrappers to use recursive seed reconstruction"
    );
    assert!(
        generated_modules.iter().any(|module| {
            contains_normalized(
                module,
                "kani::any::<std::option::Option<([u8; 8], usize)>>()",
            ) && module.contains("::std::boxed::Box::new(symbolic_param_0.map(|value| match value")
                && module.contains("unsafe_wrapper_symbolic_std_lib::box_option_string_wrap(")
        }),
        "expected Box<Option<String>> wrappers to reconstruct nested options before boxing"
    );
    assert!(
        generated_modules.iter().any(|module| {
            contains_normalized(
                module,
                "kani::any::<std::result::Result<([u8; 8], usize), bool>>()",
            ) && module.contains("::std::boxed::Box::new(match symbolic_param_0")
                && module.contains("unsafe_wrapper_symbolic_std_lib::box_result_string_wrap(")
        }),
        "expected Box<Result<String, bool>> wrappers to reconstruct nested results before boxing"
    );

    if kani_is_available() {
        assert_harness_compiles_with_kani(&run.harness_dir, &run.target_dir);
    }
}

#[test]
fn cargo_varies_supports_nested_vec_std_symbolic_inputs_for_wrapper_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-std-lib",
        "unsafe-wrapper-nested-vec-std-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_symbolic_std_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_symbolic_std_lib");
    let generated_modules = run.generated_modules();

    let vec_box_string_wrap_index = api_index(&api_metadata, "vec_box_string_wrap");
    let box_vec_option_string_wrap_index = api_index(&api_metadata, "box_vec_option_string_wrap");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![vec_box_string_wrap_index]),
        "expected a direct symbolic basic sequence for Vec<Box<String>> wrapper targets"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![box_vec_option_string_wrap_index]),
        "expected a direct symbolic basic sequence for Box<Vec<Option<String>>> wrapper targets"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("kani::any::<([([u8; 8], usize); 8], usize)>()")
                && module.contains(
                    "values.into_iter().take(len).map(|value| ::std::boxed::Box::new(match value",
                )
                && module.contains("unsafe_wrapper_symbolic_std_lib::vec_box_string_wrap(")
        }),
        "expected Vec<Box<String>> wrappers to reconstruct each element recursively"
    );
    assert!(
        generated_modules.iter().any(|module| {
            contains_normalized(
                module,
                "kani::any::<([std::option::Option<([u8; 8], usize)>; 8], usize)>()",
            ) && module.contains("::std::boxed::Box::new(match symbolic_param_0 { (values, len) =>")
                && module.contains(
                    "values.into_iter().take(len).map(|value| value.map(|value| match value",
                )
                && module.contains("unsafe_wrapper_symbolic_std_lib::box_vec_option_string_wrap(")
        }),
        "expected Box<Vec<Option<String>>> wrappers to reconstruct nested vector elements recursively before boxing"
    );

    if kani_is_available() {
        assert_harness_compiles_with_kani(&run.harness_dir, &run.target_dir);
    }
}

#[test]
fn cargo_varies_supports_nested_composite_std_symbolic_inputs_for_wrapper_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-std-lib",
        "unsafe-wrapper-nested-composite-std-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_symbolic_std_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_symbolic_std_lib");
    let generated_modules = run.generated_modules();

    let tuple_wrap_index = api_index(&api_metadata, "tuple_wrap");
    let array_wrap_index = api_index(&api_metadata, "array_wrap");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![tuple_wrap_index]),
        "expected a direct symbolic basic sequence for tuple wrapper targets"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![array_wrap_index]),
        "expected a direct symbolic basic sequence for array wrapper targets"
    );
    assert!(
        generated_modules.iter().any(|module| {
            contains_normalized(
                module,
                "kani::any::<(([u8; 8], usize), std::option::Option<([u8; 8], usize)>)>()",
            ) && module.contains("match symbolic_param_0 { (value0, value1) =>")
                && module.contains("::std::boxed::Box::new(match value0")
                && module.contains("value1.map(|value| match value")
                && module.contains("unsafe_wrapper_symbolic_std_lib::nested_tuple_wrap(")
        }),
        "expected tuple wrappers to reconstruct each adapted element recursively"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains("kani::any::<[([u8; 8], usize); 2]>()")
                && module
                    .contains("symbolic_param_0.map(|value| ::std::boxed::Box::new(match value")
                && module.contains("unsafe_wrapper_symbolic_std_lib::nested_array_wrap(")
        }),
        "expected fixed-size array wrappers to reconstruct each adapted element recursively"
    );

    if kani_is_available() {
        assert_harness_compiles_with_kani(&run.harness_dir, &run.target_dir);
    }
}

#[test]
fn cargo_varies_supports_crate_local_enum_symbolic_inputs_for_wrapper_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-local-lib",
        "unsafe-wrapper-local-enum-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_symbolic_local_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_symbolic_local_lib");
    let generated_modules = run.generated_modules();

    let local_wrap_index = api_index(&api_metadata, "local_payload_wrap");
    let option_wrap_index = api_index(&api_metadata, "option_local_payload_wrap");
    let result_wrap_index = api_index(&api_metadata, "result_local_payload_wrap");
    let box_wrap_index = api_index(&api_metadata, "box_local_payload_wrap");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![local_wrap_index]),
        "expected a direct symbolic basic sequence for crate-local enum wrapper targets"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![option_wrap_index]),
        "expected a direct symbolic basic sequence for Option<crate-local enum> wrapper targets"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![result_wrap_index]),
        "expected a direct symbolic basic sequence for Result<crate-local enum, bool> wrapper targets"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![box_wrap_index]),
        "expected a direct symbolic basic sequence for Box<crate-local enum> wrapper targets"
    );
    assert!(
        generated_modules.iter().any(|module| {
            contains_normalized(
                module,
                "kani::any::<(usize, (), (([u8; 8], usize),), (([u8; 8], usize), std::option::Option<([u8; 8], usize)>), (std::result::Result<([u8; 8], usize), bool>,))>()",
            ) && module.contains("unsafe_wrapper_symbolic_local_lib::LocalPayload::Text(")
                && module.contains("unsafe_wrapper_symbolic_local_lib::LocalPayload::Combo(")
                && module.contains("unsafe_wrapper_symbolic_local_lib::LocalPayload::State { result:")
                && module.contains("unsafe_wrapper_symbolic_local_lib::local_payload_wrap(")
        }),
        "expected direct crate-local enum wrappers to use a discriminant-plus-payload seed and reconstruct each variant recursively"
    );
    assert!(
        generated_modules.iter().any(|module| {
            contains_normalized(
                module,
                "kani::any::<std::option::Option<(usize, (), (([u8; 8], usize),), (([u8; 8], usize), std::option::Option<([u8; 8], usize)>), (std::result::Result<([u8; 8], usize), bool>,))>>()",
            ) && module.contains("symbolic_param_0.map(|value| match value")
                && module.contains("unsafe_wrapper_symbolic_local_lib::option_local_payload_wrap(")
        }),
        "expected Option<crate-local enum> wrappers to adapt the enum seed recursively inside Option"
    );
    assert!(
        generated_modules.iter().any(|module| {
            contains_normalized(
                module,
                "kani::any::<std::result::Result<(usize, (), (([u8; 8], usize),), (([u8; 8], usize), std::option::Option<([u8; 8], usize)>), (std::result::Result<([u8; 8], usize), bool>,)), bool>>()",
            ) && module.contains("unsafe_wrapper_symbolic_local_lib::result_local_payload_wrap(")
        }),
        "expected Result<crate-local enum, bool> wrappers to adapt the enum seed recursively inside Result"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains(
                "let mut symbolic_param_0_value_0_0 = ::std::boxed::Box::new(match symbolic_param_0",
            ) && module.contains("unsafe_wrapper_symbolic_local_lib::box_local_payload_wrap(")
        }),
        "expected Box<crate-local enum> wrappers to reconstruct the enum before boxing"
    );

    if kani_is_available() {
        assert_harness_compiles_with_kani(&run.harness_dir, &run.target_dir);
    }
}

#[test]
fn cargo_varies_supports_crate_local_struct_symbolic_inputs_for_wrapper_targets() {
    let run = run_varies_on_fixture(
        "unsafe-wrapper-symbolic-local-lib",
        "unsafe-wrapper-local-struct-harness",
        true,
        &[],
    );
    run.assert_success();

    let api_metadata = run.api_metadata("unsafe_wrapper_symbolic_local_lib");
    let sequence_metadata = run.sequence_metadata("unsafe_wrapper_symbolic_local_lib");
    let generated_modules = run.generated_modules();

    let record_wrap_index = api_index(&api_metadata, "local_record_wrap");
    let box_record_wrap_index = api_index(&api_metadata, "box_local_record_wrap");
    let tuple_wrap_index = api_index(&api_metadata, "local_tuple_wrap");
    let option_tuple_wrap_index = api_index(&api_metadata, "option_local_tuple_wrap");

    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![record_wrap_index]),
        "expected a direct symbolic basic sequence for local named-struct wrapper targets"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![box_record_wrap_index]),
        "expected a direct symbolic basic sequence for Box<local named-struct> wrapper targets"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![tuple_wrap_index]),
        "expected a direct symbolic basic sequence for tuple-struct wrapper targets"
    );
    assert!(
        sequence_metadata
            .iter()
            .any(|seq| seq.is_basic && seq.functions == vec![option_tuple_wrap_index]),
        "expected a direct symbolic basic sequence for Option<tuple-struct> wrapper targets"
    );
    assert!(
        generated_modules.iter().any(|module| {
            contains_normalized(
                module,
                "kani::any::<(([u8; 8], usize), std::option::Option<([u8; 8], usize)>, std::result::Result<([u8; 8], usize), bool>)>()",
            ) && module.contains("unsafe_wrapper_symbolic_local_lib::LocalRecord {")
                && module.contains("label: ::std::boxed::Box::new(")
                && module.contains("result: match field2")
                && module.contains("unsafe_wrapper_symbolic_local_lib::local_record_wrap(")
        }),
        "expected local named-struct wrappers to reconstruct each field recursively from a tuple seed"
    );
    assert!(
        generated_modules.iter().any(|module| {
            module.contains(
                "let mut symbolic_param_0_value_0_0 = ::std::boxed::Box::new(match symbolic_param_0 { (field0, field1, field2) => unsafe_wrapper_symbolic_local_lib::LocalRecord {",
            ) && module.contains("unsafe_wrapper_symbolic_local_lib::box_local_record_wrap(")
        }),
        "expected Box<local named-struct> wrappers to reconstruct the struct before boxing"
    );
    assert!(
        generated_modules.iter().any(|module| {
            contains_normalized(
                module,
                "kani::any::<(([u8; 8], usize), std::option::Option<([u8; 8], usize)>)>()",
            ) && module.contains("unsafe_wrapper_symbolic_local_lib::LocalTuple(")
                && module.contains("unsafe_wrapper_symbolic_local_lib::local_tuple_wrap(")
        }),
        "expected tuple-struct wrappers to reconstruct tuple fields recursively from a tuple seed"
    );
    assert!(
        generated_modules.iter().any(|module| {
            contains_normalized(
                module,
                "kani::any::<std::option::Option<(([u8; 8], usize), std::option::Option<([u8; 8], usize)>)>>()",
            ) && module.contains("symbolic_param_0.map(|value| match value")
                && module.contains("unsafe_wrapper_symbolic_local_lib::option_local_tuple_wrap(")
        }),
        "expected Option<tuple-struct> wrappers to adapt tuple-struct seeds recursively inside Option"
    );

    if kani_is_available() {
        assert_harness_compiles_with_kani(&run.harness_dir, &run.target_dir);
    }
}
