use super::common::{
    dependency_crate_ident, prepare_generated_layout,
    render_harness_manifest_with_extra_dependencies, reset_lockfile, write_suite_metadata,
};
use super::kani::render_kani_seeded_sequence_function;
use super::sequence_fn::RenderedSequenceFunction;
use super::{BackendKind, BackendRenderContext, BackendValidationCommand, SuiteBackend};
use crate::cli::VariesArgs;
use crate::std_adapters::{
    fuzz_seed_byte_len_for_value_ty, render_fuzz_seed_from_data_for_value_ty,
};
use crate::synth::SynthesizedSuite;
use crate::workspace::WorkspaceTarget;
use anyhow::{Context, Result};
use rustc_data_structures::sync::par_map;
use rustc_middle::ty::TyCtxt;
use std::ffi::OsString;
use std::fs;
use std::path::Path;

pub(super) struct FuzzBackend;
pub(super) static FUZZ_BACKEND: FuzzBackend = FuzzBackend;

impl SuiteBackend for FuzzBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Fuzz
    }

    fn write_suite<'tcx>(&self, context: BackendRenderContext<'_, 'tcx>) -> Result<()> {
        let BackendRenderContext {
            tcx,
            graph,
            target,
            harness_dir,
            suite,
            cli,
        } = context;
        write_suite_impl(tcx, graph, target, harness_dir, suite, cli)
    }

    fn validation_command(&self, manifest: &Path) -> Option<BackendValidationCommand> {
        Some(BackendValidationCommand {
            program: "cargo",
            args: vec![
                OsString::from("check"),
                OsString::from("--manifest-path"),
                manifest.as_os_str().to_owned(),
                OsString::from("--bins"),
                OsString::from("--locked"),
            ],
            description: "generated AFL fuzz targets cargo check",
        })
    }
}

fn write_suite_impl<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &crate::rapx_graph::ApiDependencyGraph<'tcx>,
    target: &WorkspaceTarget,
    harness_dir: &Path,
    suite: &SynthesizedSuite<'tcx>,
    cli: &VariesArgs,
) -> Result<()> {
    let layout = prepare_generated_layout(harness_dir)?;
    let fuzz_targets_dir = &layout.fuzz_targets_dir;
    let corpus_dir = harness_dir.join("corpus");
    let meta_dir = &layout.meta_dir;
    let dependency_crate = dependency_crate_ident(target);

    reset_lockfile(harness_dir)?;
    write_fuzz_cargo_config(tcx, harness_dir)?;

    fs::write(layout.src_dir.join("lib.rs"), render_fuzz_support_lib(tcx)).with_context(|| {
        format!(
            "failed to write {}",
            layout.src_dir.join("lib.rs").display()
        )
    })?;
    fs::create_dir_all(&corpus_dir)
        .with_context(|| format!("failed to create {}", corpus_dir.display()))?;

    let rendered_targets: Vec<std::result::Result<(usize, RenderedFuzzTarget), String>> =
        par_map(0..suite.sequences.len(), |index| {
            render_fuzz_target_file(tcx, graph, &dependency_crate, suite, index)
                .map(|rendered| (index, rendered))
        });
    let rendered_targets = rendered_targets
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(anyhow::Error::msg)?;

    let mut manifest =
        render_harness_manifest_with_extra_dependencies(target, cli, &[], ["afl = \"0.18.1\""]);
    for (index, _) in &rendered_targets {
        manifest.push_str(&format!(
            "\n[[bin]]\nname = \"fuzz_target_{index}\"\npath = \"fuzz_targets/fuzz_target_{index}.rs\"\ntest = false\ndoc = false\nbench = false\n"
        ));
    }
    fs::write(harness_dir.join("Cargo.toml"), manifest)
        .with_context(|| format!("failed to write {}/Cargo.toml", harness_dir.display()))?;

    for (index, rendered) in rendered_targets {
        fs::write(
            fuzz_targets_dir.join(format!("fuzz_target_{index}.rs")),
            rendered.source,
        )
        .with_context(|| format!("failed to write fuzz target {index}"))?;

        let target_corpus_dir = corpus_dir.join(format!("fuzz_target_{index}"));
        fs::create_dir_all(&target_corpus_dir).with_context(|| {
            format!(
                "failed to create corpus directory {}",
                target_corpus_dir.display()
            )
        })?;
        fs::write(
            target_corpus_dir.join("seed"),
            minimal_seed(rendered.min_data_len),
        )
        .with_context(|| format!("failed to write corpus seed for fuzz target {index}"))?;
    }

    write_suite_metadata(target, meta_dir, suite)?;

    Ok(())
}

fn write_fuzz_cargo_config(tcx: TyCtxt<'_>, harness_dir: &Path) -> Result<()> {
    let cargo_dir = harness_dir.join(".cargo");
    fs::create_dir_all(&cargo_dir)
        .with_context(|| format!("failed to create {}", cargo_dir.display()))?;
    fs::write(cargo_dir.join("config.toml"), render_fuzz_cargo_config(tcx)).with_context(|| {
        format!(
            "failed to write {}",
            cargo_dir.join("config.toml").display()
        )
    })
}

fn render_fuzz_cargo_config(_tcx: TyCtxt<'_>) -> String {
    r#"# Generated by cargo-varies.
# AFL exits once the current queue is drained and does not try to pin workers.
[env]
AFL_EXIT_WHEN_DONE = "1"
AFL_NO_AFFINITY = "1"
"#
    .to_owned()
}

struct RenderedFuzzTarget {
    source: String,
    min_data_len: usize,
}

fn render_fuzz_target_file<'tcx>(
    tcx: TyCtxt<'tcx>,
    graph: &crate::rapx_graph::ApiDependencyGraph<'tcx>,
    dependency_crate: &str,
    suite: &SynthesizedSuite<'tcx>,
    index: usize,
) -> std::result::Result<RenderedFuzzTarget, String> {
    let RenderedSequenceFunction {
        params,
        lines: function_lines,
    } = render_kani_seeded_sequence_function(tcx, graph, dependency_crate, suite, index);
    let mut param_lens = Vec::new();
    for param in &params {
        param_lens.push(
            fuzz_seed_byte_len_for_value_ty(tcx, param.value_ty).ok_or_else(|| {
                format!(
                    "fuzz backend cannot compute byte length for seed parameter {}: {}",
                    param.name, param.ty
                )
            })?,
        );
    }

    let mut lines = vec![
        "#![allow(dead_code, unused_imports, unused_mut, unused_variables)]".to_owned(),
        format!("extern crate {};", dependency_crate),
        "use varies_test::*;".to_owned(),
        String::new(),
    ];
    lines.extend(function_lines);
    lines.push(String::new());
    lines.push("fn main() {".to_owned());
    lines.push("    afl::fuzz!(|data: &[u8]| {".to_owned());

    let mut offset = 0usize;
    for param_len in &param_lens {
        offset += param_len;
    }
    if offset > 0 {
        lines.push(format!("        if data.len() < {offset} {{"));
        lines.push("            return;".to_owned());
        lines.push("        }".to_owned());
    }

    let mut start_index = 0usize;
    for (param, param_len) in params.iter().zip(param_lens) {
        let seed_expr =
            render_fuzz_seed_from_data_for_value_ty(tcx, param.value_ty, "data", start_index)
                .ok_or_else(|| {
                    format!(
                        "fuzz backend cannot render byte-buffer seed for parameter {}: {}",
                        param.name, param.ty
                    )
                })?;
        lines.push(format!("        let {} = {seed_expr};", param.name));
        start_index += param_len;
    }

    let call_args = params
        .iter()
        .map(|param| param.name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(format!(
        "        let _ = test_function{index}({call_args});"
    ));
    lines.push("    });".to_owned());
    lines.push("}".to_owned());
    Ok(RenderedFuzzTarget {
        source: lines.join("\n"),
        min_data_len: offset,
    })
}

fn minimal_seed(min_data_len: usize) -> Vec<u8> {
    let len = min_data_len.max(1);
    (0..len).map(|index| index.wrapping_add(1) as u8).collect()
}

fn render_fuzz_support_lib(tcx: TyCtxt<'_>) -> String {
    let (usize_reader, isize_reader) = match tcx.data_layout.pointer_size().bytes() {
        1 => (
            "_to_u8(data, index) as usize",
            "_to_i8(data, index) as isize",
        ),
        2 => (
            "_to_u16(data, index) as usize",
            "_to_i16(data, index) as isize",
        ),
        4 => (
            "_to_u32(data, index) as usize",
            "_to_i32(data, index) as isize",
        ),
        8 => (
            "_to_u64(data, index) as usize",
            "_to_i64(data, index) as isize",
        ),
        16 => (
            "_to_u128(data, index) as usize",
            "_to_i128(data, index) as isize",
        ),
        width => panic!("unsupported target pointer width for fuzz backend: {width} bytes"),
    };

    let mut lines = [
        "#![allow(dead_code, unused_imports, unused_mut, unused_variables)]",
        "",
        "pub fn _read<const N: usize>(data: &[u8], index: usize) -> [u8; N] {",
        "    let mut bytes = [0u8; N];",
        "    bytes.copy_from_slice(&data[index..index + N]);",
        "    bytes",
        "}",
        "",
        "pub fn _to_u8(data: &[u8], index: usize) -> u8 {",
        "    data[index]",
        "}",
        "",
        "pub fn _to_i8(data: &[u8], index: usize) -> i8 {",
        "    data[index] as i8",
        "}",
        "",
        "pub fn _to_u16(data: &[u8], index: usize) -> u16 {",
        "    u16::from_le_bytes(_read(data, index))",
        "}",
        "",
        "pub fn _to_i16(data: &[u8], index: usize) -> i16 {",
        "    i16::from_le_bytes(_read(data, index))",
        "}",
        "",
        "pub fn _to_u32(data: &[u8], index: usize) -> u32 {",
        "    u32::from_le_bytes(_read(data, index))",
        "}",
        "",
        "pub fn _to_i32(data: &[u8], index: usize) -> i32 {",
        "    i32::from_le_bytes(_read(data, index))",
        "}",
        "",
        "pub fn _to_u64(data: &[u8], index: usize) -> u64 {",
        "    u64::from_le_bytes(_read(data, index))",
        "}",
        "",
        "pub fn _to_i64(data: &[u8], index: usize) -> i64 {",
        "    i64::from_le_bytes(_read(data, index))",
        "}",
        "",
        "pub fn _to_u128(data: &[u8], index: usize) -> u128 {",
        "    u128::from_le_bytes(_read(data, index))",
        "}",
        "",
        "pub fn _to_i128(data: &[u8], index: usize) -> i128 {",
        "    i128::from_le_bytes(_read(data, index))",
        "}",
        "",
    ]
    .iter()
    .map(ToString::to_string)
    .collect::<Vec<_>>();

    lines.extend(
        [
            "pub fn _to_usize(data: &[u8], index: usize) -> usize {",
            &format!("    {usize_reader}"),
            "}",
            "",
            "pub fn _to_isize(data: &[u8], index: usize) -> isize {",
            &format!("    {isize_reader}"),
            "}",
            "",
        ]
        .iter()
        .map(ToString::to_string),
    );

    lines.extend(
        [
            "pub fn _to_f32(data: &[u8], index: usize) -> f32 {",
            "    f32::from_le_bytes(_read(data, index))",
            "}",
            "",
            "pub fn _to_f64(data: &[u8], index: usize) -> f64 {",
            "    f64::from_le_bytes(_read(data, index))",
            "}",
            "",
            "pub fn _to_bool(data: &[u8], index: usize) -> bool {",
            "    data[index] % 2 == 0",
            "}",
            "",
            "pub fn _to_char(data: &[u8], index: usize) -> char {",
            "    char::from_u32(_to_u32(data, index)).unwrap_or('\\0')",
            "}",
            "",
        ]
        .iter()
        .map(ToString::to_string),
    );

    lines.join("\n")
}
