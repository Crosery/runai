<div align="center">

<img src="docs/images/runai-logo.png" alt="runai" width="180" />

# runai

### 一个终端原生的 AI CLI skill 路由器

<p>跨 Claude Code / Codex / Gemini CLI / OpenCode 的统一 skill / MCP 管理 + LLM 智能路由器 + 实时遥测仪表盘。</p>

<p>
  <a href="README.md"><b>English</b></a>
  &nbsp;|&nbsp;
  <a href="README_zh.md"><b>中文</b></a>
</p>

<p>
  <a href="#快速开始"><b>快速开始</b></a>
  &nbsp;·&nbsp;
  <a href="#三大支柱"><b>三大支柱</b></a>
  &nbsp;·&nbsp;
  <a href="#架构一览"><b>架构一览</b></a>
  &nbsp;·&nbsp;
  <a href="AGENTS.md"><b>AGENT 指南</b></a>
</p>

<sub>单一 Rust 二进制 · macOS / Linux / Windows · 无运行时依赖 · CC BY-NC-SA 4.0</sub>

</div>

---

<div align="center">

## 架构一览

<img src="docs/images/runai-architecture.png" alt="runai architecture" width="100%" />

</div>

---

## 一句话

`runai` 把"如何在四个 AI CLI 上安装、启用、推荐、观测 skill"这件事统一了。Skill 是磁盘上真实的目录，通过 symlink 关联到每个 CLI 的 skills 目录；MCP server 是每个 CLI 配置文件里的真实条目。**文件系统 = 真值，DB 只存元数据**。

在这套核心之上：

- **LLM skill router** 选 (opt-in)：每条 user prompt 自动选最合适的 skill 注入主 agent 上下文（BM25 prefilter + LLM rerank + 真采用计数）
- **本地 dashboard** 在 `http://127.0.0.1:17888`：每次 hook 触发、token 成本、延迟、被选 skill、完整 LLM 输入 都实时记录

---

## 解决的痛点

| 你以前的痛点 | runai 怎么解 |
|---|---|
| Skill 散落在 Claude Code / Codex / Gemini / OpenCode，每个 CLI 配置都有自己的坑 | 一个 TUI + CLI + MCP server 管全四个；每个 target 写原生格式 |
| `git clone` skill repo、手动拷文件夹、改 JSON / TOML、四个 CLI 重复一遍 | `runai install owner/repo` —— 一键下载 + 入库 + 分组 + symlink 到所有 CLI |
| 2000+ skill 散在 GitHub，没办法在终端里浏览 | 内置 market：`runai market` 浏览本地缓存索引，Enter 直接装 |
| 删了的 skill 想恢复回不来 | Trash-first：`runai uninstall` 进 `~/.runai/trash/`，`runai trash restore` 拉回来 |
| "我到底启用了哪些 skill？" —— `ls` 四个目录、对比配置文件、祈祷它们一致 | 真值 = symlink 存在 + 配置条目存在；`runai status` 实时读文件系统 |
| 不知道自己实际用了哪些 skill，不知道 router 每轮在干嘛 | Dashboard 在 127.0.0.1:17888 —— 每次 router 调用都记下被选 skill / BM25 命中 / 完整 LLM 输入 / hook 输出 / 延迟 / token |

---

## 三大支柱

### 1. 多 CLI skill / MCP 管理器

- **一次安装，全 CLI 启用** —— `runai install owner/repo[@branch]` 下载 skill、入 DB、symlink 进四个 CLI 的 skills 目录。MCP 条目按每个 CLI 的原生格式写（Claude JSON / Codex TOML / Gemini JSON / OpenCode JSON）。
- **文件系统 = 真值** —— Skill 启用 ⇔ `<cli-home>/skills/<name>` 存在 symlink。MCP 启用 ⇔ 目标 config 里有条目（无 `"disabled": true`）。DB 只是元数据；删了 DB 也不会坏事。
- **分组** —— 把相关 skill（`figma` / `ktv-car-project` / `ppt-slides` …）聚成命名组；按组批量启用 / 禁用 / 重命名。
- **Market** —— 内置 2000+ skill 市场，本地缓存，后台 1h TTL 刷新。`runai market install <name>` 一键装。
- **安全删除** —— 全部 trash-first，`runai trash purge` 才真删。

### 2. LLM skill router（opt-in）

- **Hook 集成** —— Claude Code 的 `UserPromptSubmit` hook → `runai recommend` → router 决策 → 输出作为额外 context 注入主 agent 的 prompt。Dashboard / team 场景下，本地 hook 读取 `~/.runai-identity`；key 有效时使用该用户的网页偏好，key 过期时返回空 hook 输出，不再静默退回匿名默认偏好。
- **BM25 prefilter + LLM rerank** —— 双语（latin + CJK）BM25 在 AI 生成的 summary 上跑，把受 `top_k` 约束的候选列表喂给 router LLM（默认 DeepSeek v4-flash；也支持任意 OpenAI 兼容 / Anthropic / `claude-cli` 后端）。混合分 = `BM25 × 0.4 + LLM 质量 × 0.6`。
- **AI summary 富集** —— 每个 skill 都由同一个 LLM 用你选定的 `summary_lang` 生成结构化 summary（`task / triggers / inputs / outputs / not-for / score`），既当 BM25 索引文本也当 router 候选上下文。富集以显式选定语言为前提，且输出语言被强制校验（不符先重试、再不符就丢弃不写），索引保持单一语言；`triggers` 字段保留跨语言关键词以提升检索。SKILL.md 编辑后自动 refresh，`runai install` / `scan` 也会针对改动的 skill 单点 re-enrich。
- **两种模式** —— `EXCLUSIVE` 让主 agent 在候选里挑；`COMPATIBLE` 一次加载多个互补 skill 适合工作流型 prompt（"整套调试链路" / "完整发版流程"）。同 session 去重，已采用的 skill 不再被重推。
- **真采用计数** —— Hook 输出让主 agent 运行 `runai-client activate <skill>`，必要时附带 runai 自己生成的 literal `rnai_sess_*` 会话 id。这个命令只有在 server 已 ACK `/skills/use/{name}`，或本地 durable outbox 已写入 `~/.runai/client-cache/servers/<server-key>/skills/<skill-key>/.outbox/` 后，才会把 `SKILL.md` 打到 stdout。
- **客户端缓存** —— `runai-client activate` / `sync` 把整个 skill 目录缓存到 `~/.runai/client-cache`，永远不写入受管真实池 `~/.runai/skills`。缓存命中也会先发送或入队 usage event，再输出 `SKILL.md`，所以降低内容请求压力不会丢采用计数。SKILL.md 引用的附属文件由 agent 通过 `runai-client file <skill> <relpath>` 从 cache 读取。

### 3. 实时遥测仪表盘

- **单一 binary 无 CDN** —— `runai server` 启动嵌入的 axum HTTP server；`web/{index.html,app.css,app.js}` 通过 `include_str!` 编译进 Rust 二进制。
- **每个 Claude Code 会话自动拉起** —— `runai server --install-hook` 加 `SessionStart` hook，让 dashboard 永远在 `http://127.0.0.1:17888`。
- **每次 router 调用都有埋点** —— 每条事件：model + provider，mode (compat / excl)，候选数，BM25 kept，prompt / completion / total tokens，延迟，被选 skill，状态，错误，完整 user prompt，工作目录，完整 LLM 输入字符串（64 KB cap），完整 hook 输出。
- **Admin 运营商检查** —— Dashboard Admin 把全局 recommend 总开关和个人偏好分开，并能对已保存 provider 发一条短模型请求，直接显示成功或 provider 返回的错误。Provider / model 是全局配置；prompt 注入开关、跳过提醒、我的库范围是用户偏好，只影响带有效身份的 `/recommend` 请求或本地 hook。
- **Skill 详情下钻** —— `/skills` 列出每个 skill 的使用次数、LLM 质量分、AI summary；点进去看完整目录树（浏览 SKILL.md + 配套文件）、最近使用历史、原始 description vs 富集后的 summary。
- **实时刷新** —— 5 秒轮询 + `inFlight` 防并发 + `visibilitychange` 切后台自动暂停。静态资源每次 boot 加 `?v=<时间戳>` cache buster，`cargo install` 升级 binary 后浏览器普通 reload 就拿新版，不用 hard refresh。

---

## 快速开始

### 安装

一键（macOS / Linux）—— 自动检测平台、下载并校验 checksum、装进 PATH，再按你选的版本配置：

```bash
curl -fsSL https://raw.githubusercontent.com/Crosery/runai/main/install.sh | sh
# 非交互（agent / CI）：
curl -fsSL https://raw.githubusercontent.com/Crosery/runai/main/install.sh | sh -s -- --edition personal --yes
```

Windows（PowerShell）：

```powershell
irm https://raw.githubusercontent.com/Crosery/runai/main/install.ps1 | iex
```

从源码（需要 Rust）：

```bash
cargo install --git https://github.com/Crosery/runai
```

**版本**（同一个 binary，安装时选）：`personal` = 本地 skill router + Claude Code hook，无账号无 server。`team` = 跑多用户 dashboard server，其它机器用 `curl -fsSL http://<host>:17888/install | bash` 接入。

安装器参数：`--edition personal|team`、`--version <tag>`、`--bin-dir <path>`、`--yes`、`--no-hook`、`--no-setup`、`--dry-run`、`--uninstall`。预编译的 `{linux,darwin,windows} × {amd64,arm64}` 在 [releases 页](https://github.com/Crosery/runai/releases)。Windows 上 symlink 需要开发者模式或管理员权限。

### 首次配置

```bash
# 1) 启动 TUI 浏览 / 启用已有 skill + 2000+ market skill
runai

# 2) 开启 LLM router (默认 DeepSeek v4-flash，约 $0.0001 / 次)
runai recommend setup
runai recommend install-hook          # 把 UserPromptSubmit + SessionStart hook
                                       # 写进 ~/.claude/settings.json（幂等，留 .runai-bak 备份）

# 3) 启动一次 dashboard，之后 hook 会自动拉起
runai server --port 17888 --ensure
runai server --install-hook            # 每个 Claude Code session 自动拉起
```

第 2 步装完，每条 Claude Code prompt 都走 `runai recommend`；被采用的 skill 通过 `runai-client activate` 激活，先记账或入队再输出缓存内容；每条 router 事件都会进 dashboard。

### 日常命令

```bash
runai                                 # TUI
runai install owner/repo              # 从 GitHub 安装 skill 到所有 CLI
runai market install <name>           # 从 market 安装
runai search <query>                  # 搜已安装 + market
runai status                          # 看所有 CLI 的启用 / 禁用状态
runai list --target claude            # 单 CLI 视图
runai backup                          # 带时间戳备份 skill + 配置
runai trash                           # 浏览已删，restore 或 purge
runai recommend enrich                # 重生 AI summary（mtime 检测增量）
runai recommend enrich --fix-lang     # 只重生语言不符的 summary
runai recommend stats                 # router LLM 用量 / 成本 / 延迟统计
runai doctor                          # 健康检查；`--fix` 清理 dangling symlink
```

完整 CLI 列表：`runai --help`。

---

## 数据放在哪

```
~/.runai/                              ~/.{claude,codex,gemini,opencode}/skills/
├── skills/<name>/SKILL.md            └── <name> -> ~/.runai/skills/<name>     ← symlink = 启用
├── mcps/<name>.json                  ~/.claude.json          ← MCP 条目 (Claude)
├── groups/<id>.toml                  ~/.codex/config.toml    ← MCP 条目 (Codex)
├── trash/<trash-id>/                 ~/.gemini/settings.json ← MCP 条目 (Gemini)
├── backups/<timestamp>/              ~/.config/opencode/opencode.json ← MCP 条目 (OpenCode)
├── market-cache/
├── users/<user_id>/skills/<name>/   ← v0.11.0-beta.5：私有 skill 物理隔离
├── config.toml                        ← runai recommend 配置 (provider, model, api_key)
└── runai.db                           ← SQLite: skill 元数据 / 使用统计 / router_events / AI summary
```

首次启动自动从 `~/.skill-manager/` 迁移过来（v0.5.0 转换）。Env 覆盖支持：`RUNE_DATA_DIR` 和 `SKILL_MANAGER_DATA_DIR`。

## skills.sh 聚合器（v0.11.0-beta.5）

Dashboard 的 Market tab 是 [skills.sh](https://www.skills.sh) 的 leaderboard 镜像（20K+ skill，2.6K GitHub 仓库），无需 API key。

- 三个排序 tab：**All Time / Trending (24h) / Hot** 对齐 skills.sh，全部 server-side 排序
- 8W TREND 列：每行迷你 sparkline（来自 skills.sh `weeklyInstalls` 8 周数据）
- INSTALLS 列：1.8M / 478.6K 格式化
- 服务端分页：默认 50/页 + prev / next 按钮，避免一页渲染 20K skill 卡顿
- 搜索框过滤后端实时查询（debounce 250ms）
- 点 install：runai 自动 fetch 该 skill 真实 GitHub 仓库的 tree，把整个 skill 目录装到 `~/.runai/users/<user_id>/skills/<name>/`
- 登录持久化：api_key 存 localStorage，cookie 失效后下次开浏览器仍登录态
- 原有 builtin GitHub 仓库 source 已全部撤掉；用户自己加 GitHub 仓库通过右上 `+ GitHub` 走 user-added source

## 开机自启（v0.11.0-beta.5）

```bash
runai server --install-autostart       # 装上登录自启动
runai server --uninstall-autostart     # 卸载
```

- macOS：写 `~/Library/LaunchAgents/cn.crosery.runai.plist` 并 `launchctl load -w`，登录自动起 + 崩了自动拉
- Linux：写 `~/.config/systemd/user/runai.service` 并 `systemctl --user enable --now`，用户 session 启动自动起
- Windows：未实现，命令会打印 Task Scheduler 手动配置步骤

跑 `runai`（默认 TUI）也会自动 `ensure_running` 拉起 server，设 `RUNAI_NO_AUTOSPAWN=1` 可关。

## 多用户私有 skill（v0.11.0-beta.5）

`runai server` 现在支持登录注册的用户，每个用户装的 skill 物理隔离到自己的 `~/.runai/users/<user_id>/skills/<name>/` 下，DB 字段 `owner_user_id` 区分公共与私有。

- 公共 skill（`owner_user_id IS NULL`）所有用户都看得到，物理在 `~/.runai/skills/<name>/`
- 私有 skill 只 owner 自己看得到；admin 用 `*` scope 看全部
- 同名 skill 可以公共一份 + 多用户各自一份共存，DB id 用 `u:<uid>:` 前缀避免冲突
- 客户端通过 install 脚本注册账号后，dashboard 的 install 走私有；CLI / TUI 装的还是公共

详见 [AGENTS.md](AGENTS.md) 的 "Per-user physical skill isolation" 段。

## 管理员重置密码

用户忘了密码时，管理员有三条正规路径重置（都会同时轮换该用户的 api_key，旧 Bearer / 已装客户端全部失效，用户必须用新密码重新登录）：

**本机 CLI**（在跑 server 的机器上，直接写本地 `runai.db`，无需 server 在线）：

```bash
runai admin reset-password <username>                 # 交互隐藏输入 + 二次确认
runai admin reset-password <username> --password <pw> # 非交互 / 脚本 & agent 用
```

这是"忘记密码时手改 `users` 表 SQL"的正规替代。用户不存在时干净报错，不 panic。

**服务端 API**（team 模式，管理员 Bearer / session cookie）：

```bash
curl -X POST http://<host>:17888/api/admin/users/<user_id>/reset-password \
  -H "Authorization: Bearer <admin_key>" \
  -H "Content-Type: application/json" \
  -d '{"new_password":"<新密码>"}'
```

返回 200 表示成功（响应只含 `user_id` / `username`，不回传新 key）；非管理员 403、未登录 401、用户不存在 404、密码短于 6 位 400。管理员也可以重置自己的密码。

**Dashboard**（team 模式，以管理员登录）：Admin tab → 用户表 → 每行「重置密码」按钮（对自己那一行也生效）。点击后 prompt 收新密码，调的是同一个 `/api/admin/users/{id}/reset-password` 端点，成功后弹窗提醒把新密码告知用户——其旧 api_key/登录态已经失效。

---

## 项目结构

| 模块 | 源码 | 干什么 |
|---|---|---|
| `cli/` | `src/cli/mod.rs` | clap 子命令分发；每个 `runai <verb>` 的入口 |
| `core::manager` | `src/core/manager.rs` | `SkillManager` 协调 install / enable / disable / trash / migrate |
| `core::scanner` | `src/core/scanner.rs` | 文件系统发现 + 未管理 skill 的 adopt（含 cross-data-dir 安全 guard）|
| `core::linker` | `src/core/linker.rs` | 跨平台 symlink create / remove / detect |
| `core::recommend` | `src/core/recommend.rs` | LLM skill router (BM25 + AI summary + LLM rerank + adoption tracking) |
| `core::db` | `src/core/db.rs` | SQLite schema (v14) + migration + 查询层 |
| `core::installer` | `src/core/installer.rs` | GitHub / market 安装流水线 |
| `mcp::tools` | `src/mcp/tools.rs` | 22 个 `sm_*` 工具通过 MCP stdio 暴露 |
| `tui/` | `src/tui/` | ratatui + crossterm 全屏 UI |
| `server` | `src/server.rs` | axum dashboard 服务 router 遥测 |

每个模块的深度文档在 `src/**/*.LLM.md`。架构不变量在 [AGENTS.md](AGENTS.md)。

---

## 设计原则

- **文件系统是真值** —— Skill 启用 = symlink 存在。MCP 启用 = config 条目存在。DB 只是元数据；删掉 DB 也能从磁盘重建。
- **Trash-first 全员通用** —— 删除可恢复，直到 `runai trash purge`。备份带时间戳，可还原。
- **单 binary，无运行时依赖** —— Web dashboard 资产 `include_str!` 进 binary。rusqlite bundled。无 node、无 python、无 Docker。
- **Router opt-in** —— 默认 `enabled = false`；`runai recommend setup` 之前没有任何网络请求。
- **真采用 > self-report** —— 计数靠 `runai-client activate`：只有 `/skills/use/{name}` 已 ACK，或本地 durable outbox 已写入，才会输出缓存内容。
- **破坏性 syscall 加 guard** —— `scan` / `adopt` 在 2026-04-27 事故后拒绝跨 data dir 做 `rename`。`tests/safety_e2e.rs` 物理 e2e 测试锁不变量。
- **文档同步铁律** —— 每个代码改动同 commit 改 `*.LLM.md`（见 [AGENTS.md](AGENTS.md)）。

---

## 许可证

[CC BY-NC-SA 4.0](LICENSE) —— 知识共享 署名-非商业性使用-相同方式共享 4.0 国际许可协议。

- **禁止商用** —— 不得将本项目或其衍生作品用于任何商业目的。
- **相同方式共享** —— 基于本项目的修改、衍生作品必须以相同协议开源。
- **署名** —— 必须保留原作者署名并链接回本仓库。

许可证全文：[creativecommons.org/licenses/by-nc-sa/4.0/deed.zh-hans](https://creativecommons.org/licenses/by-nc-sa/4.0/deed.zh-hans)。
