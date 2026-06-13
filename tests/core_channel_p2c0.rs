//! P2 regression coverage for `core::channel` (PLANNING test plan §5.15).
//!
//! `ChannelConfig` is a JSON-on-disk config of market channel sources. It's
//! a pure data structure module — no `~/.runai/` writes, no symlinks, no CLI
//! config mutation — so these tests use plain `tempfile::TempDir` rather
//! than the full `HOME` sandbox treatment the safety contract reserves for
//! filesystem-touching subcommands.

use runai::core::channel::ChannelConfig;
use std::path::Path;
use tempfile::TempDir;

/// 5.15.1 default_config_has_channels — default config initialises with the
/// two builtin channels. Any drift here means market-source bootstrap broke.
#[test]
fn default_config_has_channels() {
    let cfg = ChannelConfig::default_config();
    assert!(
        cfg.channels.len() >= 2,
        "default_config must seed at least 2 channels, got {}",
        cfg.channels.len()
    );

    let names: Vec<&str> = cfg.channels.iter().map(|c| c.name.as_str()).collect();
    assert!(
        names.iter().any(|n| n.contains("ECC Skills")),
        "default config missing ECC Skills, names={:?}",
        names
    );
    assert!(
        names.iter().any(|n| n.contains("Vercel Skills")),
        "default config missing Vercel Skills, names={:?}",
        names
    );

    // Every channel has populated name / url / description (no empty strings)
    for ch in &cfg.channels {
        assert!(!ch.name.is_empty(), "channel name must not be empty");
        assert!(!ch.url.is_empty(), "channel url must not be empty");
        assert!(
            !ch.description.is_empty(),
            "channel description must not be empty for {}",
            ch.name
        );
        // URLs must be valid http(s)
        assert!(
            ch.url.starts_with("http://") || ch.url.starts_with("https://"),
            "channel url not http(s): {}",
            ch.url
        );
    }
}

/// 5.15.2 save_and_load_roundtrip — JSON persistence must round-trip
/// (channel count, names, urls, ordering) and add_channel must dedupe on URL.
#[test]
fn save_and_load_roundtrip() {
    let tmp = TempDir::new().expect("create tempdir");
    let path = tmp.path().join("channels.json");

    let mut cfg = ChannelConfig::default_config();
    let original_len = cfg.channels.len();
    let original_names: Vec<String> = cfg.channels.iter().map(|c| c.name.clone()).collect();
    let original_urls: Vec<String> = cfg.channels.iter().map(|c| c.url.clone()).collect();

    // Append a fresh channel — must take effect (unique URL).
    cfg.add_channel(
        "Custom Test".into(),
        "https://example.com/custom-channel".into(),
        "Custom description".into(),
    );
    assert_eq!(cfg.channels.len(), original_len + 1);

    // Re-adding the same URL with a different name must be a no-op (dedupe).
    cfg.add_channel(
        "Different Name".into(),
        "https://example.com/custom-channel".into(),
        "Another description".into(),
    );
    assert_eq!(
        cfg.channels.len(),
        original_len + 1,
        "add_channel must dedupe on URL"
    );

    cfg.save(&path).expect("save should succeed");
    assert!(path.exists(), "save must create file at path");

    // Loaded file must produce valid JSON readable as a config.
    let raw = std::fs::read_to_string(&path).expect("read saved file");
    assert!(raw.starts_with('{'), "saved file must be JSON object");
    let _: serde_json::Value = serde_json::from_str(&raw).expect("saved file must be valid JSON");

    let loaded = ChannelConfig::load(&path).expect("load should succeed");
    assert_eq!(
        loaded.channels.len(),
        cfg.channels.len(),
        "roundtrip preserves channel count"
    );

    // Default-channel order preserved.
    for (i, original_name) in original_names.iter().enumerate() {
        assert_eq!(
            &loaded.channels[i].name, original_name,
            "channel ordering not preserved at idx {}",
            i
        );
        assert_eq!(
            &loaded.channels[i].url, &original_urls[i],
            "channel url not preserved at idx {}",
            i
        );
    }

    // Appended channel is at the tail.
    let last = loaded.channels.last().expect("at least one channel");
    assert_eq!(last.name, "Custom Test");
    assert_eq!(last.url, "https://example.com/custom-channel");
    assert_eq!(last.description, "Custom description");
}

/// 5.15.3 load_missing_file_returns_default — first-launch / fresh-install
/// must degrade to default config rather than erroring.
#[test]
fn load_missing_file_returns_default() {
    let tmp = TempDir::new().expect("create tempdir");
    let missing = tmp.path().join("does_not_exist.json");
    assert!(!missing.exists(), "precondition: path must not exist");

    let cfg = ChannelConfig::load(&missing).expect("load on missing path must not error");

    // Must equal default_config (same count + names).
    let default = ChannelConfig::default_config();
    assert_eq!(
        cfg.channels.len(),
        default.channels.len(),
        "missing-file load must equal default"
    );
    for (loaded, expected) in cfg.channels.iter().zip(default.channels.iter()) {
        assert_eq!(loaded.name, expected.name);
        assert_eq!(loaded.url, expected.url);
    }

    // Also: load on a non-existent path of a non-existent parent dir is fine
    // (no file creation side effect).
    let nested = tmp.path().join("nested/missing/path/channels.json");
    let cfg2 = ChannelConfig::load(&nested).expect("nested missing must also degrade");
    assert!(!cfg2.channels.is_empty(), "default must be non-empty");
    assert!(
        !nested.exists(),
        "load(missing) must not create the file as a side effect"
    );

    // Sanity: tmp-dir absolute fixed path also produces default.
    let cfg3 = ChannelConfig::load(Path::new("/tmp/runai_p2c0_definitely_missing_xyz.json"))
        .expect("absolute missing path degrades to default");
    assert!(!cfg3.channels.is_empty());
}

/// 5.15.4 remove_channel_shifts_indices — removing index N shrinks len by 1
/// and shifts later entries forward; out-of-range index is a no-op.
#[test]
fn remove_channel_shifts_indices() {
    let mut cfg = ChannelConfig::default_config();
    let original_len = cfg.channels.len();
    assert!(
        original_len >= 2,
        "test requires default config of >=2 channels"
    );

    // Capture entries 1.. (these should slide to 0.. after removing idx 0).
    let expected_after: Vec<(String, String)> = cfg.channels[1..]
        .iter()
        .map(|c| (c.name.clone(), c.url.clone()))
        .collect();

    cfg.remove_channel(0);
    assert_eq!(
        cfg.channels.len(),
        original_len - 1,
        "remove_channel must decrement len by 1"
    );
    for (i, (expected_name, expected_url)) in expected_after.iter().enumerate() {
        assert_eq!(
            &cfg.channels[i].name, expected_name,
            "channel at idx {} did not shift forward",
            i
        );
        assert_eq!(
            &cfg.channels[i].url, expected_url,
            "channel url at idx {} did not shift forward",
            i
        );
    }

    // Out-of-range index is a no-op (must not panic, must not change len).
    let before_oob = cfg.channels.len();
    cfg.remove_channel(9999);
    assert_eq!(
        cfg.channels.len(),
        before_oob,
        "out-of-range remove must be a no-op"
    );

    // Remove every remaining channel one at a time — must reach 0 without panic.
    while !cfg.channels.is_empty() {
        let pre = cfg.channels.len();
        cfg.remove_channel(0);
        assert_eq!(cfg.channels.len(), pre - 1);
    }
    assert!(cfg.channels.is_empty());

    // Removing from empty is a no-op.
    cfg.remove_channel(0);
    assert!(cfg.channels.is_empty());
}
