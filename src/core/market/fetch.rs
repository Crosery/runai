use anyhow::{Result, bail};

use super::extract::{ExtractResult, GitTree};
use super::sources::SourceEntry;
use super::types::Market;

impl Market {
    /// Fetch skill list from GitHub API, or from the skills.sh sitemap
    /// when the source carries the [`SKILLSHUB_SENTINEL`] owner.
    ///
    /// [`SKILLSHUB_SENTINEL`]: super::SKILLSHUB_SENTINEL
    pub(crate) async fn fetch(source: &SourceEntry) -> Result<ExtractResult> {
        if source.is_skillshub() {
            let skills = Self::fetch_skillshub().await?;
            // The aggregator has no underlying git tree; subsequent
            // install paths re-fetch the real repo's tree on demand.
            return Ok(ExtractResult {
                skills,
                plugin_detected: false,
                tree: GitTree { tree: Vec::new() },
            });
        }
        let url = format!(
            "{}/repos/{}/{}/git/trees/{}?recursive=1",
            super::github_mirror::github_api_base(),
            source.owner,
            source.repo,
            source.branch,
        );

        let client = reqwest::Client::builder().user_agent("runai/0.5").build()?;

        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!(
                "GitHub API {} for {}/{}",
                resp.status(),
                source.owner,
                source.repo
            );
        }

        let body: GitTree = resp.json().await?;
        Ok(Self::extract_skills(body, source))
    }
}
