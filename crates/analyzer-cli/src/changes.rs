//! Inputs for change mode. The engine compares and filters findings after a full scan.
use analyzer_core::{
    report::diff::{apply_changes, PreviousReport},
    AnalysisReport,
};
use anyhow::Context;
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Component, Path, PathBuf},
    process::Command,
};

pub fn apply(
    report: &mut AnalysisReport,
    compare: Option<&Path>,
    changed_files: Option<&Path>,
    git_changed: Option<&str>,
) -> anyhow::Result<()> {
    if compare.is_none() && changed_files.is_none() && git_changed.is_none() {
        return Ok(());
    }
    let previous = compare
        .map(|path| -> anyhow::Result<PreviousReport> {
            let file = std::fs::File::open(path)
                .with_context(|| format!("cannot read comparison report {}", path.display()))?;
            serde_json::from_reader(file)
                .with_context(|| format!("invalid comparison report {}", path.display()))
        })
        .transpose()?;
    let root = Path::new(&report.root);
    let base = if root.is_file() {
        root.parent().unwrap_or(root)
    } else {
        root
    };
    let paths = if let Some(path) = changed_files {
        let text = if path == Path::new("-") {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .context("cannot read changed files from stdin")?;
            text
        } else {
            std::fs::read_to_string(path)
                .with_context(|| format!("cannot read changed files {}", path.display()))?
        };
        Some(
            text.trim_start_matches('\u{feff}')
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        )
    } else if let Some(reference) = git_changed {
        let mut paths = git_paths(
            base,
            &[
                "diff",
                "--name-only",
                "--relative",
                "-z",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                "--end-of-options",
                reference,
                "--",
            ],
        )?;
        paths.extend(git_paths(
            base,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?);
        Some(paths)
    } else {
        None
    };
    let paths = paths
        .map(|paths| normalize_paths(base, &paths))
        .transpose()?;
    apply_changes(report, previous.as_ref(), paths.as_ref());
    Ok(())
}

fn git_paths(base: &Path, arguments: &[&str]) -> anyhow::Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(base)
        .args(["-c", "core.fsmonitor=false"])
        .args(arguments)
        .output()
        .context("Git is required for --git-changed; could not run git")?;
    if !output.status.success() {
        anyhow::bail!(
            "--git-changed: git {} failed: {}",
            arguments[0],
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8(path.to_vec()).context("Git returned a non-UTF-8 file path"))
        .collect()
}

fn normalize_paths(base: &Path, paths: &[String]) -> anyhow::Result<BTreeSet<String>> {
    paths
        .iter()
        .map(|path| {
            let normalized = path.replace('\\', "/");
            let path = Path::new(&normalized);
            let absolute = lexical(&base.join(path));
            let relative = absolute.strip_prefix(base).with_context(|| {
                format!(
                    "changed file {} is outside scan root {}",
                    path.display(),
                    base.display()
                )
            })?;
            Ok(relative.to_string_lossy().replace('\\', "/"))
        })
        .collect()
}

// Do not canonicalize changed paths: deleted files still belong in the list.
fn lexical(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}
