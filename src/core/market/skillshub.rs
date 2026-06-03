use anyhow::Result;

use super::leaderboard::parse_leaderboard;
use super::sitemap::extract_sitemap_locs;
use super::types::{Market, MarketSkill};

impl Market {
    /// Build the unified skills.sh catalog.
    ///
    /// Pipeline:
    /// 1. Hit `/`, `/trending`, `/hot` — each SSRs ~600 leaderboard rows
    ///    with `source / skillId / installs / weeklyInstalls / isOfficial`.
    ///    Parse those into the popularity signals (`installs`,
    ///    `trending_installs`, `hot_score`, `weekly_installs`).
    /// 2. Hit `sitemap-skills-{1,2}.xml` — flatten 20K+ `owner/repo/skill`
    ///    URLs for skills the leaderboard didn't cover.
    /// 3. Merge by `(source_repo, name)` key. Leaderboard rows win
    ///    (they're enriched); sitemap entries fill the long tail with
    ///    zero popularity.
    pub(crate) async fn fetch_skillshub() -> Result<Vec<MarketSkill>> {
        let client = reqwest::Client::builder()
            .user_agent("runai/0.11")
            .build()?;

        let (all_html, trending_html, hot_html) = tokio::try_join!(
            async { client.get("https://www.skills.sh/").send().await?.error_for_status()?.text().await },
            async { client.get("https://www.skills.sh/trending").send().await?.error_for_status()?.text().await },
            async { client.get("https://www.skills.sh/hot").send().await?.error_for_status()?.text().await },
        )?;

        let mut by_key: std::collections::HashMap<String, MarketSkill> =
            std::collections::HashMap::new();
        let make_key =
            |source_repo: &str, name: &str| format!("{source_repo}//{name}");

        // /  → installs (All Time) + weeklyInstalls + isOfficial.
        for r in parse_leaderboard(&all_html) {
            let k = make_key(&r.source_repo, &r.skill_id);
            by_key
                .entry(k.clone())
                .or_insert_with(|| MarketSkill {
                    name: r.skill_id.clone(),
                    repo_path: String::new(),
                    source_label: "skills.sh".to_string(),
                    source_repo: r.source_repo.clone(),
                    branch: "main".to_string(),
                    installs: 0,
                    trending_installs: 0,
                    hot_score: 0,
                    weekly_installs: Vec::new(),
                    is_official: false,
                    installed: false,
                });
            if let Some(s) = by_key.get_mut(&k) {
                s.installs = r.installs;
                s.weekly_installs = r.weekly_installs;
                s.is_official = r.is_official;
            }
        }
        // /trending → trending_installs (24h delta).
        for r in parse_leaderboard(&trending_html) {
            let k = make_key(&r.source_repo, &r.skill_id);
            by_key
                .entry(k.clone())
                .or_insert_with(|| MarketSkill {
                    name: r.skill_id.clone(),
                    repo_path: String::new(),
                    source_label: "skills.sh".to_string(),
                    source_repo: r.source_repo.clone(),
                    branch: "main".to_string(),
                    installs: 0,
                    trending_installs: 0,
                    hot_score: 0,
                    weekly_installs: Vec::new(),
                    is_official: r.is_official,
                    installed: false,
                });
            if let Some(s) = by_key.get_mut(&k) {
                s.trending_installs = r.installs;
            }
        }
        // /hot → hot_score.
        for r in parse_leaderboard(&hot_html) {
            let k = make_key(&r.source_repo, &r.skill_id);
            by_key
                .entry(k.clone())
                .or_insert_with(|| MarketSkill {
                    name: r.skill_id.clone(),
                    repo_path: String::new(),
                    source_label: "skills.sh".to_string(),
                    source_repo: r.source_repo.clone(),
                    branch: "main".to_string(),
                    installs: 0,
                    trending_installs: 0,
                    hot_score: 0,
                    weekly_installs: Vec::new(),
                    is_official: r.is_official,
                    installed: false,
                });
            if let Some(s) = by_key.get_mut(&k) {
                s.hot_score = r.installs;
            }
        }

        // Sitemap fills the long tail. Skips entries the leaderboard
        // already covered to avoid double-counting (HashMap key check).
        const SHARDS: &[&str] = &[
            "https://www.skills.sh/sitemap-skills-1.xml",
            "https://www.skills.sh/sitemap-skills-2.xml",
        ];
        for shard in SHARDS {
            let body = client.get(*shard).send().await?.error_for_status()?.text().await?;
            for url in extract_sitemap_locs(&body) {
                let rest = match url
                    .strip_prefix("https://www.skills.sh/")
                    .or_else(|| url.strip_prefix("https://skills.sh/"))
                {
                    Some(r) => r,
                    None => continue,
                };
                let parts: Vec<&str> = rest.splitn(3, '/').collect();
                if parts.len() != 3 {
                    continue;
                }
                let (owner, repo, skill_name) = (parts[0], parts[1], parts[2]);
                if owner.is_empty() || repo.is_empty() || skill_name.is_empty() {
                    continue;
                }
                let source_repo = format!("{owner}/{repo}");
                let k = make_key(&source_repo, skill_name);
                by_key.entry(k).or_insert_with(|| MarketSkill {
                    name: skill_name.to_string(),
                    repo_path: String::new(),
                    source_label: "skills.sh".to_string(),
                    source_repo,
                    branch: "main".to_string(),
                    installs: 0,
                    trending_installs: 0,
                    hot_score: 0,
                    weekly_installs: Vec::new(),
                    is_official: false,
                    installed: false,
                });
            }
        }

        let mut skills: Vec<MarketSkill> = by_key.into_values().collect();
        // Default order: by all-time installs desc, then name asc. The
        // server-side `?sort=` query re-sorts the cached list per request.
        skills.sort_by(|a, b| {
            b.installs
                .cmp(&a.installs)
                .then(a.source_repo.cmp(&b.source_repo))
                .then(a.name.cmp(&b.name))
        });
        Ok(skills)
    }
}
