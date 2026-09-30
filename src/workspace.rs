use crate::toolchain::pin_current_toolchain;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct WorkspaceTarget {
    pub manifest_dir: PathBuf,
    pub package_name: String,
    pub lib_crate_name: String,
    pub available_features: Vec<String>,
}

pub fn discover_target(dir: &Path) -> Result<WorkspaceTarget> {
    discover_target_with_package(dir, None)
}

pub fn discover_target_for_analysis(
    fallback_dir: &Path,
    requested_package: Option<&str>,
) -> Result<WorkspaceTarget> {
    if let (Some(manifest_dir), Some(package_name)) = (
        env::var_os("CARGO_MANIFEST_DIR"),
        env::var("CARGO_PKG_NAME").ok(),
    ) {
        return discover_target_with_package(Path::new(&manifest_dir), Some(&package_name));
    }

    if let Some(package_name) = requested_package {
        discover_target_with_package(fallback_dir, Some(package_name))
    } else {
        discover_target(fallback_dir)
    }
}

pub fn discover_target_with_package(
    dir: &Path,
    requested_package: Option<&str>,
) -> Result<WorkspaceTarget> {
    let mut command = Command::new("cargo");
    command
        .arg("metadata")
        .arg("--no-deps")
        .arg("--format-version")
        .arg("1")
        .current_dir(dir);
    pin_current_toolchain(&mut command)?;
    let output = command
        .output()
        .with_context(|| format!("failed to invoke `cargo metadata` in {}", dir.display()))?;
    if !output.status.success() {
        bail!(
            "`cargo metadata` failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let metadata: MetadataOutput =
        serde_json::from_slice(&output.stdout).context("failed to parse cargo metadata JSON")?;
    let package = select_package(&metadata, requested_package)?;
    let lib_target = package
        .targets
        .iter()
        .find(|target| target.kind.iter().any(|kind| kind == "lib"))
        .context("target package does not define a library target")?;

    let manifest_dir = PathBuf::from(&package.manifest_path)
        .parent()
        .context("manifest path should have a parent directory")?
        .to_path_buf();

    if !manifest_dir.exists() {
        bail!(
            "resolved manifest directory does not exist: {}",
            manifest_dir.display()
        );
    }

    Ok(WorkspaceTarget {
        manifest_dir,
        package_name: package.name.clone(),
        lib_crate_name: lib_target.name.clone(),
        available_features: package
            .features
            .keys()
            .filter(|feature| feature.as_str() != "default")
            .cloned()
            .collect(),
    })
}

fn select_package<'a>(
    metadata: &'a MetadataOutput,
    requested_package: Option<&str>,
) -> Result<&'a MetadataPackage> {
    if let Some(package_name) = requested_package {
        return metadata
            .packages
            .iter()
            .find(|pkg| pkg.name == package_name)
            .with_context(|| {
                format!(
                    "package `{package_name}` was not found; available packages: {}",
                    package_list(&metadata.packages)
                )
            });
    }

    let root_id = metadata
        .resolve
        .as_ref()
        .and_then(|resolve| resolve.root.clone())
        .or_else(|| {
            if metadata.workspace_default_members.len() == 1 {
                metadata.workspace_default_members.first().cloned()
            } else if metadata.packages.len() == 1 {
                metadata.packages.first().map(|pkg| pkg.id.clone())
            } else {
                None
            }
        })
        .with_context(|| {
            format!(
                "could not infer a single target package from {} packages; rerun with `--package <name>`. Available packages: {}",
                metadata.packages.len(),
                package_list(&metadata.packages)
            )
        })?;

    metadata
        .packages
        .iter()
        .find(|pkg| pkg.id == root_id)
        .context("root package id was not found in package list")
}

fn package_list(packages: &[MetadataPackage]) -> String {
    packages
        .iter()
        .map(|pkg| pkg.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Deserialize)]
struct MetadataOutput {
    packages: Vec<MetadataPackage>,
    #[serde(default)]
    resolve: Option<MetadataResolve>,
    #[serde(default)]
    workspace_default_members: Vec<String>,
}

#[derive(Deserialize)]
struct MetadataResolve {
    root: Option<String>,
}

#[derive(Deserialize)]
struct MetadataPackage {
    id: String,
    name: String,
    manifest_path: String,
    #[serde(default)]
    features: BTreeMap<String, Vec<String>>,
    targets: Vec<MetadataTarget>,
}

#[derive(Deserialize)]
struct MetadataTarget {
    name: String,
    kind: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{MetadataOutput, discover_target, discover_target_with_package};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn metadata_parse_accepts_null_resolve() {
        let metadata = serde_json::from_str::<MetadataOutput>(
            r#"{
                "packages":[
                    {
                        "id":"pkgid",
                        "name":"basic-lib",
                        "version":"0.1.0",
                        "manifest_path":"/tmp/basic-lib/Cargo.toml",
                        "targets":[{"name":"basic_lib","kind":["lib"]}]
                    }
                ],
                "workspace_root":"/tmp/basic-lib",
                "workspace_default_members":["pkgid"],
                "resolve":null
            }"#,
        )
        .expect("metadata JSON should parse");

        assert!(metadata.resolve.is_none());
        assert_eq!(metadata.workspace_default_members, vec!["pkgid"]);
    }

    #[test]
    fn discover_target_handles_single_package_with_null_resolve() {
        let temp = tempdir().expect("tempdir should exist");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"single\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n",
        )
        .expect("manifest should be written");
        fs::create_dir(temp.path().join("src")).expect("src dir should exist");
        fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn value() -> u32 { 0 }\n",
        )
        .expect("lib should be written");

        let target = discover_target(temp.path()).expect("single package should resolve");
        assert_eq!(target.package_name, "single");
        assert_eq!(target.lib_crate_name, "single");
        assert_eq!(target.manifest_dir, temp.path());
    }

    #[test]
    fn discover_target_with_package_handles_virtual_workspace() {
        let temp = tempdir().expect("tempdir should exist");
        fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"helper\", \"target\"]\nresolver = \"3\"\n",
        )
        .expect("workspace manifest should be written");

        for member in ["helper", "target"] {
            let member_dir = temp.path().join(member);
            fs::create_dir(&member_dir).expect("member dir should exist");
            fs::create_dir(member_dir.join("src")).expect("member src dir should exist");
            fs::write(
                member_dir.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{member}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n"
                ),
            )
            .expect("member manifest should be written");
            fs::write(
                member_dir.join("src/lib.rs"),
                "pub fn value() -> u32 { 0 }\n",
            )
            .expect("member lib should be written");
        }

        let target = discover_target_with_package(temp.path(), Some("target"))
            .expect("explicit package selection should resolve");
        assert_eq!(target.package_name, "target");
        assert_eq!(target.lib_crate_name, "target");
        assert_eq!(target.manifest_dir, temp.path().join("target"));
    }
}
