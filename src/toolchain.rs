use anyhow::{Context, Result};
use std::env;
use std::process::Command;

pub(crate) fn pin_current_toolchain(command: &mut Command) -> Result<()> {
    if let Some(toolchain) = current_rustup_toolchain()? {
        command.env("RUSTUP_TOOLCHAIN", toolchain);
    }
    Ok(())
}

fn current_rustup_toolchain() -> Result<Option<String>> {
    if let Some(toolchain) = env::var_os("RUSTUP_TOOLCHAIN") {
        let toolchain = toolchain
            .into_string()
            .map_err(|_| anyhow::anyhow!("RUSTUP_TOOLCHAIN contained non-utf8 data"))?;
        if !toolchain.is_empty() {
            return Ok(Some(toolchain));
        }
    }

    let output = match Command::new("rustup")
        .arg("show")
        .arg("active-toolchain")
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).context("failed to invoke `rustup show active-toolchain`")?;
        }
    };

    if !output.status.success() {
        return Ok(None);
    }

    Ok(parse_active_toolchain_output(&output.stdout))
}

fn parse_active_toolchain_output(stdout: &[u8]) -> Option<String> {
    std::str::from_utf8(stdout)
        .ok()?
        .split_whitespace()
        .next()
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::parse_active_toolchain_output;

    #[test]
    fn parse_active_toolchain_output_extracts_toolchain_name() {
        assert_eq!(
            parse_active_toolchain_output(
                b"nightly-2025-10-23-x86_64-unknown-linux-gnu (overridden by '/tmp/project')\n"
            ),
            Some("nightly-2025-10-23-x86_64-unknown-linux-gnu".to_owned())
        );
    }

    #[test]
    fn parse_active_toolchain_output_ignores_empty_output() {
        assert_eq!(parse_active_toolchain_output(b"\n"), None);
    }
}
