/// One row from a skills.sh leaderboard SSR HTML page. The same shape
/// shows up on `/`, `/trending`, `/hot`; the meaning of `installs`
/// depends on which page it came from (all-time, 24h delta, hot score).
#[derive(Debug, Clone)]
pub(crate) struct LeaderboardRow {
    pub source_repo: String,
    pub skill_id: String,
    pub installs: u64,
    pub weekly_installs: Vec<u64>,
    pub is_official: bool,
}

/// Parse the streamed Next.js SSR payload embedded in a skills.sh page.
/// The payload is escaped JSON inside `self.__next_f.push(...)` calls;
/// each leaderboard row reads literally as
/// `\"source\":\"<owner/repo>\",\"skillId\":\"<slug>\",\"name\":\"...\",\"installs\":<n>[,\"weeklyInstalls\":[...]][,\"isOfficial\":true]`.
/// Stdlib-only — works on `/`, `/trending`, `/hot` identically.
pub(crate) fn parse_leaderboard(body: &str) -> Vec<LeaderboardRow> {
    let mut out = Vec::new();
    // Find each `\"source\":\"` anchor (literal backslash + quote in source).
    let needle = "\\\"source\\\":\\\"";
    let mut search_from = 0;
    while let Some(rel) = body[search_from..].find(needle) {
        let start = search_from + rel + needle.len();
        search_from = start;
        // Capture source until the closing `\"`.
        let Some(end_source) = body[start..].find("\\\"") else {
            break;
        };
        let source_repo = body[start..start + end_source].to_string();
        if source_repo.is_empty() || !source_repo.contains('/') {
            continue;
        }
        let after_source = start + end_source + needle.len() - "source\\\":\\\"".len();
        // Look ahead within a bounded window for skillId + installs (+ optional fields).
        // Clamp window_end down to the nearest UTF-8 char boundary — skills.sh HTML
        // contains em-dash and other multi-byte chars; slicing inside one panics.
        let mut window_end = (after_source + 800).min(body.len());
        while window_end > after_source && !body.is_char_boundary(window_end) {
            window_end -= 1;
        }
        let window = &body[after_source..window_end];

        let Some(skill_id) = extract_quoted_field(window, "skillId") else {
            continue;
        };
        let Some(installs_str) = extract_field(window, "installs") else {
            continue;
        };
        let Ok(installs) = installs_str.parse::<u64>() else {
            continue;
        };
        let weekly_installs = extract_array_field(window, "weeklyInstalls").unwrap_or_default();
        let is_official = window.contains("\\\"isOfficial\\\":true");

        out.push(LeaderboardRow {
            source_repo,
            skill_id,
            installs,
            weekly_installs,
            is_official,
        });
    }
    out
}

fn extract_quoted_field(window: &str, name: &str) -> Option<String> {
    let needle = format!("\\\"{name}\\\":\\\"");
    let start = window.find(&needle)? + needle.len();
    let end = window[start..].find("\\\"")?;
    Some(window[start..start + end].to_string())
}

fn extract_field(window: &str, name: &str) -> Option<String> {
    let needle = format!("\\\"{name}\\\":");
    let start = window.find(&needle)? + needle.len();
    let mut end = start;
    let bytes = window.as_bytes();
    while end < bytes.len() {
        let c = bytes[end];
        if !c.is_ascii_digit() && c != b'-' {
            break;
        }
        end += 1;
    }
    if end == start {
        return None;
    }
    Some(window[start..end].to_string())
}

fn extract_array_field(window: &str, name: &str) -> Option<Vec<u64>> {
    let needle = format!("\\\"{name}\\\":[");
    let start = window.find(&needle)? + needle.len();
    let end = window[start..].find(']')?;
    let inner = &window[start..start + end];
    let nums: Vec<u64> = inner
        .split(',')
        .filter_map(|n| n.trim().parse::<u64>().ok())
        .collect();
    if nums.is_empty() {
        return None;
    }
    Some(nums)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_leaderboard_extracts_installs_and_weekly() {
        // Skills.sh SSR embeds rows as escaped JSON inside `__next_f.push`.
        // The pattern is stable across `/`, `/trending`, `/hot`.
        let snippet = r#"
            ...random stuff...
            \"source\":\"vercel-labs/skills\",\"skillId\":\"find-skills\",\"name\":\"find-skills\",\"installs\":1765053,\"weeklyInstalls\":[100113,116613,115950,102724,94569,101582,116305,37369],\"isOfficial\":true},
            \"source\":\"anthropics/skills\",\"skillId\":\"frontend-design\",\"name\":\"frontend-design\",\"installs\":478598,\"weeklyInstalls\":[33429,31868,29995,26231,19072,26733,30547,9776]},
            \"source\":\"random/repo\",\"skillId\":\"no-weekly\",\"name\":\"no-weekly\",\"installs\":42},
        "#;
        let rows = parse_leaderboard(snippet);
        assert_eq!(rows.len(), 3, "must extract all three rows");

        assert_eq!(rows[0].skill_id, "find-skills");
        assert_eq!(rows[0].source_repo, "vercel-labs/skills");
        assert_eq!(rows[0].installs, 1_765_053);
        assert_eq!(rows[0].weekly_installs.len(), 8);
        assert_eq!(rows[0].weekly_installs[0], 100_113);
        assert!(rows[0].is_official);

        assert_eq!(rows[1].skill_id, "frontend-design");
        assert_eq!(rows[1].installs, 478_598);
        assert!(!rows[1].is_official);

        // No weeklyInstalls / isOfficial in the third row — defaults must apply.
        assert_eq!(rows[2].skill_id, "no-weekly");
        assert_eq!(rows[2].installs, 42);
        assert!(rows[2].weekly_installs.is_empty());
        assert!(!rows[2].is_official);
    }

    #[test]
    fn parse_leaderboard_handles_empty_and_garbage() {
        assert!(parse_leaderboard("").is_empty());
        assert!(parse_leaderboard("no JSON at all").is_empty());
        // Source without slash is rejected.
        let bad = r#"\"source\":\"justname\",\"skillId\":\"x\",\"installs\":1"#;
        assert!(parse_leaderboard(bad).is_empty());
    }
}
