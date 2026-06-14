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

**这里"客户端"指什么场景**：

设想 admin Alice 在自己机器上跑 `runai server --mode team`。同事 Bob **没装 runai binary**，他想用 Alice 的 server。Bob 的所有操作（注册账号、装 hook、上传社区 skill、看市场）都通过 `curl Alice-server/...` 完成。

问题：怎么保证 Bob 拉下来的命令集只包含 team 模式相关功能、不暴露 Alice 服务端的本机管理命令（`scan` / `discover` / `doctor` 等）？

**定稿决策**：客户端入口是**脚本**而非 binary。脚本内容由 server 按自身 mode 现场生成（不是 repo 里那份固定 install.sh 原样发出去）。

- Bob `curl http://alice-server:port/install | bash` → 拉到的是 server 端拼装版本
- Alice server 是 team 模式 → 脚本含 register / login / hook 装载 / 社区上传 / 库管理
- Alice server 是 owner 模式 → `/install` 端点直接 404，Bob 拉不到脚本（owner 模式 = 单人自用，外部远程客户端无入场）

也就是"客户端能看到什么、能跑什么"由 server mode 在生成脚本时决定，不靠客户端"主动表现良好"。

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

**注入开关 / 用户偏好**：

提示词内容固定（编进 binary），不让用户改正文。可改的只是"启用 / 跳过"这一组 per-user boolean。

- 现有 `recommend` 已经有 `enabled` flag。其他提示词逐个补 `enabled` 开关
- 多用户场景下"是否注入"落在 `users.prefs_json.prompt_injection_flags: HashMap<String, bool>`
- server `/recommend` 收到请求按发起者 uid 读 flags，按开关裁剪
- 默认值（首次注册）= 全部启用，用户后续按需关
- dashboard "我的偏好" tab 加一组启用开关列表，一行一条提示词，含名字 + 用处描述 + toggle
- 改完即时 POST `/api/prefs`，无需重启

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

### 1.4 社区市场（team 模式核心 — 2026-06 重写)

**定稿决策(rewrite)**：默认 upload 不直接进社区池,而是 **「私有池 → 自动富集 → 用户申请 publish → admin 审核 → 社区池」** 四段工作流。这避免任何 user 都能把没经审核的 skill 推到全员共享池里。原 §1.4 直传社区池的描述见 §4 已完成第一波 (C6/C7);本段是 publish workflow 第二波 (C9a-C9h) 的最终设计。

**核心数据**:
- `resources.publish_status TEXT NOT NULL DEFAULT 'draft'` (schema v17),可选值 `draft` / `pending` / `approved` / `rejected`。
- `resources.publish_reason TEXT` 存 admin 拒绝时的理由。
- public-pool row(`owner_user_id IS NULL`)的 publish_status 永远 'draft' 被工作流忽略。

**新端点**(C9a/C9c/C9d/C9e 后端):
- `POST /api/users/me/skills/upload` (multipart name + bundle) → 落 `<data>/users/<uid>/skills/<name>/` + `resources` 行 owner_user_id=uid + publish_status='draft' + spawn_enrich(name)。
- `POST /api/users/me/skills/{name}/publish-request` → draft → pending。pre-condition: `resource_ai_summary.summary` 非空(enrich 已完成)。
- `GET /api/users/me/skills` → 当前用户的私有 skill 表 + workflow 状态 (uploaded_at / enrich_status / publish_status / publish_reason)。
- `GET /api/admin/publish-requests` → admin-gated 列所有 pending,JOIN users + ai_summary。
- `POST /api/admin/publish-requests/{resource_id}/approve` → pending → approved + 复制 `<data>/users/<uid>/skills/<name>/` 到 `<data>/community/<uid>/<name>/` + community_skills upsert。
- `POST /api/admin/publish-requests/{resource_id}/reject` `{reason}` → pending → rejected + 写 publish_reason (空 reason 400)。

**保留的旧端点**(C6 一波 — 社区池浏览 / 安装,仍是 read 路径):
- `GET /api/community/list` 社区池排序分页。
- `GET /api/community/skill/{uid}/{name}` 社区池详情。
- `POST /api/community/install/{uid}/{name}` 安装到自己私有池。
- `DELETE /api/community/skill/{uid}/{name}` 仅 uploader / admin。

**已废弃**:`POST /api/community/upload` 不再是 user-facing 入口(底层路径仍在被 C9d approve 内部复用,但 user 走 publish-request,admin 走 approve)。

**runai-client CLI 接面**(C9f):
- `runai-client upload` 默认走 `/api/users/me/skills/upload`(私有,publish_status='draft')。
- `runai-client list-mine` 拉自己的私有 skill 表。
- `runai-client publish <name>` 提交 publish-request。
- `runai-client list` 浏览社区池(仍是原 `/api/community/list` 路径)。
- `runai-client install <uid> <name>` 装社区 skill 到自己私有池。

**dashboard 接面**(C9g):
- 用户库 sub-tab (C6b/c) 显示每用户私有 + 导入。
- Admin tab 加「待审核发布」section,approve / reject 按钮,reject 弹 `window.prompt` 收理由。
- 普通用户上传 / 申请发布走 CLI(浏览器不打包 tar 路径)。

**安全 / 不变量**:
- 上传始终落自己私有池,不会动其他用户私有 row(`owner_user_id != uid` → 不存在,因为 `register_local_skill_for(name, Some(uid))` 唯一 owner 写入路径)。
- publish-request 前端 / CLI 都不能跳过 enrich gate(server 端 `resource_ai_summary` 非空校验是唯一可信线)。
- admin reject 必须给 reason,空 reason 400(让用户能改后再申请)。
- approve 复制后用户私有副本仍保留(双份),uploader 想撤销可走 `runai uninstall`(只动私有 row),社区池行靠 admin trash。

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

**上传入口按使用者机器分场景**：

| 使用者 | 机器状态 | 入口形态 |
|---|---|---|
| server 管理员 | 装了 runai binary 且本机就是 server | 命令行 `runai community upload --path <dir> --name <name>` 或 runai TUI Community tab 扫描 + picker |
| 远程用户（Bob） | 无 binary，curl 装过 install 脚本 | install 脚本生成的 bash 命令 `runai-client upload`，扫描 `~/.claude/skills/` 与当前 cwd 下 `.claude/skills/` 项目 skill，TUI 模式靠 fzf/gum 让用户勾选，CLI 模式 `--path <dir> --name <name>` 非交互 |
| agent | 任一机器 | 直送 tar gz 到 /api/community/upload，自己处理分包，绕过任何 TUI |

remote 客户端 TUI 上传依赖 `fzf` 或 `gum`，脚本检测缺则给安装指引并 fallback 到 CLI 模式（不强求装 TUI 工具）。

**dashboard 不做上传 UI**：浏览器无法直接打包目录，FormData 上传体验差且重复 CLI/TUI 路径。社区 tab 只渲染浏览 + 安装 + 删除 + 详情；上传走命令行。空池提示文案明确指引用户用 `runai community upload` 或 TUI。

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
- `src/tui/app/` Community tab 接入社区市场（与 dashboard 数据源一致）
- `src/tui/app/` Community tab 加上传 panel：扫描 `~/.claude/skills/` + cwd 下 `.claude/skills/` 列出本机所有 skill，用户用方向键选择 + Enter 上传（复用 `cli::handlers::community::upload` 逻辑）—— 待办，dashboard 上传 UI 已经移除
- 现有 skill 管理 tab 不动

**不在 runai 本机 TUI 做的**：
- server mode 切换 —— 命令行专用，TUI 切换 mode 涉及重启 server，不适合

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

### 2.3 抓包安全 —— 提示词不进网络流、客户端流量加固、抓包探索成本拉高

**核心原则**：`src/core/prompts/*.md` 任何一行内容都不出现在 client ↔ server 之间的 HTTP body 或 header 里。

**威胁模型**：

- T1 client 端用户在自己机器上 mitmproxy / Charles 抓 client ↔ server 流量，目的拿提示词模板、看路由逻辑、试探其他用户 skill
- T2 同网段中间人被动 sniff client ↔ server 流量
- T3 自动化 agent 通过 `/openapi.json` `/swagger` `/docs` `/__schema` 等惯用端点反向工程 API 形态
- T4 通过登录端点暴力枚举账号是否存在

**防护设计**：

1. **提示词永不离开 server 进程**：
   - 现状已天然满足：`src/core/prompts/*.md` 通过 `include_str!` 编进 binary
   - hook → `/recommend` 协议里，client 发 `user_prompt`，server 内部调 LLM provider，仅返回路由 decision + SKILL.md 正文
   - 提示词模板只出现在 server → LLM provider 出向请求里，client ↔ server 流量永远不含
   - 验证：新增 `tests/prompt_leak_e2e.rs`，跑一组 `/recommend` 请求，dump 所有响应 body + header，grep 任一提示词的指纹字符串必须返回零行
   - 同步检查：`/api/event/:id` 详情端点不能把 `user_prompt`(已落库) 拼提示词后返回。dashboard 看历史 event 只看用户提交那部分，不还原成完整 LLM input

2. **强制 HTTPS（team 模式）**：
   - owner 模式可裸 HTTP（127.0.0.1 不出外网）
   - team 模式 server 启动时校验：若 `--host` 不在 127.0.0.1/::1 且未配 TLS → 直接拒启
   - 新增 server flag：`--tls-cert <path>` `--tls-key <path>`
   - 新增 `scripts/runai-gen-tls.sh` 生成本地 CA + server cert + client trust chain，用于自部署 team server quickstart
   - 改动位置：新增 `src/server/tls.rs`（用 `axum-server` rustls feature 而非 native-tls），`src/cli/mod.rs::Commands::Server` 加两字段，`src/core/server_mode.rs` 校验 `team + non-loopback + no-tls = bail`

3. **客户端证书指纹 pinning**：
   - install 脚本拉下来时把 server 证书 SHA-256 烧进 `~/.runai-server.json`
   - `~/.runai-hook.sh` 每次发 `/recommend` 先校验 server cert 指纹，不匹配则拒绝并报错
   - 防御 T2 中间人换证书攻击
   - 改动位置：`scripts/runai-client-install.sh` 增 `--pin-fingerprint` 选项；`~/.runai-hook.sh` 改用 `curl --cacert <pinned-cert>` 而非系统信任链

4. **反 agent 探索**：
   - 不暴露 `/openapi.json` `/swagger` `/swagger-ui` `/docs` `/api-docs` `/__schema` 任一路径
   - 任何路径不存在统一 404 + 空 body，鉴权失败统一 401 + 空 body，**不靠错误文案区分两者**
   - dashboard `/api/*` 全部需要 Bearer 或 session cookie，未授权一律 401 + 空 body
   - GET `/` 渲染 dashboard HTML 不嵌 routes inventory / API endpoint 列表
   - 改动位置：`src/server/mod.rs` 加全局 fallback handler 返回空 404；逐个 API handler 校验返回值不带任何"路径不存在" / "需要登录"之类的人类可读提示

5. **登录端点抗枚举**：
   - `/auth/login` 失败统一返回 `401 + {"error":"invalid_credentials"}`，**不区分**"用户不存在"vs"密码错"
   - 服务端日志可以记真因供 admin 排错，但响应里不含
   - 改动位置：`src/server/auth.rs` 现有 register/login handler 收口错误返回

6. **速率限制**：
   - `/auth/login` 单 IP 每分钟 5 次，超返 429 空 body，不带 retry-after 计数细节
   - `/api/community/upload` 单 user 每小时 10 次
   - `/skills/get/*` 单 IP 每秒 20 次
   - 改动位置：新增 `src/server/middleware/rate_limit.rs`（用 `tower-governor` 或自写计数；现有依赖优先），`src/server/mod.rs` 挂中间件

**已知妥协**：

- team 模式 admin 可读 `router_events` 表的历史 `user_prompt`（用户提交给 router 的 prompt 已落库做 dashboard 展示）。这是设计而非 bug。team server 的 README / install 脚本输出里必须明确告知用户："你提交的 prompt 会被 server 落库 N 天用于展示"，让用户在使用前知情
- LLM provider 一侧抓包属于"信任你选的 LLM 厂商"问题，runai 不解决。文档里建议 team 模式 admin 选支持 zero-retention 的 provider（DeepSeek / Anthropic claim 不落 prompt）
- 客户端机器本身被攻破后什么都拦不住（拿到 `~/.runai-identity` 的 api_key 就等于该用户），这不在抓包模型里

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
- §1.1 owner 模式 dashboard 后端裁剪：进程级 `SERVER_MODE` atomic + `synthetic_owner()`（implicit admin sentinel `user_id="owner"`） + `state::current_user` owner 短路 + `state::private_data_locked` owner 恒 `false` + `MeResp.mode` 字段 + `serve_index` 注入 `body class="mode-owner"`；41 个 `require_user`/`require_admin`/`current_owner_id` 调用点零改动。`tests/server_mode_dashboard_e2e.rs` 7 fn 物理 e2e 守。
- §1.1 owner 模式 dashboard 前端裁剪：`web/css/13-owner-mode.css` 一刀切 hide `#account-pill` / `#auth-modal` / `#library-scope-bar` / market 社区 tab btn / `#market-community-pane` / `#community-detail-modal` / `:has(#admin-users-rows)` 用户管理 section；保留路由总闸门 + 运营商配置（owner 本人隐式 admin）。`11-account-library.js::refreshMe` 每次同步 body `mode-owner` class；`12-admin-scope-skills.js::loadAdminUsers` owner 模式 short-circuit return。真浏览器渲染断言 spec 入 issue #20（Playwright harness 待重建）。
- §1.4 重写 publish 工作流(C9a-C9h 8 commit): schema v17 加 resources.publish_status + publish_reason; POST /api/users/me/skills/upload 私有上传 + spawn_enrich; publish-request 端点 + enrich gate; admin GET /publish-requests + approve(copy 到社区池 + community_skills) / reject(reason 必填); GET /api/users/me/skills list-mine + workflow 状态; runai-client 加 list-mine / publish 子命令,默认 upload 走 private; dashboard Admin tab 新增「待审核发布」section + approve/reject 按钮。tests: private_skill_upload_e2e (5 fn) / publish_request_e2e (4 fn) / admin_publish_approve_e2e (7 fn) / list_mine_e2e (4 fn)。

---

## 维护规则

- 新方向先进 §3（后续规划），细化后升到 §1（已对齐待实施）
- §1 任一条实施完进 §4（已完成）
- §2 约束只增不删；删除要在 commit message 写清楚为什么
- 实施时 commit message 引用本文档章节号（如 `[feat] 实施 PLANNING.md §1.3 提示词集中化`）
