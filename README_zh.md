# Runai

[English](README.md) | **中文**

<p align="center">
  <img src="docs/images/runai-logo.png" alt="runai logo" width="180">
</p>

**runai** — 一把管理 AI CLI skills 和 MCP servers 的瑞士军刀。

skills 和 MCP servers 散落在 Claude Code / Codex / Gemini CLI / OpenCode 的各自配置里，格式不一、装多了就乱。runai 给你一个 TUI（外加可脚本化的 CLI 和一个 MCP server）一次搞定浏览、启用、搜索、安装、备份，跨 4 个 CLI、**macOS / Linux / Windows** 三平台。

- 一条命令从 GitHub 把任意 skill 装到所有 CLI
- 内置市场 2000+ skill 一键安装
- 文件系统即真源：symlink 在 = 启用，不在 = 禁用
- 跨 Claude JSON / Codex TOML / Gemini JSON / OpenCode JSON 原生格式同步
- macOS / Linux / Windows 原生二进制；Windows 上 symlink 需开启开发者模式

![TUI 分组视图](docs/images/tui-groups.png)

## 功能特性

- **TUI 终端界面** — 浏览、启用/禁用、搜索 skills 和 MCPs
- **多 CLI 支持** — 跨 4 个 AI CLI 统一管理，`1234` 切换目标
- **分组管理** — 将 skills/MCPs 组织成组，批量启用/禁用，重命名
- **一键安装** — `runai install owner/repo` 自动下载、注册、分组、启用
- **市场安装** — 浏览 2000+ skills，TUI Market 标签页 Enter 直接安装
- **Skill 发现** — 内置递归扫描器，秒级发现磁盘上所有 SKILL.md
- **统一搜索** — `sm_search` 同时搜索已安装资源和市场
- **使用追踪** — 记录 skill 使用次数和最后使用时间，识别未使用的 skill
- **MCP 服务器** — 30 个工具通过 MCP 协议暴露，首次启动自动注册到所有 CLI
- **批量操作** — 批量启用/禁用/删除/安装，一次调用完成
- **多 CLI 配置** — 原生支持：Claude JSON、Codex TOML、OpenCode 自定义 JSON、Gemini JSON
- **深色/亮色主题** — 按 `t` 切换，适配两种终端背景
- **文件系统为唯一数据源** — skill 启用 = 软链接存在；MCP 启用 = 配置条目存在
- **备份与恢复** — 带时间戳的完整备份，包括 skill 文件、MCP 配置和 CLI 配置
- **自动迁移** — 从 `skill-manager` 无缝升级到 `runai`（数据目录、数据库、软链接、MCP 条目）
- **命令行** — 子命令支持脚本自动化

## 安装

```bash
git clone https://github.com/Crosery/runai.git
cd runai
cargo install --path .
```

### Windows

预编译二进制：从 [releases](https://github.com/Crosery/runai/releases) 下载 `runai-windows-amd64.zip`，把 `runai.exe` 加入 PATH。也可以 `cargo install --path .` 从源码构建。

**软链接前置条件**：runai 用文件系统软链接作为"skill 启用"的真源。Windows 上创建软链接需要满足以下之一：

- 启用**开发者模式**（设置 → 隐私和安全 → 开发者 → 开发人员模式），或
- 以**管理员身份**运行 shell

否则 `enable` / `install` 在创建软链接时会失败。推荐开开发者模式（不用每次提权）。

四个 CLI 的配置文件路径与 unix 一致（`%USERPROFILE%\.claude.json`、`.codex\config.toml`、`.gemini\settings.json`、`.config\opencode\opencode.json`）—— 已核对各 CLI 源码确认。

## 快速开始

```bash
# 启动 TUI（首次运行会自动扫描、注册 MCP，并从 skill-manager 迁移）
runai

# 从 GitHub 安装 skills（自动下载、注册、分组、启用）
runai install pbakaus/impeccable
runai install MiniMax-AI/skills

# 从市场安装
runai market-install github

# 查看使用统计
runai usage --top 10

# 发现磁盘上所有 skill
runai discover

# CLI 管理
runai list                    # 列出所有 skills 和 MCPs
runai status                  # 查看启用数量
runai enable brainstorming    # 启用某个 skill
runai scan                    # 扫描已知目录
runai backup                  # 创建备份
```

## TUI 快捷键

底部显示常用按键，按 `?` 打开完整帮助面板。

| 按键 | 操作 |
|------|------|
| `j/k` | 上下导航 |
| `H/L` 或 `Tab` | 切换标签页（Skills / MCPs / Groups / Market） |
| `Space` | 启用/禁用 |
| `Enter` | 打开分组详情 / 从市场安装 |
| `/` | 搜索过滤 |
| `1234` | 切换 CLI 目标（Claude/Codex/Gemini/OpenCode） |
| `i` | 从 GitHub 安装 |
| `t` | 切换深色/亮色主题 |
| `?` | 帮助面板（所有快捷键） |
| `q` | 退出 |

## MCP 工具（30 个）

作为 MCP 服务器运行时（`runai mcp-serve`），提供 30 个工具：

**Skills 和 MCPs**

| 工具 | 说明 |
|------|------|
| `sm_list` | 列出 skills/MCPs 及使用次数（支持按类型/分组/目标过滤） |
| `sm_status` | 各 CLI 的启用/总数统计 |
| `sm_enable` / `sm_disable` | 启用/禁用（支持模糊组名匹配） |
| `sm_delete` | 删除 skill/MCP（文件 + 软链接 + 数据库） |
| `sm_scan` | 扫描已知目录发现新 skills |
| `sm_discover` | 全盘发现 SKILL.md，返回未管理的 skill 列表 |
| `sm_search` | 统一搜索已安装资源 + 市场 |
| `sm_batch_enable` / `sm_batch_disable` | 批量启用/禁用多个 |

**安装**

| 工具 | 说明 |
|------|------|
| `sm_install` | 返回 CLI 安装命令（AI 通过 Bash 执行，避免代理超时） |
| `sm_market` | 浏览缓存的市场 skills（按源/关键词/路径过滤） |
| `sm_market_install` | 返回市场安装 CLI 命令 |
| `sm_batch_install` | 返回批量安装多个 skill 的 CLI 命令 |
| `sm_sources` | 列出/添加/删除/启用/禁用市场源 |

**分组**

| 工具 | 说明 |
|------|------|
| `sm_groups` | 列出所有分组及成员数 |
| `sm_create_group` / `sm_delete_group` | 创建/删除分组 |
| `sm_group_add` / `sm_group_remove` | 添加/移除成员（支持单个 `name` 或批量 `names`） |
| `sm_update_group` | 更新分组名称和/或描述 |
| `sm_group_enable` / `sm_group_disable` | 批量启用/禁用分组成员（模糊组名匹配） |

**使用追踪**

| 工具 | 说明 |
|------|------|
| `sm_record_usage` | 记录 skill 或 MCP 的使用事件 |
| `sm_usage_stats` | 查看使用统计，按使用次数排序 |

**备份与工具**

| 工具 | 说明 |
|------|------|
| `sm_backup` | 创建带时间戳的备份 |
| `sm_restore` | 从备份恢复（默认最新，可指定时间戳） |
| `sm_backups` | 列出所有可用备份 |
| `sm_register` | 注册 MCP 到所有 CLI 配置 |
| `sm_batch_delete` | 批量删除多个资源 |

## 多 CLI 配置格式

| CLI | 配置文件 | 格式 |
|-----|---------|------|
| Claude | `~/.claude.json` | JSON (`mcpServers`) |
| Codex | `~/.codex/config.toml` | TOML (`[mcp_servers.*]`) |
| Gemini | `~/.gemini/settings.json` | JSON (`mcpServers`) |
| OpenCode | `~/.config/opencode/opencode.json` | JSON (`mcp`，command=数组) |

## 数据存储

所有数据存储在 `~/.runai/`：
- `skills/` — 托管的 skill 目录（每个包含 SKILL.md）
- `mcps/` — 被禁用的 MCP 配置备份（JSON）
- `groups/` — 分组定义（TOML 文件）
- `backups/` — 带时间戳的完整备份
- `market-cache/` — 市场 skill 列表缓存（JSON，1 小时有效期）
- `market-sources.json` — 自定义市场源
- `runai.db` — SQLite 数据库（skill 元数据、使用统计、分组成员）

## 从 skill-manager 迁移

Runai v0.5.0 首次启动时自动迁移：
1. 数据目录：`~/.skill-manager/` → `~/.runai/`
2. 数据库：`skill-manager.db` → `runai.db`
3. 软链接：所有 CLI skill 软链接自动重新指向
4. MCP 条目：所有 CLI 配置中 `skill-manager` → `runai`
5. 环境变量：`RUNE_DATA_DIR` 和 `SKILL_MANAGER_DATA_DIR` 都可用

无需手动操作，所有数据完整保留。

## 许可证

MIT
