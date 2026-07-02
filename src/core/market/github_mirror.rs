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
///
/// **Test-only override** (`RUNAI_GITHUB_RAW_BASE`): when set to a non-empty
/// base URL, raw downloads are routed there with the raw.githubusercontent
/// path shape (`<base>/<owner>/<repo>/<branch>/<path>`). Production never
/// sets this — it exists so `tests/install_fixture_e2e.rs` can point the
/// install pipeline at a local fixture server and exercise the落盘 path
/// offline. It takes precedence over `RUNAI_GH_MIRROR`.
pub(crate) fn raw_url_for(owner: &str, repo: &str, branch: &str, path: &str) -> String {
    if let Ok(v) = std::env::var("RUNAI_GITHUB_RAW_BASE") {
        let base = v.trim().trim_end_matches('/');
        if !base.is_empty() {
            return format!("{base}/{owner}/{repo}/{branch}/{path}");
        }
    }
    let base = mirror_base();
    if base == "https://raw.githubusercontent.com" {
        format!("{base}/{owner}/{repo}/{branch}/{path}")
    } else {
        format!("{base}/gh/{owner}/{repo}@{branch}/{path}")
    }
}

/// Base URL for the GitHub REST API (git-trees + Contents endpoints).
/// Defaults to the real `https://api.github.com`.
///
/// **Test-only override** (`RUNAI_GITHUB_API_BASE`): when set to a non-empty
/// base URL, the tree API (`fetch.rs`) and Contents API (`install.rs`) point
/// there instead. Production never sets it; `tests/install_fixture_e2e.rs`
/// uses it to serve a minimal GitHub API from a local fixture server so the
/// install pipeline runs without network.
pub(crate) fn github_api_base() -> String {
    if let Ok(v) = std::env::var("RUNAI_GITHUB_API_BASE") {
        let base = v.trim().trim_end_matches('/');
        if !base.is_empty() {
            return base.to_string();
        }
    }
    "https://api.github.com".to_string()
}
