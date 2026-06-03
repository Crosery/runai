use super::model::{App, FirstLaunchInfo};
use crate::core::cli_target::CliTarget;

impl App {
    pub fn do_first_launch_scan(&mut self) {
        self.scan_log.clear();

        self.scan_log.push("Scanning skill directories...".into());
        for t in CliTarget::ALL {
            for dir in &[t.skills_dir(), t.agents_skills_dir()] {
                if dir.exists() {
                    self.scan_log
                        .push(format!("  ✓ {} — {}", t.name(), dir.display()));
                }
            }
        }

        let scan_result = self.mgr.scan().unwrap_or_default();
        self.scan_log.push(format!(
            "  Found {} skills ({} new, {} existing)",
            scan_result.adopted + scan_result.skipped,
            scan_result.adopted,
            scan_result.skipped,
        ));
        if !scan_result.errors.is_empty() {
            self.scan_log.push(format!(
                "  ⚠ {} errors (see ~/.runai/scan.log)",
                scan_result.errors.len()
            ));
            let log_path = self.mgr.paths().data_dir().join("scan.log");
            let log_content = format!(
                "=== Scan Log {} ===\n\n{}\n",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
                scan_result.errors.join("\n"),
            );
            let _ = std::fs::write(&log_path, log_content);
        }

        self.scan_log.push("Discovering MCP servers...".into());
        let home = dirs::home_dir().unwrap_or_default();
        let mcp_entries = crate::core::mcp_discovery::McpDiscovery::discover_all(&home);
        self.scan_log
            .push(format!("  Found {} MCP servers", mcp_entries.len()));
        for entry in &mcp_entries {
            let status = if entry.disabled {
                "disabled"
            } else {
                "enabled"
            };
            self.scan_log
                .push(format!("    · {} ({})", entry.name, status));
        }

        self.scan_log
            .push("Registering MCP server to all CLIs...".into());
        let reg_result = crate::core::mcp_register::McpRegister::register_all(&home);
        for name in &reg_result.registered {
            self.scan_log.push(format!("  ✓ Registered to {name}"));
        }
        for name in &reg_result.skipped {
            self.scan_log
                .push(format!("  · {name} (already registered)"));
        }
        for err in &reg_result.errors {
            self.scan_log.push(format!("  ⚠ {err}"));
        }

        self.scan_log.push("Done!".into());

        self.first_launch_info = Some(FirstLaunchInfo {
            skills_found: scan_result.adopted + scan_result.skipped,
            mcps_found: mcp_entries.len(),
        });
    }
}
