pub mod dm;
pub mod dm_history;
pub mod history;
pub mod presence;
pub mod react;
pub mod send;
pub mod send_file;
pub mod send_thread;
pub mod threads;

use std::path::{Path, PathBuf};

pub struct SlackToolConfig {
    pub script_path: PathBuf,
    pub workspace_dir: PathBuf,
}

impl SlackToolConfig {
    pub fn new(script: &str, workspace_dir: &Path) -> Self {
        Self {
            script_path: workspace_dir.join(script),
            workspace_dir: workspace_dir.to_path_buf(),
        }
    }

    /// Run the Slack CLI script with the given args. Returns stdout on success.
    pub async fn run(&self, args: &[&str]) -> anyhow::Result<String> {
        let output = tokio::process::Command::new("npx")
            .args(["tsx", &self.script_path.to_string_lossy()])
            .args(args)
            .current_dir(&self.workspace_dir)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("slack-cli failed: {stderr}");
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_tool_config_resolves_script_path() {
        let cfg =
            SlackToolConfig::new("skills/slack/scripts/slack-cli.ts", Path::new("/workspace"));
        assert_eq!(
            cfg.script_path,
            PathBuf::from("/workspace/skills/slack/scripts/slack-cli.ts")
        );
        assert_eq!(cfg.workspace_dir, PathBuf::from("/workspace"));
    }
}
