#[derive(Clone, Copy, PartialEq)]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    pub fn toggle(self) -> Self {
        match self {
            Lang::Zh => Lang::En,
            Lang::En => Lang::Zh,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Lang::Zh => "中文",
            Lang::En => "EN",
        }
    }
}

pub struct T {
    pub lang: Lang,
}

impl T {
    pub fn new(lang: Lang) -> Self {
        Self { lang }
    }

    fn zh_en(&self, zh: &'static str, en: &'static str) -> &'static str {
        match self.lang {
            Lang::Zh => zh,
            Lang::En => en,
        }
    }

    // ── Tabs ──
    pub fn tab_skills(&self) -> &'static str {
        self.zh_en("技能", "Skills")
    }
    pub fn tab_mcps(&self) -> &'static str {
        self.zh_en("MCP", "MCPs")
    }
    pub fn tab_groups(&self) -> &'static str {
        self.zh_en("分组", "Groups")
    }
    pub fn tab_market(&self) -> &'static str {
        self.zh_en("市场", "Market")
    }

    // ── Filter modes ──
    pub fn filter_all(&self) -> &'static str {
        self.zh_en("全部", "All")
    }
    pub fn filter_enabled(&self) -> &'static str {
        self.zh_en("已启用", "Enabled")
    }
    pub fn filter_disabled(&self) -> &'static str {
        self.zh_en("未启用", "Disabled")
    }

    // ── Footer help ──
    pub fn help_normal_skills(&self) -> &'static str {
        self.zh_en(
            "j/k ↕  H/L 切换  SPACE 开关  f 过滤  / 搜索  t 主题  ? 帮助  q 退出",
            "j/k ↕  H/L tab  SPACE toggle  f filter  / search  t theme  ? help  q quit",
        )
    }
    pub fn help_normal_groups(&self) -> &'static str {
        self.zh_en(
            "j/k ↕  H/L 切换  SPACE 开关  / 搜索  t 主题  ? 帮助  q 退出",
            "j/k ↕  H/L tab  SPACE toggle  / search  t theme  ? help  q quit",
        )
    }
    pub fn help_normal_market(&self) -> &'static str {
        self.zh_en(
            "j/k ↕  H/L 切换  ENTER 安装  [ ] 切源  t 主题  ? 帮助  q 退出",
            "j/k ↕  H/L tab  ENTER install  [ ] source  t theme  ? help  q quit",
        )
    }
    pub fn help_search(&self) -> &'static str {
        self.zh_en("ESC 取消  ENTER 确认", "ESC cancel  ENTER confirm")
    }

    // ── Status bar ──
    pub fn status_skills(&self) -> &'static str {
        self.zh_en("技能  ", "skills  ")
    }
    pub fn status_mcp(&self) -> &'static str {
        self.zh_en("mcp ", "mcp ")
    }

    // ── List titles ──
    pub fn title_groups(&self) -> &'static str {
        self.zh_en("分组", "Groups")
    }
    pub fn title_market_loading(&self) -> &'static str {
        self.zh_en("加载中", "Loading")
    }
    pub fn title_market_no_source(&self) -> &'static str {
        self.zh_en(
            " 市场 — 无可用源 (按 's') ",
            " Market — No sources enabled (press 's') ",
        )
    }

    // ── Group detail ──
    pub fn group_empty(&self) -> &'static str {
        self.zh_en(
            "  (空) — 按 'a' 添加技能",
            "  (empty) — press 'a' to add skills",
        )
    }
    pub fn help_group_detail(&self) -> &'static str {
        self.zh_en(
            "j/k 导航  SPACE 开关  a 添加  d 移除  1234 CLI  ESC 关闭",
            "j/k navigate  SPACE toggle  a add  d remove  1234 CLI  ESC close",
        )
    }

    // ── Pick skill ──
    pub fn pick_filter_hint(&self) -> &'static str {
        self.zh_en("  输入过滤...", "  Type to filter...")
    }
    pub fn help_pick_skill(&self) -> &'static str {
        self.zh_en(
            " j/k 导航  ENTER 添加  TAB 技能/MCP  输入搜索  BS 清除  ESC 返回",
            " j/k navigate  ENTER add  TAB skill/mcp  type search  BS clear  ESC back",
        )
    }

    // ── Source manager ──
    pub fn title_sources(&self) -> &'static str {
        self.zh_en(" 市场源 ", " Market Sources ")
    }
    pub fn help_sources(&self) -> &'static str {
        self.zh_en(
            " j/k 导航  SPACE 开关  a 添加  d 删除  ESC 关闭",
            " j/k navigate  SPACE toggle  a add  d delete  ESC close",
        )
    }
    pub fn cant_delete_builtin(&self) -> &'static str {
        self.zh_en(
            "内置源不可删除（可禁用）",
            "Can't delete built-in source (disable it instead)",
        )
    }

    // ── Add source dialog ──
    pub fn title_add_source(&self) -> &'static str {
        self.zh_en(" 添加市场源 ", " Add Market Source ")
    }
    pub fn add_source_prompt(&self) -> &'static str {
        self.zh_en(
            "  输入 GitHub 仓库 (owner/repo 或完整 URL):",
            "  Enter GitHub repo (owner/repo or full URL):",
        )
    }
    pub fn add_source_example(&self) -> &'static str {
        self.zh_en(
            "  例: anthropics/claude-code  或  owner/repo@branch",
            "  e.g. anthropics/claude-code  or  owner/repo@branch",
        )
    }
    pub fn help_add_source(&self) -> &'static str {
        self.zh_en("  ESC 取消  ENTER 添加", "  ESC cancel  ENTER add")
    }

    // ── Install dialog ──
    pub fn title_install(&self) -> &'static str {
        self.zh_en(" 从 GitHub 安装 ", " Install from GitHub ")
    }
    pub fn install_prompt(&self) -> &'static str {
        self.zh_en(
            "  输入 GitHub 源 (owner/repo 或 owner/repo@branch):",
            "  Enter GitHub source (owner/repo or owner/repo@branch):",
        )
    }
    pub fn help_install(&self) -> &'static str {
        self.zh_en("  ESC 取消  ENTER 安装", "  ESC cancel  ENTER install")
    }

    // ── Create group dialog ──
    pub fn title_create_group(&self, step: u8) -> &'static str {
        if step == 0 {
            self.zh_en(" 创建分组 (1/2) ", " Create Group (1/2) ")
        } else {
            self.zh_en(" 创建分组 (2/2) ", " Create Group (2/2) ")
        }
    }
    pub fn create_group_prompt(&self, step: u8) -> &'static str {
        if step == 0 {
            self.zh_en("分组名称:", "Group Name:")
        } else {
            self.zh_en("描述 (Enter 跳过):", "Description (Enter to skip):")
        }
    }
    pub fn help_dialog(&self) -> &'static str {
        self.zh_en("  ESC 取消  ENTER 确认", "  ESC cancel  ENTER confirm")
    }

    // ── Rename group ──
    pub fn title_rename_group(&self) -> &'static str {
        self.zh_en(" 重命名分组 ", " Rename Group ")
    }
    pub fn rename_prompt(&self) -> &'static str {
        self.zh_en("  新名称:", "  New name:")
    }

    // ── Add to group picker ──
    pub fn title_add_to_group(&self) -> &'static str {
        self.zh_en(" 添加到分组 ", " Add to Group ")
    }
    pub fn help_group_picker(&self) -> &'static str {
        self.zh_en(
            " j/k 导航  ENTER 选择  ESC 取消",
            " j/k navigate  ENTER select  ESC cancel",
        )
    }

    // ── First launch ──
    pub fn title_welcome(&self) -> &'static str {
        self.zh_en(" 欢迎使用 Runai ", " Welcome to Runai ")
    }
    pub fn title_scanning(&self) -> &'static str {
        self.zh_en(" 扫描中... ", " Scanning... ")
    }
    pub fn title_scan_done(&self) -> &'static str {
        self.zh_en(" 扫描完成 ", " Scan Complete ")
    }
    pub fn welcome_detected(&self) -> &'static str {
        self.zh_en("  检测到首次启动！", "  First time setup detected!")
    }
    pub fn welcome_will(&self) -> &'static str {
        self.zh_en("  将会:", "  This will:")
    }
    pub fn welcome_scan_dirs(&self) -> &'static str {
        self.zh_en(
            "    • 扫描所有 CLI 目录中的技能",
            "    • Scan all CLI directories for skills",
        )
    }
    pub fn welcome_scan_dirs2(&self) -> &'static str {
        self.zh_en(
            "      (Claude, Codex, Gemini, OpenCode)",
            "      (Claude, Codex, Gemini, OpenCode)",
        )
    }
    pub fn welcome_discover_mcp(&self) -> &'static str {
        self.zh_en(
            "    • 从配置文件发现 MCP 服务器",
            "    • Discover MCP servers from config files",
        )
    }
    pub fn welcome_auto_group(&self) -> &'static str {
        self.zh_en("    • 提供智能自动分组", "    • Offer smart auto-grouping")
    }
    pub fn welcome_keys(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self.lang {
            Lang::Zh => ("  ENTER ", "开始扫描    ", "ESC ", "跳过"),
            Lang::En => ("  ENTER ", "start scan    ", "ESC ", "skip"),
        }
    }
    pub fn scanning_msg(&self) -> &'static str {
        self.zh_en(
            "  正在扫描所有技能目录和 MCP 配置...",
            "  Scanning all skill directories and MCP configs...",
        )
    }
    pub fn scanning_wait(&self) -> &'static str {
        self.zh_en("  请稍候...", "  Please wait...")
    }
    pub fn scan_skills_found(&self) -> &'static str {
        self.zh_en("  发现技能: ", "  Skills found: ")
    }
    pub fn scan_mcps_found(&self) -> &'static str {
        self.zh_en("  发现 MCP:  ", "  MCPs found:   ")
    }
    pub fn scan_continue(&self) -> &'static str {
        self.zh_en("  按任意键继续。", "  Press any key to continue.")
    }
    pub fn scan_in_progress(&self) -> &'static str {
        self.zh_en("  扫描中...", "  Scanning...")
    }

    // ── Help overlay ──
    pub fn title_keybindings(&self) -> &'static str {
        self.zh_en(" 快捷键 ", " Keybindings ")
    }
    pub fn help_section_nav(&self) -> &'static str {
        self.zh_en("  导航", "  Navigation")
    }
    pub fn help_g(&self) -> &'static str {
        self.zh_en("跳到顶部/底部", "Jump to top/bottom")
    }
    pub fn help_1234(&self) -> &'static str {
        self.zh_en(
            "切换 CLI 目标  1:Claude  2:Codex  3:Gemini  4:OpenCode",
            "Switch CLI  1:Claude  2:Codex  3:Gemini  4:OpenCode",
        )
    }
    pub fn help_f(&self) -> &'static str {
        self.zh_en(
            "切换显示过滤 (全部 / 已启用 / 未启用)",
            "Toggle filter (All / Enabled / Disabled)",
        )
    }
    pub fn help_section_skills(&self) -> &'static str {
        self.zh_en("  技能 & MCP", "  Skills & MCPs")
    }
    pub fn help_enter(&self) -> &'static str {
        self.zh_en(
            "打开分组详情 / 从市场安装",
            "Open group detail / Install from market",
        )
    }
    pub fn help_s(&self) -> &'static str {
        self.zh_en("扫描新技能", "Scan for new skills")
    }
    pub fn help_i(&self) -> &'static str {
        self.zh_en(
            "从 GitHub 安装 (owner/repo)",
            "Install from GitHub (owner/repo)",
        )
    }
    pub fn help_d(&self) -> &'static str {
        self.zh_en("删除选中的技能或 MCP", "Delete selected skill or MCP")
    }
    pub fn help_section_groups(&self) -> &'static str {
        self.zh_en("  分组", "  Groups")
    }
    pub fn help_c(&self) -> &'static str {
        self.zh_en("创建新分组", "Create new group")
    }
    pub fn help_r(&self) -> &'static str {
        self.zh_en("重命名分组 (分组标签)", "Rename group (Groups tab)")
    }
    pub fn help_a(&self) -> &'static str {
        self.zh_en("添加选中项到分组", "Add selected to a group")
    }
    pub fn help_section_market(&self) -> &'static str {
        self.zh_en("  市场", "  Market")
    }
    pub fn help_brackets(&self) -> &'static str {
        self.zh_en("切换市场源", "Switch market source")
    }
    pub fn help_s_market(&self) -> &'static str {
        self.zh_en("源管理器 (市场标签)", "Source manager (Market tab)")
    }
    pub fn help_close(&self) -> &'static str {
        self.zh_en("  按任意键关闭", "  Press any key to close")
    }
    pub fn help_l_lang(&self) -> &'static str {
        self.zh_en("切换语言 (中文/EN)", "Toggle language (中文/EN)")
    }

    // ── Messages ──
    pub fn msg_scan_done(&self) -> &'static str {
        self.zh_en("扫描完成", "Scan complete")
    }
    pub fn msg_filter(&self, label: &'static str) -> String {
        match self.lang {
            Lang::Zh => format!("显示: {}", label),
            Lang::En => format!("Filter: {}", label),
        }
    }
    pub fn msg_theme(&self, label: &str) -> String {
        match self.lang {
            Lang::Zh => format!("主题: {}", label),
            Lang::En => format!("Theme: {}", label),
        }
    }
    pub fn msg_lang_switched(&self) -> &'static str {
        match self.lang {
            Lang::Zh => "已切换为中文",
            Lang::En => "Switched to English",
        }
    }
}
