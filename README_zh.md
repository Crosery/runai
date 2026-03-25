# Skill Manager

[English](README.md) | **中文**

终端界面的 AI CLI skill/MCP 资源管理器。支持 **Claude Code**、**Codex**、**Gemini CLI** 和 **OpenCode**。

## 功能特性

- **TUI 终端界面** — 浏览、启用/禁用、搜索 skills 和 MCPs
- **多 CLI 支持** — 跨 4 个 AI CLI 统一管理，`1234` 切换目标
- **分组管理** — 将 skills/MCPs 组织成组，批量启用/禁用
- **一键安装** — `skill-manager install owner/repo` 自动下载、注册、分组、启用
- **Skill 市场** — 浏览 2000+ 来自 5 个内置源的 skills，支持自定义 GitHub 源
- **MCP 服务器** — 24 个工具通过 MCP 协议暴露，首次启动自动注册到所有 CLI
- **文件系统为唯一数据源** — skill 启用 = 软链接存在；MCP 启用 = 配置条目存在
- **备份与恢复** — 带时间戳的完整备份，包括 skill 文件、MCP 配置和 CLI 配置
- **命令行** — 子命令支持脚本自动化

## 安装

```bash
git clone https://github.com/Crosery/skill-manager.git
cd skill-manager
cargo install --path .
```

## 快速开始

```bash
# 启动 TUI（首次运行会自动扫描并注册 MCP）
skill-manager

# 从 GitHub 安装 skills（自动下载、注册、分组、启用）
skill-manager install pbakaus/impeccable
skill-manager install MiniMax-AI/skills

# 或直接使用 CLI
skill-manager list                    # 列出所有 skills 和 MCPs
skill-manager status                  # 查看启用数量
skill-manager enable brainstorming    # 启用某个 skill
skill-manager scan                    # 扫描新 skills
skill-manager backup                  # 创建备份
```

## 架构

```
文件系统是唯一的数据源：
  Skill 启用 = ~/.claude/skills/<name> 软链接存在
  MCP 启用   = ~/.claude.json mcpServers 中存在条目
  MCP 禁用   = 条目被移除，配置备份到 ~/.skill-manager/mcps/

DB 只存储：
  Skill 元数据（名称、描述、来源、目录）
  分组成员关系（支持 skill 和 MCP 混合成员）
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
| `a` | 添加到分组（Skills/MCPs 页） |
| `s` | 源管理（Market 页）/ 扫描（其他页） |
| `[ ]` | 切换市场源 |
| `q` | 退出 |

## MCP 工具（24 个）

作为 MCP 服务器运行时（`skill-manager mcp-serve`），提供 24 个工具：

**Skills 和 MCPs**

| 工具 | 说明 |
|------|------|
| `sm_list` | 列出 skills/MCPs（支持按类型、分组过滤） |
| `sm_status` | 各 CLI 的启用/总数统计 |
| `sm_enable` / `sm_disable` | 启用/禁用 skill/MCP |
| `sm_delete` | 删除 skill/MCP（文件 + 软链接 + 数据库） |
| `sm_scan` | 扫描目录发现新 skills（显示错误详情） |
| `sm_batch_enable` / `sm_batch_disable` | 批量启用/禁用多个 |

**安装**

| 工具 | 说明 |
|------|------|
| `sm_install` | 返回 CLI 安装命令（AI 通过 Bash 执行，下载更快） |
| `sm_market` | 浏览缓存的市场 skills（按源/关键词过滤） |
| `sm_market_install` | 从市场安装单个 skill |
| `sm_sources` | 列出/添加/删除/启用/禁用市场源 |

**分组**

| 工具 | 说明 |
|------|------|
| `sm_groups` | 列出所有分组及成员数 |
| `sm_create_group` / `sm_delete_group` | 创建/删除分组 |
| `sm_group_add` / `sm_group_remove` | 管理分组成员 |
| `sm_batch_group_add` | 批量添加成员到分组 |
| `sm_group_enable` / `sm_group_disable` | 批量启用/禁用分组内所有成员 |

**备份与工具**

| 工具 | 说明 |
|------|------|
| `sm_backup` | 创建带时间戳的备份 |
| `sm_restore` | 从备份恢复（默认最新，可指定时间戳） |
| `sm_backups` | 列出所有可用备份 |
| `sm_register` | 注册 MCP 到所有 CLI 配置 |

## MCP 管理行为

- **禁用** = 从 CLI 配置中删除条目，完整配置备份到 `~/.skill-manager/mcps/{name}.json`
- **启用** = 将备份的配置恢复写回 CLI 配置文件
- **skill-manager 不可禁用自身**（自我保护）
- 被禁用的 MCP 在 TUI/列表中仍然可见（显示为禁用状态，可重新启用）
- 首次启动时自动注册到所有 CLI

## Skill 发现

- 扫描 `~/.claude/skills/`（用户管理）和 `~/.claude/.agents/skills/`（插件管理，只读）
- 解析 SKILL.md frontmatter 中的 `description` 字段
- 重新扫描时自动刷新过时的描述
- 检测插件格式仓库（`.claude-plugin`），自动处理安装

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

## 备份与恢复

备份存储在 `~/.skill-manager/backups/{时间戳}/`：

```
backups/20260325_120000/
├── managed-skills/     # ~/.skill-manager/skills/ 的完整副本
├── managed-mcps/       # 被禁用的 MCP 配置备份
├── claude-skills/      # ~/.claude/skills/ 中的软链接
├── claude.json         # ~/.claude.json 的副本
├── gemini-settings.json
├── codex-settings.json
├── opencode-settings.json
└── timestamp
```

首次扫描前会自动创建备份。

## 数据存储

所有数据存储在 `~/.skill-manager/`：
- `skills/` — 托管的 skill 目录（每个包含 SKILL.md）
- `mcps/` — 被禁用的 MCP 配置备份（JSON）
- `groups/` — 分组定义（TOML 文件）
- `backups/` — 带时间戳的完整备份
- `market-cache/` — 市场 skill 列表缓存（JSON，1 小时有效期）
- `market-sources.json` — 自定义市场源
- `skill-manager.db` — SQLite 数据库（仅 skill 元数据 + 分组成员）

## 版本升级

从 v0.1.x 升级时：
- 旧 DB 表（`resource_targets`、`resources` 中的 MCP 行）保留但不再读取
- 新代码从文件系统读取状态而非 DB
- 降级回旧版本是安全的（旧数据完整保留）

## 许可证

MIT
