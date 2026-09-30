use crate::cli::VariesArgs;
use crate::synth::SynthesizedSuite;
use crate::workspace::WorkspaceTarget;
use anyhow::{Context, Result};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
struct ApiFunctionMetadata {
    full_name: String,
    instantiated_path: String,
    concrete_args: Vec<String>,
    index: usize,
    is_mono: bool,
    api_safety: String,
    supported: bool,
    unsafe_wrappers: Vec<String>,
}

#[derive(Serialize)]
struct SequenceMetadata {
    index: usize,
    length: usize,
    target: usize,
    unsafe_wrapper: String,
    is_basic: bool,
    predecessor: usize,
    successor: Vec<usize>,
    latest_call: usize,
    latest_mutator: Option<usize>,
    latest_mutator_path: Option<String>,
    latest_mutator_param: Option<usize>,
    ranking_value: f32,
    construction_cost: f32,
    functions: Vec<usize>,
    unsafe_wrappers: Vec<String>,
}

pub(super) struct GeneratedLayout {
    pub src_dir: PathBuf,
    pub fuzz_targets_dir: PathBuf,
    pub meta_dir: PathBuf,
}

pub(super) fn prepare_generated_layout(harness_dir: &Path) -> Result<GeneratedLayout> {
    let src_dir = harness_dir.join("src");
    let fuzz_targets_dir = harness_dir.join("fuzz_targets");
    let corpus_dir = harness_dir.join("corpus");
    let meta_dir = harness_dir.join("varies_meta");
    let target_tmp_dir = harness_dir.join("target").join("tmp");

    clean_generated_dir(&src_dir)?;
    clean_generated_dir(&fuzz_targets_dir)?;
    clean_generated_dir(&corpus_dir)?;
    clean_generated_dir(&meta_dir)?;
    fs::create_dir_all(&src_dir)
        .with_context(|| format!("failed to create {}", src_dir.display()))?;
    fs::create_dir_all(&fuzz_targets_dir)
        .with_context(|| format!("failed to create {}", fuzz_targets_dir.display()))?;
    fs::create_dir_all(&meta_dir)
        .with_context(|| format!("failed to create {}", meta_dir.display()))?;
    fs::create_dir_all(&target_tmp_dir)
        .with_context(|| format!("failed to create {}", target_tmp_dir.display()))?;

    Ok(GeneratedLayout {
        src_dir,
        fuzz_targets_dir,
        meta_dir,
    })
}

pub(super) fn reset_lockfile(harness_dir: &Path) -> Result<()> {
    let harness_lockfile = harness_dir.join("Cargo.lock");

    if harness_lockfile.exists() {
        fs::remove_file(&harness_lockfile).with_context(|| {
            format!(
                "failed to remove stale harness lockfile {}",
                harness_lockfile.display()
            )
        })?;
    }

    Ok(())
}

pub(super) fn write_suite_metadata(
    target: &WorkspaceTarget,
    meta_dir: &Path,
    suite: &SynthesizedSuite<'_>,
) -> Result<()> {
    let api_metadata = suite
        .apis
        .iter()
        .map(|api| ApiFunctionMetadata {
            full_name: api.path.clone(),
            instantiated_path: api.instantiated_path.clone(),
            concrete_args: api.concrete_args.clone(),
            index: api.index,
            is_mono: api.is_mono,
            api_safety: api.api_safety.as_str().to_owned(),
            supported: api.supported,
            unsafe_wrappers: api.unsafe_wrappers_without_std_internal(),
        })
        .collect::<Vec<_>>();
    fs::write(
        meta_dir.join(format!("{}_api_functions.json", target.lib_crate_name)),
        serde_json::to_string_pretty(&api_metadata).context("failed to serialize api metadata")?,
    )
    .context("failed to write api_functions metadata")?;

    let sequence_metadata = suite
        .sequences
        .iter()
        .enumerate()
        .map(|(index, sequence)| {
            let latest_mutator = (sequence.latest_call != sequence.target_step)
                .then_some(sequence.steps[sequence.latest_call].api_index);
            let latest_mutator_path =
                latest_mutator.map(|api_index| suite.apis[api_index].path.clone());

            SequenceMetadata {
                index,
                length: sequence.steps.len(),
                target: sequence.target_api,
                unsafe_wrapper: sequence.unsafe_wrapper.clone(),
                is_basic: sequence.predecessor.is_none(),
                predecessor: sequence.predecessor.unwrap_or(0),
                successor: sequence.successor.clone(),
                latest_call: sequence.latest_call,
                latest_mutator,
                latest_mutator_path,
                latest_mutator_param: sequence.mutated_param,
                ranking_value: sequence.ranking_value,
                construction_cost: sequence.steps.len() as f32,
                functions: sequence.steps.iter().map(|step| step.api_index).collect(),
                unsafe_wrappers: suite.apis[sequence.target_api]
                    .unsafe_wrappers_without_std_internal(),
            }
        })
        .collect::<Vec<_>>();
    fs::write(
        meta_dir.join(format!("{}_api_sequences.json", target.lib_crate_name)),
        serde_json::to_string_pretty(&sequence_metadata)
            .context("failed to serialize sequence metadata")?,
    )
    .context("failed to write api_sequences metadata")?;

    Ok(())
}

pub(super) fn sequence_module_name(crate_name: &str, index: usize) -> String {
    format!("test_{}{}", sanitize_rust_identifier(crate_name), index)
}

pub(super) fn qualify_api_path(crate_name: &str, path: &str) -> String {
    if let Some(qualified) = qualify_ufcs_path(crate_name, path) {
        return sanitize_named_lifetimes(&qualified);
    }
    sanitize_named_lifetimes(&qualify_embedded_path(crate_name, path))
}

pub(super) fn dependency_crate_ident(target: &WorkspaceTarget) -> String {
    sanitize_rust_identifier(&target.lib_crate_name)
}

pub(super) fn render_harness_manifest(
    target: &WorkspaceTarget,
    cli: &VariesArgs,
    extra_sections: &[&str],
) -> String {
    render_harness_manifest_with_extra_dependencies(
        target,
        cli,
        extra_sections,
        std::iter::empty::<&str>(),
    )
}

pub(super) fn render_harness_manifest_with_extra_dependencies<'a>(
    target: &WorkspaceTarget,
    cli: &VariesArgs,
    extra_sections: &[&str],
    extra_dependencies: impl IntoIterator<Item = &'a str>,
) -> String {
    let mut dependency_fields = Vec::new();
    let dependency_crate = dependency_crate_ident(target);

    if target.package_name != dependency_crate {
        dependency_fields.push(format!("package = {:?}", target.package_name));
    }
    dependency_fields.push(format!("path = {:?}", target.manifest_dir));

    let dependency_features = render_dependency_features(target, cli);
    if cli.no_default_features {
        dependency_fields.push("default-features = false".to_owned());
    }
    if !dependency_features.is_empty() {
        dependency_fields.push(format!(
            "features = [{}]",
            dependency_features
                .iter()
                .map(|feature| format!("{feature:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    let package_line = format!(
        "{} = {{ {} }}",
        dependency_crate,
        dependency_fields.join(", ")
    );
    let extra_sections = extra_sections.join("\n");
    let extra_sections = if extra_sections.is_empty() {
        String::new()
    } else {
        format!("\n{extra_sections}")
    };

    let extra_dependencies = extra_dependencies
        .into_iter()
        .collect::<Vec<_>>()
        .join("\n");
    let extra_dependencies = if extra_dependencies.is_empty() {
        String::new()
    } else {
        format!("\n{extra_dependencies}")
    };

    format!(
        "[package]\nname = \"varies_test\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[lib]\npath = \"src/lib.rs\"\ndoctest = false\n{extra_sections}\n\n[dependencies]\n{package_line}{extra_dependencies}\n"
    )
}

pub(super) fn sanitize_rust_identifier(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());

    for ch in name.chars() {
        let valid = ch == '_' || ch.is_ascii_alphanumeric();
        sanitized.push(if valid { ch } else { '_' });
    }

    if sanitized.is_empty() {
        sanitized.push_str("crate_");
    }

    if sanitized
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_digit())
    {
        sanitized.insert(0, '_');
    }

    sanitized
}

fn render_dependency_features(target: &WorkspaceTarget, cli: &VariesArgs) -> Vec<String> {
    if cli.all_features {
        return target.available_features.to_vec();
    }

    let mut features = cli
        .features
        .iter()
        .filter_map(|feature| normalize_dependency_feature(feature, &target.package_name))
        .collect::<Vec<_>>();
    features.sort();
    features.dedup();
    features
}

fn normalize_dependency_feature(feature: &str, package_name: &str) -> Option<String> {
    match feature.split_once('/') {
        Some((package, feature_name)) if package == package_name && !feature_name.is_empty() => {
            Some(feature_name.to_owned())
        }
        Some(_) => None,
        None if feature == "default" || feature.is_empty() => None,
        None => Some(feature.to_owned()),
    }
}

fn clean_generated_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let entry_path = entry.path();
        if entry_path.is_dir() {
            fs::remove_dir_all(&entry_path)
                .with_context(|| format!("failed to remove {}", entry_path.display()))?;
        } else {
            fs::remove_file(&entry_path)
                .with_context(|| format!("failed to remove {}", entry_path.display()))?;
        }
    }
    Ok(())
}

fn qualify_ufcs_path(crate_name: &str, path: &str) -> Option<String> {
    if !path.starts_with('<') {
        return None;
    }
    let as_marker = " as ";
    let tail_marker = ">::";
    let as_index = path.find(as_marker)?;
    let tail_index = path.rfind(tail_marker)?;
    let self_ty = &path[1..as_index];
    let trait_path = &path[as_index + as_marker.len()..tail_index];
    let item = &path[tail_index + tail_marker.len()..];
    Some(format!(
        "<{} as {}>::{}",
        qualify_embedded_path(crate_name, self_ty.trim()),
        qualify_embedded_path(crate_name, trait_path.trim()),
        item
    ))
}

fn qualify_embedded_path(crate_name: &str, path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed.starts_with('!') || trimmed.starts_with('\'') {
        return trimmed.to_owned();
    }

    if let Some(rest) = trimmed.strip_prefix("&mut ") {
        return format!("&mut {}", qualify_embedded_path(crate_name, rest));
    }
    if let Some(rest) = trimmed.strip_prefix('&') {
        return format!("&{}", qualify_embedded_path(crate_name, rest));
    }
    if let Some(rest) = trimmed.strip_prefix("*const ") {
        return format!("*const {}", qualify_embedded_path(crate_name, rest));
    }
    if let Some(rest) = trimmed.strip_prefix("*mut ") {
        return format!("*mut {}", qualify_embedded_path(crate_name, rest));
    }
    if let Some(qualified) = qualify_parenthesized_path(crate_name, trimmed) {
        return qualified;
    }
    if let Some(qualified) = qualify_bracketed_path(crate_name, trimmed) {
        return qualified;
    }
    qualify_path_like(crate_name, trimmed)
}

fn qualify_parenthesized_path(crate_name: &str, path: &str) -> Option<String> {
    let inner = strip_outer_delimiters(path, '(', ')')?;
    if inner.trim().is_empty() {
        return Some("()".to_owned());
    }
    let parts = split_top_level(inner, ',')
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return Some("()".to_owned());
    }

    let mut rendered = parts
        .iter()
        .map(|part| qualify_embedded_path(crate_name, part))
        .collect::<Vec<_>>()
        .join(", ");
    if inner.trim_end().ends_with(',') {
        rendered.push(',');
    }
    Some(format!("({rendered})"))
}

fn qualify_bracketed_path(crate_name: &str, path: &str) -> Option<String> {
    let inner = strip_outer_delimiters(path, '[', ']')?;
    if let Some((element, len)) = split_top_level_once(inner, ';') {
        return Some(format!(
            "[{}; {}]",
            qualify_embedded_path(crate_name, element),
            len.trim()
        ));
    }
    Some(format!("[{}]", qualify_embedded_path(crate_name, inner)))
}

fn qualify_path_like(crate_name: &str, path: &str) -> String {
    if let Some(rest) = path.strip_prefix("::") {
        return format!("::{}", qualify_path_like(crate_name, rest));
    }

    let mut segments = split_top_level_path_segments(path)
        .into_iter()
        .map(|segment| qualify_path_segment(crate_name, segment))
        .collect::<Vec<_>>();

    if let Some(first) = segments.first_mut() {
        let root = first
            .split([':', '<'])
            .next()
            .map(str::trim)
            .unwrap_or_default();
        if !root.is_empty()
            && !root.starts_with(crate_name)
            && !is_builtin_type_path(root)
            && !has_std_like_root(root)
            && root != "_"
            && root != "Self"
        {
            *first = format!("{crate_name}::{first}");
        }
    }

    segments.join("::")
}

fn qualify_path_segment(crate_name: &str, segment: &str) -> String {
    let mut qualified = String::with_capacity(segment.len());
    let chars = segment.char_indices().collect::<Vec<_>>();
    let mut cursor = 0usize;

    while cursor < chars.len() {
        let (byte_index, ch) = chars[cursor];
        if ch != '<' {
            qualified.push(ch);
            cursor += 1;
            continue;
        }

        let close = matching_angle_index(segment, byte_index)
            .expect("generic argument list should have a closing `>`");
        qualified
            .push_str(&segment[chars[cursor - 1].0 + chars[cursor - 1].1.len_utf8()..byte_index]);
        let args = &segment[byte_index + 1..close];
        let rendered_args = split_top_level(args, ',')
            .into_iter()
            .map(|arg| qualify_embedded_path(crate_name, arg))
            .collect::<Vec<_>>()
            .join(", ");
        qualified.push('<');
        qualified.push_str(&rendered_args);
        qualified.push('>');

        let next_cursor = chars
            .iter()
            .position(|(index, _)| *index >= close + 1)
            .unwrap_or(chars.len());
        cursor = next_cursor;
    }

    if cursor == 0 {
        return segment.to_owned();
    }

    if let Some((last_index, last_char)) = chars.get(cursor.saturating_sub(1)) {
        let tail_start = *last_index + last_char.len_utf8();
        if tail_start < segment.len() {
            qualified.push_str(&segment[tail_start..]);
        }
    }

    qualified
}

fn split_top_level_path_segments(path: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut angle = 0usize;
    let mut paren = 0usize;
    let mut bracket = 0usize;
    let bytes = path.as_bytes();
    let mut index = 0usize;

    while index + 1 < bytes.len() {
        match bytes[index] {
            b'<' => angle += 1,
            b'>' => angle = angle.saturating_sub(1),
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b'[' => bracket += 1,
            b']' => bracket = bracket.saturating_sub(1),
            b':' if angle == 0 && paren == 0 && bracket == 0 && bytes[index + 1] == b':' => {
                if bytes.get(index + 2) == Some(&b'<') {
                    index += 2;
                    continue;
                }
                segments.push(path[start..index].trim());
                index += 2;
                start = index;
                continue;
            }
            _ => {}
        }
        index += 1;
    }

    segments.push(path[start..].trim());
    segments
}

fn split_top_level(input: &str, delimiter: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut angle = 0usize;
    let mut paren = 0usize;
    let mut bracket = 0usize;

    for (index, ch) in input.char_indices() {
        match ch {
            '<' => angle += 1,
            '>' => angle = angle.saturating_sub(1),
            '(' => paren += 1,
            ')' => paren = paren.saturating_sub(1),
            '[' => bracket += 1,
            ']' => bracket = bracket.saturating_sub(1),
            _ if ch == delimiter && angle == 0 && paren == 0 && bracket == 0 => {
                parts.push(input[start..index].trim());
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }

    parts.push(input[start..].trim());
    parts
}

fn split_top_level_once(input: &str, delimiter: char) -> Option<(&str, &str)> {
    let mut angle = 0usize;
    let mut paren = 0usize;
    let mut bracket = 0usize;

    for (index, ch) in input.char_indices() {
        match ch {
            '<' => angle += 1,
            '>' => angle = angle.saturating_sub(1),
            '(' => paren += 1,
            ')' => paren = paren.saturating_sub(1),
            '[' => bracket += 1,
            ']' => bracket = bracket.saturating_sub(1),
            _ if ch == delimiter && angle == 0 && paren == 0 && bracket == 0 => {
                return Some((input[..index].trim(), input[index + ch.len_utf8()..].trim()));
            }
            _ => {}
        }
    }

    None
}

fn strip_outer_delimiters(path: &str, open: char, close: char) -> Option<&str> {
    let trimmed = path.trim();
    if !trimmed.starts_with(open) || !trimmed.ends_with(close) {
        return None;
    }
    let close_index = matching_delimiter_index(trimmed, 0, open, close)?;
    if close_index + close.len_utf8() != trimmed.len() {
        return None;
    }
    Some(&trimmed[open.len_utf8()..close_index])
}

fn matching_delimiter_index(
    path: &str,
    open_index: usize,
    open: char,
    close: char,
) -> Option<usize> {
    let mut depth = 0usize;
    for (index, ch) in path[open_index..].char_indices() {
        let index = open_index + index;
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn matching_angle_index(path: &str, open_index: usize) -> Option<usize> {
    matching_delimiter_index(path, open_index, '<', '>')
}

fn sanitize_named_lifetimes(path: &str) -> String {
    let mut sanitized = String::with_capacity(path.len());
    let mut chars = path.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\''
            && let Some(next) = chars.peek().copied()
        {
            if next == '_' {
                sanitized.push(ch);
                sanitized.push(chars.next().expect("peeked lifetime placeholder"));
                continue;
            }
            if next.is_ascii_alphabetic() {
                sanitized.push('\'');
                sanitized.push('_');
                while chars
                    .peek()
                    .is_some_and(|candidate| candidate.is_ascii_alphanumeric() || *candidate == '_')
                {
                    chars.next();
                }
                continue;
            }
        }
        sanitized.push(ch);
    }

    sanitized
}

fn has_std_like_root(path: &str) -> bool {
    path.split([':', '<'])
        .next()
        .is_some_and(|root| matches!(root, "core" | "std" | "alloc"))
}

fn is_builtin_type_path(path: &str) -> bool {
    matches!(
        path,
        "bool"
            | "char"
            | "str"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
    )
}

#[cfg(test)]
mod tests {
    use super::{
        qualify_api_path, render_harness_manifest, sanitize_named_lifetimes,
        sanitize_rust_identifier, sequence_module_name,
    };
    use crate::backend::BackendKind;
    use crate::cli::{LogLevel, VariesArgs};
    use crate::workspace::WorkspaceTarget;
    use std::path::PathBuf;

    #[test]
    fn sanitize_rust_identifier_normalizes_invalid_crate_names() {
        assert_eq!(sanitize_rust_identifier("sxd-document"), "sxd_document");
        assert_eq!(sanitize_rust_identifier("metrics-util"), "metrics_util");
        assert_eq!(sanitize_rust_identifier("async-lock"), "async_lock");
        assert_eq!(sanitize_rust_identifier("123crate"), "_123crate");
    }

    #[test]
    fn generated_names_use_sanitized_crate_identifiers() {
        assert_eq!(
            sequence_module_name("sxd-document", 7),
            "test_sxd_document7"
        );
        assert_eq!(
            qualify_api_path("async_lock", "Counter::new"),
            "async_lock::Counter::new"
        );
    }

    #[test]
    fn qualify_api_path_rewrites_named_lifetimes() {
        assert_eq!(
            qualify_api_path("flatgeobuf", "Feature::<'a>::geometry"),
            "flatgeobuf::Feature::<'_>::geometry"
        );
    }

    #[test]
    fn qualify_api_path_qualifies_local_generic_arguments() {
        assert_eq!(
            qualify_api_path(
                "unsafe_wrapper_generic_cursor_lib",
                "cursor_touch::<Reader06>"
            ),
            "unsafe_wrapper_generic_cursor_lib::cursor_touch::<unsafe_wrapper_generic_cursor_lib::Reader06>"
        );
    }

    #[test]
    fn qualify_api_path_qualifies_nested_reference_generic_arguments() {
        assert_eq!(
            qualify_api_path(
                "unsafe_wrapper_generic_cursor_lib",
                "cursor_touch::<&mut Reader06>"
            ),
            "unsafe_wrapper_generic_cursor_lib::cursor_touch::<&mut unsafe_wrapper_generic_cursor_lib::Reader06>"
        );
    }

    #[test]
    fn qualify_api_path_preserves_tuple_type_syntax() {
        assert_eq!(
            qualify_api_path("local_crate", "touch::<(Reader06,)>"),
            "local_crate::touch::<(local_crate::Reader06,)>"
        );
        assert_eq!(
            qualify_api_path("local_crate", "touch::<()>"),
            "local_crate::touch::<()>"
        );
    }

    #[test]
    fn sanitize_named_lifetimes_keeps_placeholder_lifetimes() {
        assert_eq!(
            sanitize_named_lifetimes("<Cursor<'_> as Iterator>::next"),
            "<Cursor<'_> as Iterator>::next"
        );
    }

    #[test]
    fn render_harness_manifest_renames_hyphenated_dependencies() {
        let target = WorkspaceTarget {
            package_name: "metrics-util".to_owned(),
            lib_crate_name: "metrics-util".to_owned(),
            manifest_dir: PathBuf::from("/tmp/metrics-util"),
            available_features: Vec::new(),
        };

        let manifest = render_harness_manifest(&target, &sample_cli_args(), &[]);

        assert!(
            manifest.contains("metrics_util = { package = \"metrics-util\"")
                && manifest.contains("path = \"/tmp/metrics-util\""),
            "expected generated harness manifest to rename hyphenated dependencies to a valid crate identifier"
        );
    }

    #[test]
    fn render_harness_manifest_includes_extra_sections() {
        let target = WorkspaceTarget {
            package_name: "basic-lib".to_owned(),
            lib_crate_name: "basic-lib".to_owned(),
            manifest_dir: PathBuf::from("/tmp/basic-lib"),
            available_features: Vec::new(),
        };

        let manifest = render_harness_manifest(
            &target,
            &sample_cli_args(),
            &["[lints.rust]", "unexpected_cfgs = { level = \"allow\" }"],
        );

        assert!(
            manifest.contains("[lints.rust]")
                && manifest.contains("unexpected_cfgs = { level = \"allow\" }"),
            "expected backend-specific manifest sections to be inserted before dependencies"
        );
    }

    fn sample_cli_args() -> VariesArgs {
        VariesArgs {
            harness_crate: PathBuf::from("varies_test"),
            backends: vec![BackendKind::Kani],
            timeout: None,
            check_harness: false,
            max_depth: 0,
            max_mutators_per_target: 2,
            dir: None,
            manifest_path: None,
            package: None,
            features: Vec::new(),
            all_features: false,
            no_default_features: false,
            log_level: LogLevel::Info,
        }
    }
}
