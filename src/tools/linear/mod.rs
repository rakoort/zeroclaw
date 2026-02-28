pub mod add_comment;
pub mod archive_issue;
pub mod create_cycle;
pub mod create_issue;
pub mod create_label;
pub mod create_project;
pub mod cycles;
pub mod issues;
pub mod labels;
pub mod projects;
pub mod states;
pub mod teams;
pub mod update_issue;
pub mod users;

use std::path::{Path, PathBuf};

pub struct LinearToolConfig {
    pub script_path: PathBuf,
    pub workspace_dir: PathBuf,
}

impl LinearToolConfig {
    pub fn new(script: &str, workspace_dir: &Path) -> Self {
        Self {
            script_path: workspace_dir.join(script),
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }

    /// Run the Linear CLI script with the given args. Returns stdout on success.
    pub async fn run(&self, args: &[&str]) -> anyhow::Result<String> {
        let output = tokio::process::Command::new("npx")
            .args(["tsx", &self.script_path.to_string_lossy()])
            .args(args)
            .current_dir(&self.workspace_dir)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("linear-cli failed: {stderr}");
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_tool_config_resolves_script_path() {
        let cfg = LinearToolConfig::new(
            "skills/linear/scripts/linear-cli.ts",
            Path::new("/workspace"),
        );
        assert_eq!(
            cfg.script_path,
            PathBuf::from("/workspace/skills/linear/scripts/linear-cli.ts")
        );
        assert_eq!(cfg.workspace_dir, PathBuf::from("/workspace"));
    }
}
