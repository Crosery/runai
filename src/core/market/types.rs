use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSkill {
    pub name: String,
    pub repo_path: String, // e.g. "skills/brainstorming"
    pub source_label: String,
    pub source_repo: String, // "owner/repo"
    pub branch: String,
    /// All-time install count harvested from skills.sh leaderboard SSR.
    /// `0` for entries known only through the sitemap (no popularity
    /// signal). Used to power the "All Time" ordering and INSTALLS column.
    #[serde(default)]
    pub installs: u64,
    /// 24-hour install delta (Trending tab). `0` when unknown.
    #[serde(default)]
    pub trending_installs: u64,
    /// Hot score from skills.sh `/hot` ranking. `0` when unknown.
    /// Concretely populated as "installs from /hot SSR" — skills.sh's
    /// internal score isn't exposed but the ordered list is.
    #[serde(default)]
    pub hot_score: u64,
    /// 8-week install trend from skills.sh `weeklyInstalls`. Empty when
    /// unknown. Used by the SPARKLINE column in the leaderboard UI.
    #[serde(default)]
    pub weekly_installs: Vec<u64>,
    /// Marked official by skills.sh.
    #[serde(default)]
    pub is_official: bool,
    #[serde(skip)]
    pub installed: bool,
}

pub struct Market;
