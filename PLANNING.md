# runai 规划文档

> 这份文档列出 runai 项目当前已对齐方向但尚未实施、以及更长期已构思的工作。
> 每个新启动的 AI 助手在动手任何相关方向前都应读完本文，避免重新设计已经定稿的方案。
> 本文不写耗时、不写时间轴。每个条目都用"改动位置 + 改动性质 + 跨调用链影响"描述范围。

---

## 1. 已对齐方向（待实施）

### 1.1 owner 模式 vs team 模式 —— server 启动 flag

**定稿决策**：模式区分由 `runai server` 启动参数决定，不依赖运行时"有几个用户注册过"的隐式推断。

- 新参数：`runai server --mode {owner|team}`，默认 `owner`
- owner 模式语义：
  - 单用户单机自用。`/users/register` 端点返回 403（拒绝外部新建账号）
  - `/install` `/uninstall` 端点返回 404（owner 模式不暴露远程安装入口）
  - dashboard 不渲染"账号注册"按钮，登录条目隐藏，本机用户为隐式 admin
  - admin 面板隐藏（owner 模式没有"其他用户"这个概念）
- team 模式语义：
  - `/users/register` 开放，第一个注册的自动 admin
  - `/install` `/uninstall` 返回**按 team 模式裁剪过的脚本模板**（见 1.2）
  - dashboard 顶栏渲染账号 pill + 注册/登录入口
  - admin 面板可见，可管全部用户的 skill 与权限

**改动位置**：
- `src/cli/mod.rs::Commands::Server` 新增 `mode: ServerMode` 字段
- 新增 `src/core/server_mode.rs`（枚举 + 解析 + 透传到 axum AppState）
- `src/server/mod.rs` AppState 加 `mode` 字段；每个 handler 按 mode 短路
- `src/server/auth.rs`（或现有 auth 文件）`api_register` 在 owner 模式直接返回 403
- 自启脚本 `src/core/autostart.rs` 写 LaunchAgent / systemd unit 时把 mode 注入命令行参数

**测试改动**：
- `tests/multiuser_owner_e2e.rs` 拆出 `tests/server_mode_e2e.rs`：
  - owner 模式下 register/install/uninstall 端点全 403/404
  - team 模式下 register 走通、二个用户隔离
  - 跨 mode 切换（server 重启换 flag）下旧数据兼容

### 1.2 客户端安装 —— server 模板化脚本

**定稿决策**：客户端入口是**脚本**而非 binary。脚本内容由 server 按自身 mode 生成。客户端无 runai binary 装载需求。

- owner 模式 server：`/install` 直接 404，远程客户端无法拉脚本
- team 模式 server：`/install` 返回当前模式下可用命令的脚本子集
  - 包含：账号注册/登录、写 `~/.runai-identity`、装 Claude Code UserPromptSubmit hook、装 `~/.runai-hook.sh`
  - 不包含：`scan` `discover` `migrate` `doctor --fix` 等本机管理命令（这些只在装了 runai binary 的服务端机器上可用）
- 脚本同时支持两种调用形态：
  - **人友好**：默认走 TTY prompt 收集用户名/密码（已有 `scripts/runai-client-install.sh` 路径）
  - **agent 友好**：
    - `--help` 列全部 env var 和 flag
    - 非交互模式：`RUNAI_USERNAME` `RUNAI_PASSWORD` env 注入跳过 TTY
    - `--register-only` `--login-only` `--hook-only` 子开关，让 agent 分步驱动

**改动位置**：
- `scripts/runai-client-install.sh` 和 `.ps1` 加 `--help` / 非交互分支
- `src/server/mod.rs` `/install` handler 按 `state.mode` 在脚本模板里删减命令段（用占位符注释包裹可裁剪段，server 端字符串替换）
- 同步 `/uninstall` 脚本

**测试改动**：
- 新增 `tests/install_script_e2e.rs`：
  - owner 模式下 `curl /install` 返回 404
  - team 模式下脚本不含 scan/discover/doctor 等命令字符串
  - `RUNAI_USERNAME=x RUNAI_PASSWORD=y bash install.sh` 非交互跑通

### 1.3 提示词集中化模块

**定稿决策**：把项目所有 LLM 提示词从代码 `include_str!` 散布状态收编到 `src/core/prompts/`，统一命名空间 + 注入开关 + 多用户隔离。运行时不支持用户从 `~/.runai/prompts/` 覆盖（避免破坏"读源码就懂"的可审性）。

- `src/core/prompts/mod.rs` 暴露每个提示词为 `pub const PROMPT_<NAME>: &str`
- 每个 `.md` 文件加 frontmatter（用处 / 调用方 / 输入变量 / 输出契约）
- `src/core/prompts/AGENTS.md` 列出所有提示词的索引、每个提示词的"调用方-参数-用法"

**注入开关**：
- 现有 `recommend` 已经有 `enabled` flag。其他提示词逐个补 `enabled` 开关
- 多用户场景下"是否注入"是**per-user 配置**，落在 `users.prefs_json`
- server `/recommend` 收到请求后读发起者的 `prefs.prompt_injection_flags`，按开关裁剪

**多用户隔离测试**（这块用户特别强调要严测）：
- 新增 `tests/prompts_multiuser_e2e.rs`：
  - 用户 A 关闭某提示词、用户 B 开启 → 同一时刻并发 `/recommend` 拿到不同结果
  - 用户 A 改了自己的 prefs 不能影响 B 的请求
  - 未登录请求走兜底默认配置，不读任何 user prefs
  - 切换登录账号后 hook 立刻拿到新账号的 prefs（不缓存旧账号）

**改动位置**：
- `src/core/prompts/` 新增 `mod.rs` + `AGENTS.md`
- 现有 7 个 `.md` 文件加 frontmatter
- `src/core/recommend/{prompts,router,project_context,hook_output}.rs` 把 `include_str!("../prompts/...")` 改成 `use crate::core::prompts::PROMPT_...`
- `src/core/prefs.rs` 加 `prompt_injection_flags: HashMap<String, bool>` 字段
- `src/server/api_prefs.rs`（或现有 prefs 路由）暴露 GET/POST 读写

### 1.4 社区市场（team 模式核心）

**定稿决策**：team 模式服务端运营一个用户互享 skill 池，承载上传、下载、浏览、安装。dashboard market tab 现有 skills.sh 数据不变；新增"社区"子 tab 接社区市场端点。TUI 是否加见 1.5。

**新端点**：
- `POST /api/community/upload` —— 上传 skill 包（gz tar，沿用现有 `/skills/bundle/{name}` 的反向）
  - 鉴权：必须 Bearer 登录态
  - 服务端落到 `<data>/community/<uploader_uid>/<skill_name>/`
  - 写 `community_skills` 表：uploader_uid / name / version / installs_total / created_at
- `GET /api/community/list` —— 按 install / created_at / name 排序分页
- `GET /api/community/skill/{uid}/{name}` —— 详情（含 README、版本历史）
- `GET /api/community/download/{uid}/{name}` —— 下载 gz tar
- `POST /api/community/install/{uid}/{name}` —— 调用方拉到自己的私有池（owner-aware install）
- `DELETE /api/community/skill/{uid}/{name}` —— 仅 uploader 或 admin

**agent 友好接口**：上面所有端点都返回 JSON；不依赖 dashboard，外部 agent 可直接 curl 操作。

**人友好接口**：
- dashboard 加"社区"tab，复用现有 `.ml-row` 样式
- "上传" 按钮触发本地 zip 选择 → 走 `/api/community/upload`

**改动位置**：
- `src/server/community.rs` 新增（按 server 现有按路由族拆分约定）
- `src/server/AGENTS.md` 加 community.rs 一行
- `src/core/db/community.rs` 新增 schema v16 ALTER 加 `community_skills` 表
- `web/js/` 新增 `17-community-market.js`、`web/css/` 新增 `12-community-market.css`
- `src/server/mod.rs` `STATIC_JS` / `STATIC_CSS` 追加新文件 `include_str!`

**测试改动**：
- 新增 `tests/community_market_e2e.rs`：上传/下载/列表/详情/删除全链路
- 多用户隔离：A 上传后 B 看得到、B 装到自己私有池物理路径正确（`<data>/users/<B_uid>/skills/<name>/`）
- 删除：非 uploader 非 admin 删返 403

### 1.5 TUI 安装/卸载流程

**定稿决策**：TUI 选择性加，权衡为只加 hook 安装/卸载和市场浏览。不加完整的"server mode 切换"（那是命令行/服务端管理动作）。

**TUI 改动范围**：
- `src/tui/app/` 新增 hook 安装/卸载 panel
- `src/tui/app/` Market tab 接入社区市场（与 dashboard 数据源一致）
- 现有 skill 管理 tab 不动

**不在 TUI 做的**：
- server mode 切换（命令行专用）
- 社区市场上传（dashboard 专用，因为涉及文件选择）

---

## 2. 强约束（动手前必须遵守）

### 2.1 测试 —— orb VM + 物理 e2e 双轨

`AGENTS.md` 安全契约要求所有高危改动跑物理 e2e。本节新增约束：

- 涉及多用户隔离、客户端脚本、社区市场的 e2e **必须在 orb 虚拟机上跑一遍真实跨机场景**，不止 mktemp HOME
- orb VM 测试：起两台 VM
  - VM1 跑 `runai server --mode team --port 17888`
  - VM2 不装 runai，仅 `curl http://VM1:17888/install | bash`
  - 验证 VM2 注册账号后 hook 真打到 VM1、`/recommend` 返回正确结果、VM2 装了一个社区 skill 后物理落在 VM1 的 `<data>/users/<VM2-uid>/skills/`
- 测试矩阵写进 `tests/orb_vm_matrix.md`（手动跑，CI 不跑 orb；本地 release 前必跑）

### 2.2 多用户隔离 —— 提示词与 prefs 是高风险区

提示词集中化里"不同账号得到不同结果"是用户重点关注的可靠性风险。所有涉及读 `users.prefs_json` 的代码路径必须：

- 显式从请求拿 owner_uid，**不能从全局状态/上次请求残留**取
- 单元测试覆盖：两个用户的请求在同一进程并发，prefs 串扰为 0
- `src/server/AGENTS.md` 加一条：任何 handler 读 prefs 前必须先经过 `current_owner_id`，跳过 = bug

---

## 3. 后续规划（已构思，未排期）

下列方向已构思但未细化到改动位置级别。任一开始实施前先在本节扩成 1.x 形态。

### 3.1 每个 skill 的详情页（dashboard 端）

每个 skill 在 dashboard market tab 点进去要有独立详情页，含：SKILL.md 渲染、安装计数、版本历史、依赖关系、用户评论、相关 skill 推荐。

### 3.2 skill 质检

skill 上传到社区市场前自动跑：SKILL.md 字段完整性、frontmatter 合法性、依赖 mcp 存在性、危险命令扫描、试运行（podman 隔离）。质检报告随 skill 一起展示。

### 3.3 skill 面板图

每个 skill 一张可视化"调用图谱"：何时被 router 选中、依赖哪些工具、被哪些 group 包含、与哪些 skill 常共现。可能用 ASCII 或 SVG，定稿在 3.1 之前。

---

## 4. 已完成（不要重做）

- v15 multi-user schema：`users` / `user_skill_library` / `resources.owner_user_id` / `router_events.user_id`
- 物理隔离：`<data>/skills/` 公共池 + `<data>/users/<uid>/skills/` 私有池
- owner-aware 查询：`Database::list_resources_for_user` / `find_resource_by_name_for_user`
- server 端 `current_owner_id` + `resolve_skill_dir`
- 第一个注册用户自动 admin
- `/skills/bundle/{name}` 打包下载（owner-aware）
- 启动期 `cleanup_orphan_library_entries`
- `tests/multiuser_owner_e2e.rs` 6 个测试覆盖 owner-pool 契约
- market dashboard 翻页闪烁修复（`fix/market-pagination-flicker` 分支，commit `2d2c01d`）

---

## 维护规则

- 新方向先进 §3（后续规划），细化后升到 §1（已对齐待实施）
- §1 任一条实施完进 §4（已完成）
- §2 约束只增不删；删除要在 commit message 写清楚为什么
- 实施时 commit message 引用本文档章节号（如 `[feat] 实施 PLANNING.md §1.3 提示词集中化`）
