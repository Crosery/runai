/// Base URL for raw GitHub mirror. jsdelivr CDN by default (measured
/// ~1s/file in mainland China vs raw.githubusercontent.com's 7s+).
/// Override with `RUNAI_GH_MIRROR` env to point at a different host —
/// must serve `/gh/<owner>/<repo>@<branch>/<path>` like jsdelivr does.
/// Set `RUNAI_GH_MIRROR=raw` to fall back to raw.githubusercontent.com
/// (useful when behind GFW-free networks where jsdelivr's fastly route
/// is actually slower).
fn mirror_base() -> String {
    let v = std::env::var("RUNAI_GH_MIRROR").unwrap_or_default();
    let v = v.trim().trim_end_matches('/');
    if v.is_empty() || v == "default" {
        return "https://cdn.jsdelivr.net".into();
    }
    if v == "raw" || v == "github" {
        // Sentinel for "go direct to raw.githubusercontent.com" — handled
        // by callers via the special prefix check.
        return "https://raw.githubusercontent.com".into();
    }
    v.to_string()
}

/// Build a raw-file URL for `owner/repo@branch/path` honoring the
/// configured mirror. Encapsulates the path-shape difference between
/// jsdelivr ("/gh/o/r@b/p") and raw.github ("/o/r/b/p").
pub(crate) fn raw_url_for(owner: &str, repo: &str, branch: &str, path: &str) -> String {
    let base = mirror_base();
    if base == "https://raw.githubusercontent.com" {
        format!("{base}/{owner}/{repo}/{branch}/{path}")
    } else {
        format!("{base}/gh/{owner}/{repo}@{branch}/{path}")
    }
}
