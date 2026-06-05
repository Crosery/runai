//! `runai community ...` CLI dispatch.
//!
//! Thin client over the server's `/api/community/*` endpoints. Reads
//! `~/.runai-identity` for the Bearer key (compatibility with the same
//! identity file installed by `scripts/runai-client-install.sh`). When the
//! file is missing the caller must pass `--server` and the server must be
//! in owner mode or the request will 401 — that's intentional, community is
//! a team-mode feature.

use anyhow::{Context, Result, bail};
use std::path::Path;

use crate::cli::command_enums::CommunityCommands;

/// Default if neither `--server` nor `RUNAI_SERVER` is given. Matches the
/// LaunchAgent / autostart default port for the local runai server.
const DEFAULT_SERVER: &str = "http://127.0.0.1:17888";

pub(crate) fn handle_community(cmd: CommunityCommands) -> Result<()> {
    match cmd {
        CommunityCommands::Upload { path, name, server } => {
            upload(&path, name.as_deref(), server.as_deref())
        }
        CommunityCommands::List { sort, server } => list(&sort, server.as_deref()),
        CommunityCommands::Install {
            uploader,
            name,
            server,
        } => install(&uploader, &name, server.as_deref()),
        CommunityCommands::Delete {
            uploader,
            name,
            server,
        } => delete(&uploader, &name, server.as_deref()),
    }
}

/// Resolve server base URL: --server > RUNAI_SERVER env > DEFAULT.
fn resolve_server(flag: Option<&str>) -> String {
    if let Some(s) = flag {
        return s.trim_end_matches('/').to_string();
    }
    if let Ok(s) = std::env::var("RUNAI_SERVER") {
        if !s.is_empty() {
            return s.trim_end_matches('/').to_string();
        }
    }
    DEFAULT_SERVER.to_string()
}

/// Read api_key from ~/.runai-identity (created by client-install.sh).
fn read_identity_key() -> Result<String> {
    let home = dirs::home_dir().context("cannot resolve $HOME")?;
    let path = home.join(".runai-identity");
    let contents = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "no identity file at {} — run `curl <server>/install | bash` first or set RUNAI_API_KEY env",
            path.display()
        )
    })?;
    // identity file is JSON: {"api_key":"rnai_live_...","server":"..."}
    let v: serde_json::Value = serde_json::from_str(contents.trim())
        .with_context(|| format!("identity file at {} is not valid JSON", path.display()))?;
    v.get("api_key")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .context("identity file missing api_key field")
}

fn resolve_key() -> Result<String> {
    if let Ok(k) = std::env::var("RUNAI_API_KEY") {
        if !k.is_empty() {
            return Ok(k);
        }
    }
    read_identity_key()
}

fn upload(skill_dir: &Path, name_override: Option<&str>, server_flag: Option<&str>) -> Result<()> {
    let server = resolve_server(server_flag);
    let key = resolve_key()?;

    if !skill_dir.is_dir() {
        bail!("not a directory: {}", skill_dir.display());
    }
    let skill_md = skill_dir.join("SKILL.md");
    if !skill_md.is_file() {
        bail!(
            "no SKILL.md at {} — community upload requires a valid skill directory",
            skill_md.display()
        );
    }
    let name = match name_override {
        Some(n) => n.to_string(),
        None => skill_dir
            .file_name()
            .and_then(|s| s.to_str())
            .context("cannot derive skill name from directory path")?
            .to_string(),
    };

    // Tar + gzip the directory in memory. Mirrors what
    // /skills/bundle/{name} does on the server side (tar + flate2).
    let mut buf = Vec::new();
    {
        let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        tar.append_dir_all(&name, skill_dir)
            .with_context(|| format!("tar-pack skill dir {}", skill_dir.display()))?;
        tar.finish().context("tar finish")?;
    }

    let url = format!("{server}/api/community/upload");
    let client = reqwest::blocking::Client::new();
    let form = reqwest::blocking::multipart::Form::new()
        .text("name", name.clone())
        .part(
            "bundle",
            reqwest::blocking::multipart::Part::bytes(buf)
                .file_name(format!("{name}.tar.gz"))
                .mime_str("application/gzip")
                .context("set mime")?,
        );
    let resp = client
        .post(&url)
        .bearer_auth(&key)
        .multipart(form)
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !status.is_success() {
        bail!("upload failed: HTTP {status} — {body}");
    }
    println!("uploaded {name} to {server}");
    Ok(())
}

fn list(sort: &str, server_flag: Option<&str>) -> Result<()> {
    let server = resolve_server(server_flag);
    let key = resolve_key()?;
    let url = format!("{server}/api/community/list?sort={sort}&limit=200");
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(&url)
        .bearer_auth(&key)
        .send()
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().context("decode list response")?;
    if !status.is_success() {
        bail!(
            "list failed: HTTP {status} — {}",
            v.get("error")
                .and_then(|x| x.as_str())
                .unwrap_or("(no body)")
        );
    }
    let items = v.get("items").and_then(|x| x.as_array());
    let Some(items) = items else {
        return Ok(());
    };
    for it in items {
        let uid = it
            .get("uploader_uid")
            .and_then(|x| x.as_str())
            .unwrap_or("?");
        let name = it.get("name").and_then(|x| x.as_str()).unwrap_or("?");
        let ver = it.get("version").and_then(|x| x.as_str()).unwrap_or("-");
        let installs = it
            .get("installs_total")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        println!("{uid}\t{name}\t{ver}\t{installs}");
    }
    Ok(())
}

fn install(uploader: &str, name: &str, server_flag: Option<&str>) -> Result<()> {
    let server = resolve_server(server_flag);
    let key = resolve_key()?;
    let url = format!(
        "{server}/api/community/install/{}/{}",
        percent_encoding::utf8_percent_encode(uploader, percent_encoding::NON_ALPHANUMERIC),
        percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC)
    );
    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(&url)
        .bearer_auth(&key)
        .send()
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !status.is_success() {
        bail!("install failed: HTTP {status} — {body}");
    }
    println!("installed {uploader}/{name} into private pool");
    Ok(())
}

fn delete(uploader: &str, name: &str, server_flag: Option<&str>) -> Result<()> {
    let server = resolve_server(server_flag);
    let key = resolve_key()?;
    let url = format!(
        "{server}/api/community/skill/{}/{}",
        percent_encoding::utf8_percent_encode(uploader, percent_encoding::NON_ALPHANUMERIC),
        percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC)
    );
    let client = reqwest::blocking::Client::new();
    let resp = client
        .delete(&url)
        .bearer_auth(&key)
        .send()
        .with_context(|| format!("DELETE {url}"))?;
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if !status.is_success() {
        bail!("delete failed: HTTP {status} — {body}");
    }
    println!("deleted {uploader}/{name}");
    Ok(())
}
