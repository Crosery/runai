# Skill Manager

[English](README.md) | **中文**

终端界面的 AI CLI skill/MCP 资源管理器。支持 **Claude Code**、**Codex**、**Gemini CLI** 和 **OpenCode**。

## 功能特性

- **TUI 终端界面** — 浏览、启用/禁用、搜索 skills 和 MCPs
- **多 CLI 支持** — 跨 4 个 AI CLI 统一管理，`1234` 切换目标
- **分组管理** — 将 skills/MCPs 组织成组，批量启用/禁用
- **Skill 市场** — 浏览 2000+ 来自 5 个内置源的 skills，支持自定义 GitHub 源
- **MCP 服务器** — 17 个工具通过 MCP 协议暴露，首次启动自动注册到所有 CLI
- **命令行** — 15 个子命令，支持脚本自动化

## 安装

```bash
git clone https://github.com/Crosery/skill-manager.git
cd skill-manager
cargo build --release
```

可选：添加到 PATH：
```bash
cp target/release/skill-manager ~/.local/bin/
```

## 快速开始

```bash
# 启动 TUI（首次运行会自动扫描并注册 MCP）
skill-manager

# 或直接使用 CLI
skill-manager list                    # 列出所有 skills
skill-manager status                  # 查看启用数量
skill-manager enable brainstorming    # 启用某个 skill
skill-manager scan                    # 扫描新 skills
```

## TUI 快捷键

| 按键 | 操作 |
|------|------|
| `H/L` 或 `Tab` | 切换标签页（Skills / MCPs / Groups / Market） |
| `j/k` | 上下导航 |
| `Space` | 启用/禁用 |
| `1234` | 切换 CLI 目标（Claude/Codex/Gemini/OpenCode） |
| `/` | 搜索 |
| `Enter` | 打开分组详情 / 从市场安装 |
| `d` | 删除选中项 |
| `c` | 创建新分组 |
| `s` | 源管理（Market 标签页）/ 扫描（其他标签页） |
| `[ ]` | 切换市场源 |
| `q` | 退出 |

## MCP 工具

作为 MCP 服务器运行时（`skill-manager mcp-serve`），提供 17 个工具：

| 工具 | 说明 |
|------|------|
| `sm_list` | 列出 skills/MCPs（支持过滤） |
| `sm_groups` | 列出所有分组 |
| `sm_status` | 各 CLI 的启用/总数统计 |
| `sm_enable` / `sm_disable` | 启用/禁用 skill/MCP |
| `sm_scan` | 扫描目录发现新 skills |
| `sm_delete` | 删除 skill/MCP |
| `sm_create_group` / `sm_delete_group` | 创建/删除分组 |
| `sm_group_add` / `sm_group_remove` | 管理分组成员 |
| `sm_group_enable` / `sm_group_disable` | 批量启用/禁用分组 |
| `sm_market` | 浏览市场 skills |
| `sm_market_install` | 从市场安装单个 skill |
| `sm_sources` | 管理市场源 |
| `sm_register` | 注册 MCP 到所有 CLI |

MCP 服务器会在首次启动时自动注册到 `~/.claude.json`、`~/.codex/settings.json`、`~/.gemini/settings.json` 和 `~/.opencode/settings.json`。

## 市场源

内置源（在 Market 标签页按 `s` 管理）：

| 源 | Skills 数量 | 默认状态 |
|----|------------|----------|
| Anthropic Official | 23 | 启用 |
| Everything Claude Code | 125 | 启用 |
| Terminal Skills | 900+ | 禁用 |
| Antigravity Skills | 1300+ | 禁用 |
| OK Skills | 55 | 禁用 |

按 `a` 添加自定义源（格式：`owner/repo` 或 `owner/repo@branch`）。

## 数据存储

所有数据存储在 `~/.skill-manager/`：
- `skills/` — 托管的 skill 目录（每个包含 SKILL.md）
- `groups/` — 分组定义（TOML 文件）
- `market-cache/` — 市场 skill 列表缓存（JSON，自动刷新）
- `market-sources.json` — 自定义市场源
- `skill-manager.db` — SQLite 数据库（资源、目标状态、分组成员）

## 许可证

MIT
