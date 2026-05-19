# runai 多用户沙箱执行体系 — 设计规范

- 文档日期：2026-05-20
- 目标版本：v0.12.0
- 状态：待评审
- 作用域：服务端架构由 “只下发 SKILL.md 文本 + 客户端 Bash 执行” 升级为 “服务端 podman 沙箱内执行 skill + 多用户隔离 + 用户偏好驱动的 hook 输出”

---

## 0. 目录

1. 背景与本次变更的根问题
2. 总体架构变化
3. 服务端 skill 执行：podman 沙箱
4. 多用户身份模型
5. 管理员引导与服务端启动门禁
6. 用户自注册流程
7. dashboard 鉴权与多用户视图
8. 用户偏好（hook 行为可配置）
9. 每用户 skill 可见性
10. dashboard 视觉重设计
11. 数据库 schema 新增项
12. 改动范围（文件级清单 + 风险标记）
13. 与 AGENTS.md 安全契约对齐
14. 待用户拍板的开放问题
15. 上线与回退方案

---

## 1. 背景与本次变更的根问题

当前 runai 是单用户、单机产品。`/skills/get/<name>` 返回 SKILL.md 文本，客户端 agent 自己用 Bash 跑 curl / 脚本。这种 “服务端只是 CDN，执行在客户端” 的模型暴露了三类问题：

- **客户端跑 skill 的安全边界是 agent 沙箱（=用户 home），没有任何资源限制**。skill 可以写任意路径、跑任意命令，受信链 = SKILL.md 文本本身。
- **没有用户概念**。dashboard 把所有 telemetry 公开渲染，install.sh 没有身份注入。Crosery 在团队内分发时，多人共用一个服务端 = 数据混在一起。
- **hook 输出格式是写死的**。同事反馈 “开头 tradeoff 块太啰嗦”、“session history 不需要”、“反馈协议每次都打印”，但当前 `render_hook_output` 不可配置。

本次变更同时解决这三个问题：执行边界往服务端收，引入用户实体，给用户可配置的 hook 行为开关。

---

## 2. 总体架构变化

当前形态：

```
client agent  --POST /recommend-->  server  --LLM rerank-->  返回候选 SKILL 名
client agent  --POST /skills/get-->  server  --return SKILL.md text-->  client agent 自行 Bash 跑
```

新形态：

```
client install.sh --gen identity--> ~/.runai-identity (secret + user_id)
client install.sh --POST /users/register Bearer--> server upsert
client agent --POST /recommend Bearer--> server (auth, prefs-aware render)
                                              --filter by user_skill_prefs-->
client agent --POST /skills/exec/{name} Bearer--> server
                                              --quota check-->
                                              --podman run sandbox-->
                                              --stream stdout/stderr-->
                                              --collect /output dir-->
                                              --audit row-->  client
```

要点：

- 客户端 agent 不再 `curl ... | bash`。skill 执行整体迁入服务端 podman 容器。
- 兼容退路：skill 目录 `skill.toml` 没有 `[exec]` section 时，`/skills/get/{name}` 仍按原协议返回 SKILL.md 文本（V1 渐进迁移）。
- 所有写库、写文件、查路由的接口走 `Authorization: Bearer <api_key>` 鉴权。
- 沙箱镜像统一为受信镜像（`ghcr.io/crosery/runai-skill-base:<tag>` 或 skill 自带 Dockerfile 由管理员预构建）。
- dashboard 不再公开。访问 `/` 强制 cookie session；无 session 跳 `/login`。

---

## 3. 服务端 skill 执行：podman 沙箱

### 3.1 HTTP 接口

**POST `/skills/exec/{name}`**

请求体：

```json
{
  "args": ["--mode", "fast"],
  "stdin": "可选的标准输入字符串",
  "env": {"FOO": "bar"},
  "timeout_override": 60
}
```

`env` 只允许 skill manifest 中 `[exec.env_allow]` 白名单的 key。`timeout_override` 不能超过 manifest 上限，否则被夹到上限。

响应体：

```json
{
  "exec_id": "exec_01HXYZ...",
  "exit_code": 0,
  "stdout": "...",
  "stdout_truncated": false,
  "stdout_url": "/skills/output/exec_01HXYZ/__stdout__",
  "stderr": "...",
  "stderr_truncated": false,
  "duration_ms": 1834,
  "outputs": [
    {"path": "result.json", "size": 2048, "url": "/skills/output/exec_01HXYZ/result.json"}
  ]
}
```

- `stdout`/`stderr` 是截断后的可内联文本（默认 `stdout_max_kb` 上限）。
- 完整流通过 `stdout_url` / `stderr_url` 拉，存在服务端 `~/.runai/exec-output/<exec_id>/` 下，TTL 24h。
- `outputs[]` 是容器内 `/output` 目录里 skill 写出的文件，递归列出，URL 用预签名 path 让用户拉。

**GET `/skills/output/{exec_id}/{*path}`**

只允许该 exec 的发起者访问；其他用户 404（不 403，避免泄露存在性）。`__stdout__` / `__stderr__` 是保留路径，分别返回完整流。

### 3.2 podman 命令形状

最小生效命令：

```
podman run --rm \
  --network=<none|bridge> \
  --read-only --read-only-tmpfs \
  --tmpfs /tmp:size=64m,exec \
  --tmpfs /output:size=<output_max_total_mb>m,exec \
  -v <skill_dir>:/skill:ro,Z \
  -v <user_data_dir>:/data:rw,Z \
  --user 65534:65534 \
  --memory=<mem> --memory-swap=<mem> \
  --cpus=<cpus> --pids-limit=<pids> \
  --cap-drop=ALL \
  --security-opt=no-new-privileges \
  --security-opt=seccomp=/etc/runai/seccomp.json \
  --workdir /skill --hostname sandbox \
  <image> timeout <secs> <entrypoint> <args...>
```

参数取值来源：

| 参数 | 来源 |
|---|---|
| `--network` | `skill.exec.network.mode` |
| `--tmpfs /output size` | `skill.exec.resources.output_max_total_mb` |
| `<skill_dir>` | `~/.runai/skills/<name>/` |
| `<user_data_dir>` | `~/.runai/user-data/<user_id>/<skill_name>/` （持久挂载点） |
| `--memory` | `skill.exec.resources.memory` （默认 256m） |
| `--cpus` | `skill.exec.resources.cpu` （默认 1.0） |
| `--pids-limit` | `skill.exec.resources.pids` （默认 64） |
| `<secs>` | `min(timeout_override, skill.exec.resources.timeout)` |
| `<image>` | `skill.exec.image` |
| `<entrypoint>` | `skill.exec.entrypoint` |

`/data` 是用户持久卷。`/skill` 是只读源。`/output` 是 tmpfs，容器退出后由服务端把里面文件拷出到 `~/.runai/exec-output/<exec_id>/` 再 unmount。

### 3.3 skill 目录 manifest 扩展

`~/.runai/skills/<name>/skill.toml`（新增文件，可选；不存在则维持兼容）：

```toml
[skill]
name = "fetch-html"
version = "0.1.0"

[exec]
enabled = true
image = "ghcr.io/crosery/runai-skill-base:py3.12"
entrypoint = "python3 main.py"

[exec.network]
mode = "bridge"          # none | bridge
# allow = ["example.com"]  # V2，V1 忽略

[exec.storage]
persistent_data = true   # 挂 /data
outputs_dir = "/output"  # 容器内写出目录

[exec.resources]
memory = "256m"
cpu = 1.0
timeout = 120            # 秒
pids = 64
output_max_total_mb = 50
stdout_max_kb = 64

[exec.env_allow]
keys = ["LANG", "HTTPS_PROXY"]
```

`[exec.enabled = false]` 或缺失 `[exec]` section → 走旧协议 `/skills/get` 返回 SKILL.md。

### 3.4 V1 显式范围

- `network` 只允许 `none` / `bridge` 二值。egress DNS 白名单（V2）。
- 容器池（V2）：V1 每次 exec 都 cold start，可接受。
- skill 镜像构建：V1 由管理员手工 `podman build -t runai-skill/<name>:<ver>`，写入 `skill.exec.image`；自动构建（V2）。
- 没有跨容器 IPC、没有 GPU passthrough、没有 host PID/network namespace。

---

## 4. 多用户身份模型

### 4.1 身份文件

位置：`~/.runai-identity`（不在 `~/.runai/` 内，目的是 uninstall 不删，重装保留）。

权限：`0600`。

内容：

```json
{
  "secret": "rnai_live_<43字符 base32>",
  "user_id": "u_<10字符 base32>",
  "server_url": "https://runai.example.com",
  "created_at": 1731974400
}
```

派生规则：

- `secret`：256-bit cryptographic random → base32（去掉 padding）→ 加 `rnai_live_` 前缀。
- `user_id`：`"u_" + base32(sha256(secret))[:10]`，全局可推导，方便 admin 用日志里看到的 user_id 反查。
- `api_key`：等同 `secret`，HTTP 头 `Authorization: Bearer <secret>`。
- 服务端只存 `sha256(secret)`，从不存明文。

### 4.2 install / uninstall 处理

- `install.sh` / `install.ps1` 第一次运行：检测无 `~/.runai-identity` → 生成 → POST `/users/register`。
- 已有 identity：复用，POST `/users/register`（服务端幂等：已存在 → 200，记录最近一次 IP）。
- `uninstall.sh` / `uninstall.ps1` 默认保留 identity。
- `uninstall --purge-identity` 显式删除（提示 “删后无法恢复，跨设备同步前请先 `runai identity export`”）。

### 4.3 多设备同步

V1 手工：

- `runai identity export` → stdout 打印 base64(json)
- `runai identity import <base64>` → 写文件，覆盖前若已有则要求 `--force`

server-side device list：每次 register 记录 `(user_id, ip, ua, last_seen)`，dashboard `身份` 面板可见，不做强制踢出。

### 4.4 身份丢失语义

V1：身份丢失 = 数据丢失。文档明示。admin 可手动 `runai user migrate --from <old_uid> --to <new_uid>` 把旧 user_id 的 telemetry / user_skill_prefs / user-data 卷整体迁到新身份。无自动恢复。

---

## 5. 管理员引导与服务端启动门禁

### 5.1 `runai admin bootstrap`

行为：

1. 检查 DB `users` 表是否存在任意 `is_admin=1` 行 → 有则 `bail!("admin already exists, use runai user promote-admin if you mean to add another")`。
2. 生成 admin identity（同 §4.1 格式）写到运行 bootstrap 的当前 home 的 `~/.runai-identity`。
3. 服务端 DB `INSERT INTO users (..., is_admin=1)`。
4. stdout 输出 server_url + user_id + secret，提示 “这是唯一一次显示 secret，请记下”。

### 5.2 `runai server` 启动门禁

启动顺序：

1. 初始化 DB / 跑 migration。
2. `SELECT COUNT(*) FROM users WHERE is_admin = 1`。结果 0 → 写 stderr `"no admin configured, run `runai admin bootstrap` first"` 退出非 0。
3. `doctor::server_preflight()`：
   - `which podman` → 没有 fail。
   - cgroup v2 检测（读 `/sys/fs/cgroup/cgroup.controllers`） → 没有 fail。
   - subuid 配置（`/etc/subuid` 含当前用户） → 没有 warn（rootless 必须，非 rootless 跑可继续）。
   - seccomp profile 存在（`/etc/runai/seccomp.json`） → 没有 fail。
4. 通过 → 绑端口、`println!("runai dashboard at ...")`。

V0.12 起，`runai server` 在没有 admin 的机器上不允许跑。这是不可绕过的门禁。

---

## 6. 用户自注册

### 6.1 默认开放注册

服务端启动参数 `--require-invite=false`（默认）。

`POST /users/register`，请求 `Authorization: Bearer <secret>`，body 可空：

- 服务端从 secret 派生 user_id 和 api_key_hash。
- 已存在该 user_id 行 → 200，更新 `last_seen`。
- 不存在 → INSERT，`is_admin=0`，`prefs_json='{}'`，配额取默认值。
- 同 IP 5 次/小时上限（token bucket，命中 → 429）。

### 6.2 邀请制（可选）

启动参数 `--require-invite=true`：

- `POST /users/register?invite=<token>`：必须带 invite。
- 服务端验证 invite 未过期、未使用 → 标记 `used_by=<user_id>`, `used_at=<now>`。
- 缺 invite / invite 无效 → 403。

`runai invite create [--ttl 7d] [--note <text>]` → 生成 32-char base32 token，INSERT `invites`，stdout 打印。

`runai invite list` / `runai invite revoke <token>`。

### 6.3 限速

不管是否 invite-only，`/users/register`、`/auth/login`、`/auth/magic-link` 全部走每 IP 5 次/小时。命中 429 + `Retry-After` 头。

---

## 7. dashboard 鉴权与多用户视图

### 7.1 cookie session

- cookie 名：`runai_session`，`HttpOnly; SameSite=Lax; Secure (生产)`，TTL 30 天。
- 内容：opaque token，DB 映射到 user_id。
- 登录页 `/login`：粘贴 api_key → POST `/auth/login` body `{key: ...}` → 服务端验 hash → 写 cookie。

### 7.2 magic link

- install 脚本注册成功后 POST `/auth/magic-link` Bearer，服务端生成 32-char token（TTL 1h，单次），INSERT `magic_links`。
- 脚本 stdout 打印 `Dashboard: https://<server>/login?magic=<token>`。
- 用户点链接：GET `/login?magic=<token>` → 验证 → set cookie → redirect `/`。

### 7.3 鉴权中间件

- 所有 `/api/*`、`/skills/exec/*`、`/skills/output/*`、`/recommend`、`/skills/get/*`、`/feedback` 强制鉴权。
- `/login`、`/install`、`/install.ps1`、`/uninstall`、`/uninstall.ps1`、`/auth/*`、`/users/register` 公开。
- 优先级：`Authorization: Bearer` 头 > cookie session > 未鉴权。
- 未鉴权访问 `/api/*` → 401。访问 HTML 路由 → 302 `/login`。

### 7.4 admin 全局视图

- admin 用户的 cookie session 标记 `is_admin=true`。
- dashboard 顶栏出现 `Global view` toggle。开启 → 所有 `/api/*` 加 `?scope=global` → 服务端跳过 user_id filter。
- 非 admin 看不到这个 toggle，强行带 `?scope=global` → 403。
- `RUNAI_BOOTSTRAP_ADMIN=<api_key_hash>` env：服务端启动时如果该 hash 对应行不存在 → 自动 INSERT is_admin=1。用于 docker / k8s 部署场景，避免每次起容器都要 `admin bootstrap`。

---

## 8. 用户偏好（hook 行为可配置）

### 8.1 数据形态

`users.prefs_json TEXT NOT NULL DEFAULT '{}'`。整列 JSON 存，避免每加一个开关都做 schema migration。

V1 schema：

```rust
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct UserPrefs {
    #[serde(default = "default_true")] pub show_tradeoff: bool,
    #[serde(default = "default_true")] pub show_session_history: bool,
    #[serde(default = "default_true")] pub show_feedback_protocol: bool,
    #[serde(default)] pub recommend_mode: RecommendMode,
    #[serde(default = "default_candidate_limit")] pub candidate_limit: u8,
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "snake_case")]
pub enum RecommendMode { #[default] Compatible, Exclusive, Off }
```

- `candidate_limit` 取值 [1, 5]，越界服务端夹断。
- `recommend_mode=off`：`/recommend` 直接返回空 hook（`{"chosen":[]}`），不调 LLM、不写 router_events。
- 缺字段 → 用 default。旧 user 自动等同全开。

### 8.2 hook 模板改造

`src/core/prompts/hook_output.md` 已有 `{SESSION_HISTORY_BLOCK}` `{FEEDBACK_PROTOCOL_BLOCK}` 占位符。本次新增 `{TRADEOFF_BLOCK}` 占位符替换当前硬编码的 “回复开头交代取舍” 段。

新模板形状（仅示意，最终以代码 PR 为准）：

```
runai 推荐 (mode={MODE})

{REASONING_BLOCK}{TRADEOFF_BLOCK}候选：

{CANDIDATES_BLOCK}

激活方式：每个 skill 跑一次 Bash

  curl -s -X POST '{SERVER_URL}/skills/exec/<skill_name>' -H 'Authorization: Bearer <client_key>'

stdout 是 skill 执行结果 JSON。按 result 内容继续执行用户原 prompt。

{ACTIVATION_DIRECTIVE}

{SESSION_HISTORY_BLOCK}{FEEDBACK_PROTOCOL_BLOCK}
```

### 8.3 render_hook_output 改造

签名变为：

```rust
pub fn render_hook_output(
    skills: &[Recommendation],
    decision: &RouteDecision,
    session_history: &[String],
    session_id: &str,
    server_url: &str,
    user_header: &str,
    prefs: &UserPrefs,           // 新增
) -> String
```

实现：

- `prefs.show_tradeoff=false` → `tradeoff_block = String::new()`
- `prefs.show_session_history=false` → `session_history_block = String::new()`
- `prefs.show_feedback_protocol=false` → `feedback_protocol_block = String::new()`
- `prefs.recommend_mode=Off` → 在更上层短路，不调 `render_hook_output`。

### 8.4 客户端 Bearer 注入

`scripts/runai-client-hook`（客户端的 UserPromptSubmit hook wrapper）改成：

```bash
#!/usr/bin/env bash
set -euo pipefail
KEY=$(jq -r .secret ~/.runai-identity 2>/dev/null || true)
[ -z "$KEY" ] && exit 0
URL=$(jq -r .server_url ~/.runai-identity)
exec curl -sS -X POST "$URL/recommend" \
  -H "Authorization: Bearer $KEY" \
  -H "Content-Type: application/json" \
  --data-binary @-
```

hook stdout 中所有 `curl ... '{SERVER_URL}/...'` 都改成 `-H 'Authorization: Bearer $KEY'`，`$KEY` 由 hook wrapper 在向 agent 输出前展开为实际值。

---

## 9. 每用户 skill 可见性

### 9.1 V1 黑名单模型

默认：所有服务端注册的 skill 对所有 user 可见。

用户在 dashboard `Skill 库` 面板里关掉某个 skill → INSERT `user_skill_prefs(user_id, skill_name, hidden_at=now)`。

撤销隐藏 → DELETE 行。

### 9.2 接口过滤

- `/recommend`：候选池构造前，从全量 skill 列表中剔除 `user_skill_prefs.hidden_at IS NOT NULL` 的项。
- `/skills/get/{name}`：目标 skill 在用户黑名单 → 404（不 403），错误体 `{"error":"not found"}`。
- `/skills/exec/{name}`：同上。
- 不泄露 “该 skill 存在但你看不到” 的信号。

### 9.3 V2 占位

V2 引入 `users/<id>/skills/` 用户私有上传 + 镜像构建。V1 不做。

---

## 10. dashboard 视觉重设计

### 10.1 信息架构

```
[ top bar: brand | user_id | theme | logout ]
[ sidebar ]                          [ main view ]
- Overview
- Skills
- Activity
- Settings
- Admin   (仅 admin 可见)
```

- 左侧栏固定宽度（220px），sticky。
- 顶栏 user_id 点开下拉显示 server_url、最近 IP、登出。
- hash router：`#/overview` `#/skills` `#/activity` `#/settings` `#/admin`，默认 `#/overview`。

### 10.2 三套视觉方向

放 `docs/specs/ui-mocks/` 下让 Crosery 挑：

| 方向 | 风格 | 适合场景 |
|---|---|---|
| Linear-style | 单色调、留白多、字体克制 | 内部工具，干净 |
| Vercel-style | 卡片密集、accent 多色、动态过渡 | 对外 SaaS 感 |
| HTOP-style | 等宽字体、ascii box、密度极高 | 终端原生玩家 |

V1 落地选 1 套，剩下 2 套留 mock 不实装。Linear-style 是默认推荐方向，理由：与现有 router dashboard 美学最接近，迁移成本最低。

### 10.3 Settings 面板详细分区

**Hook 行为**

- toggle `show_tradeoff`（label：回复开头交代取舍）
- toggle `show_session_history`（label：注入本 session 已看过的 skill 列表）
- toggle `show_feedback_protocol`（label：注入正负向反馈协议）
- select `recommend_mode`：Compatible / Exclusive / Off
- slider `candidate_limit`：1 ~ 5

**Skill 库**

- 搜索框 + 全量 skill 列表
- 每行：name | description（截断） | 用量计数 | 可见性 toggle
- 默认全可见，关 toggle 即写 blacklist。

**配额 + 用量**

4 个 progress bar：

- exec/min 用量（当前桶 vs 上限）
- /data 占用（MB / 上限）
- /output 占用（MB / 上限）
- 并发 exec 数（当前 / 上限）

进度条颜色：< 60% 绿、60%–85% 黄、> 85% 红。

**身份**

- user_id（点击复制）
- API key 查看（两步：先点 reveal 按钮，再确认 “我知道泄露后果”）
- rotate API key（生成新 secret，老 secret 立即失效，要求用户立即 import 新 identity）
- 导出 identity（base64 复制到剪贴板）
- 最近 IP 列表（5 条）

**Admin（仅 admin 可见）**

- 用户列表（user_id、is_admin、disabled、created_at、最近 IP），inline 编辑配额
- 邀请 token 生成器 + token 列表（状态：未用 / 已用 by user_id / 已撤销 / 已过期）
- 全局审计日志查询（exec_audit）：按 user_id / skill / 时间窗 过滤

### 10.4 交互细节

- 所有 toggle / slider / select 自动保存：失焦或值变化后 300ms debounce → `PATCH /api/prefs`。
- 保存成功 → 右下角 toast “已保存”，1.5s 自动消失。失败 → toast 红色 + 错误文本。
- 不出现 Save 按钮。
- 前端继续用纯 vanilla JS + hash router。不引入 React / Vue / 任何 npm 依赖。runai 单二进制无外部依赖的产品哲学不变。

---

## 11. 数据库 schema 新增项

迁移版本：v0.12 = schema v14（当前 v13）。在 `Database::migrate()` 里加 v14 step，幂等。

```sql
CREATE TABLE users (
  user_id TEXT PRIMARY KEY,
  api_key_hash TEXT NOT NULL,
  is_admin INTEGER NOT NULL DEFAULT 0,
  prefs_json TEXT NOT NULL DEFAULT '{}',
  quota_exec_per_min INTEGER NOT NULL DEFAULT 60,
  quota_data_mb INTEGER NOT NULL DEFAULT 1024,
  quota_output_mb INTEGER NOT NULL DEFAULT 500,
  quota_concurrent_exec INTEGER NOT NULL DEFAULT 4,
  disabled INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX idx_users_api_key_hash ON users(api_key_hash);

CREATE TABLE user_skill_prefs (
  user_id TEXT NOT NULL,
  skill_name TEXT NOT NULL,
  hidden_at INTEGER,
  PRIMARY KEY(user_id, skill_name)
);

CREATE TABLE invites (
  token TEXT PRIMARY KEY,
  created_by TEXT NOT NULL,
  used_by TEXT,
  expires_at INTEGER,
  used_at INTEGER
);

CREATE TABLE exec_audit (
  exec_id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  skill_name TEXT NOT NULL,
  entrypoint TEXT,
  args_json TEXT,
  exit_code INTEGER,
  duration_ms INTEGER,
  stdout_bytes INTEGER,
  stderr_bytes INTEGER,
  outputs_count INTEGER,
  outputs_total_bytes INTEGER,
  ts INTEGER NOT NULL,
  FOREIGN KEY(user_id) REFERENCES users(user_id)
);
CREATE INDEX idx_audit_user_ts ON exec_audit(user_id, ts);
CREATE INDEX idx_audit_skill_ts ON exec_audit(skill_name, ts);

CREATE TABLE magic_links (
  token TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at INTEGER,
  FOREIGN KEY(user_id) REFERENCES users(user_id)
);

CREATE TABLE sandbox_data_quota (
  user_id TEXT NOT NULL,
  scope TEXT NOT NULL,  -- 'data' | 'output'
  bytes_used INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(user_id, scope)
);
```

迁移 v13 → v14 的回填：

- `router_events`、`router_feedback` 等历史表新增 `user_id TEXT DEFAULT NULL` 列，旧行保持 NULL。
- 不删除任何旧表 / 旧列。前向迁移单向，不可逆。

---

## 12. 改动范围

### 12.1 新增文件

| Path | Type | 内容 |
|---|---|---|
| `src/core/sandbox.rs` | 新 | podman 命令构造（参数表 → `Command`）、执行（spawn、wait、收 stdout/stderr）、output 目录回收、limits 解析 |
| `src/core/exec.rs` | 新 | exec 编排：parse manifest → 配额检查 → 调用 sandbox → 写 exec_audit → 包装响应 |
| `src/core/identity.rs` | 新 | 身份文件生成、读、export base64、import 校验、`derive_user_id(secret)` 工具函数 |
| `src/core/auth.rs` | 新 | Bearer 解析、`hash_api_key(secret) -> String`（sha256 hex）、session 中间件、admin 中间件、magic link 校验 |
| `src/core/prefs.rs` | 新 | UserPrefs 结构 + serde + DB 读写 helper（`load_prefs(user_id)` / `save_prefs(user_id, prefs)`） |
| `src/core/quota.rs` | 新 | 内存 token bucket（per-user-per-route），磁盘存量统计（`sandbox_data_quota` 表，定期 scan `~/.runai/user-data/<uid>` 累加） |
| `src/cli/user.rs` | 新 | `runai user list / disable / enable / set-quota / promote-admin / migrate` |
| `src/cli/invite.rs` | 新 | `runai invite create / list / revoke` |
| `src/cli/identity_cli.rs` | 新 | `runai identity show / export / import / rotate` |
| `src/cli/admin.rs` | 新 | `runai admin bootstrap` |
| `scripts/runai-client-hook` | 新 | bash wrapper：从 `~/.runai-identity` 注 Bearer 后转发到 `/recommend` |
| `src/core/seccomp_profile.json` | 新 | 默认 seccomp 白名单（embed via `include_str!`，启动时 dump 到 `/etc/runai/seccomp.json` 如不存在） |
| `deploy/systemd/runai.service` | 新 | systemd unit 样例（After=network.target，ExecStart=runai server，Restart=on-failure） |
| `deploy/nginx-example.conf` | 新 | TLS termination + reverse proxy 到 `127.0.0.1:8080` |
| `docs/specs/ui-mocks/linear.html` | 新 | mock 1 |
| `docs/specs/ui-mocks/vercel.html` | 新 | mock 2 |
| `docs/specs/ui-mocks/htop.html` | 新 | mock 3 |

### 12.2 修改文件

下表 “高风险” = AGENTS.md 安全契约要求物理 e2e 才能合并的文件。

| Path | Type | 改动 | 风险 |
|---|---|---|---|
| `src/core/recommend.rs` | 改 | `render_hook_output` 加 `prefs: &UserPrefs` 参数，hook 模板内 `curl` 头由 `X-Runai-User` 改 `Authorization: Bearer`；`/recommend` 调用入口先 load 用户 prefs |  |
| `src/core/prompts/hook_output.md` | 改 | 新增 `{TRADEOFF_BLOCK}` 占位符；激活方式 curl 改为 `/skills/exec` + Bearer |  |
| `src/core/db.rs` | 改 | schema v14 migration、users / user_skill_prefs / invites / exec_audit / magic_links / sandbox_data_quota 全套 CRUD、router_events 新增 user_id 列回填 NULL | 高风险（用户元数据，迁移单向） |
| `src/server.rs` | 改 | 新路由：`/skills/exec/{name}` `/skills/output/{exec_id}/{*path}` `/users/register` `/auth/login` `/auth/logout` `/auth/magic-link` `/api/prefs` `/api/quota/usage` `/api/identity/*` `/api/admin/*`；auth + admin 中间件；`handle_skill_get` 在 manifest 有 `[exec]` 时返回 “请改用 /skills/exec” 而不是 SKILL.md |  |
| `src/core/scanner.rs` | 改 | 解析 skill 目录的 `skill.toml`，填 `Skill.exec: Option<ExecConfig>` | 高风险（scanner 是 4-27 事故的核心模块） |
| `src/core/installer.rs` | 改 | 安装时若 repo 含 `Dockerfile` 且 manifest 显式 `[exec.build] auto=true`，跑 `podman build`（V1 默认 off，只读 manifest 不构建） | 高风险（podman build 跑任意 Dockerfile） |
| `src/core/resource.rs` | 改 | `Skill` 加 `pub exec: Option<ExecConfig>`，新增 `pub struct ExecConfig { image, entrypoint, network, resources, ... }` |  |
| `src/mcp/tools.rs` | 改 | 新增 `sm_exec(skill, args, stdin?, timeout?)` 映射到 HTTP `/skills/exec`；MCP 调用方也走 Bearer（MCP server 启动时从 `~/.runai-identity` 读 secret） |  |
| `src/core/doctor.rs` | 改 | 加 `server_preflight()`：podman / cgroup v2 / subuid / seccomp 文件 |  |
| `src/cli/mod.rs` | 改 | 注册 user / invite / identity_cli / admin 四组新子命令；`runai server` 启动前调 `auth::ensure_admin_exists()` |  |
| `scripts/runai-client-install.sh` | 改 | identity 生成或复用 → POST `/users/register` → POST `/auth/magic-link` → 打印 dashboard 链接 |  |
| `scripts/runai-client-install.ps1` | 改 | 同上，Windows 版 |  |
| `scripts/runai-client-uninstall.sh` | 改 | 加 `--purge-identity` flag |  |
| `scripts/runai-client-uninstall.ps1` | 改 | 同上 |  |
| `web/index.html` | 改 | 整体重构为 sidebar + main 布局，加 login / settings / admin / skills / activity 视图 |  |
| `web/app.js` | 改 | hash router、auth fetch wrapper（401 → 跳 /login）、prefs 自动保存、admin 全局开关 |  |
| `web/app.css` | 改 | 按选定视觉方向重写 |  |
| `AGENTS.md` | 改 | 加 “Identity & Multi-user”、“Skill Exec Model”、“Admin Bootstrap” 三节；module index 新增 sandbox / exec / identity / auth / prefs / quota 行；exec.rs / sandbox.rs / installer.rs Dockerfile 路径列入高危改动名单 |  |
| `README.md` | 改 | 多用户部署一节：bootstrap、注册、客户端安装、dashboard 登录 |  |
| `README_zh.md` | 改 | 同上中文版 |  |
| `Cargo.toml` | 改 | bump 到 0.12.0；可能新增 `base32`、`sha2`（如未已传递依赖）、`tower-cookies` 等 |  |

### 12.3 影响调用链

- `recommend::recommend_for_request` → 入参加 `user_id` → `db::load_prefs` → `render_hook_output(..., &prefs)`
- `mcp::tools::sm_*` 全部经 `auth::current_user(req)` 取出 user_id，再访问 DB 过滤
- `handle_skill_get` 现有 “返回 SKILL.md + 同级 reference 列表” 行为继续支持，但 manifest 有 `[exec]` 时 stdout 改为：

  ```
  此 skill 走 sandbox exec 模型。
  调用：curl -X POST '<server>/skills/exec/<name>' -H 'Authorization: Bearer <key>' -d '<body>'
  ```

  附文档链接到 dashboard 的 skill 详情页。

---

## 13. 与 AGENTS.md 安全契约对齐

### 13.1 触发的高危改动

按 AGENTS.md “5 条铁律”，本次改动触发以下高危分类：

- **DB schema migration**：新增 6 张表 + 旧表新增列。属于 “修改 runai.db 中影响文件系统的字段” 的相邻分类（影响用户数据完整性、单向迁移），按高危处理。
- **scanner.rs 改动**：4-27 事故的核心模块，新增 `skill.toml` 解析逻辑必须保证不动 `~/.runai/skills/` 真实目录。
- **installer.rs 改动**：新增 `podman build` 路径会执行任意 Dockerfile，本身就是 RCE 等价物。V1 强制 `auto=false` 默认，admin 显式 opt-in 才跑。
- **sandbox.rs / exec.rs**：在服务端跑用户提交的命令，逃逸 = 用户互相访问。
- **identity 文件管理**：用户私产，路径在 `~/.runai-identity`，删错 = 永久身份丢失。

### 13.2 必做的物理 e2e（合并前不允许跳）

按 AGENTS.md 铁律 2 的物理 e2e 模板：

- **隔离 HOME**：每个 case `HOME=$(mktemp -d)` + 显式 `RUNE_DATA_DIR`。
- **跨 RUNE_DATA_DIR 双跑**：默认 home 一次，自定义路径一次。验 scanner 加的 `skill.toml` 解析不会因为 `paths::data_dir()` 取错路径而误读用户私产。
- **跨 4 个 CLI target 验证**：和现有 cli_target_symmetry 测试合并；本次主要确认 hook wrapper 的 Bearer 注入在 4 个 target 下都生效。
- **多用户隔离**：起 2 个 user (uA / uB)，uA 的 `/skills/exec` 不能读到 uB 的 `/data` 卷；uA 拉 uB 的 `exec_id` 输出 → 404；uA 调 admin 接口 → 403。
- **沙箱逃逸 case**：skill 入口尝试 `mount` / `mknod` / `unshare` / 写 `/host` → seccomp 拒绝，container 非 0 退出；尝试在 `/output` 写 100MB 文件 → tmpfs ENOSPC，exec 报错；尝试 fork bomb → `--pids-limit` 阻断。
- **DB 迁移幂等**：v13 库跑 migrate 一遍 → v14；再跑一遍 → no-op；rollback 到 v13 备份 → 再 migrate → 结果等价。

物理 e2e 清单要写进合并 PR 的描述里，按 “跑了哪些命令、检查了哪些路径不动 / 哪些路径正确改变” 逐条列。无清单 = PR 未 ready。

### 13.3 / skills/get 与 / skills/exec 的泄漏防御

合并前必须验：

- 用户 A 隐藏了 skill X，A 调 `/skills/get/X` → 404；A 调 `/skills/exec/X` → 404。
- 用户 B 没隐藏 X，B 调 → 正常 200。
- 错误体形状一致，不靠 status code、body length、timing 暴露 X 存在与否。

### 13.4 身份文件丢失的明示

`install.sh` stdout 在生成新 identity 那一刻打印（醒目）：

```
新身份已生成：u_xxxxx
身份文件：~/.runai-identity（权限 0600）
请立即备份：
  runai identity export > runai-identity.b64
丢失 = 永久无法找回该 user 的数据。
```

---

## 14. 待用户拍板的开放问题

| 编号 | 问题 | V1 默认建议 |
|---|---|---|
| Q1 | dashboard 视觉方向选哪套 | Linear-style |
| Q2 | admin 是否可设服务端默认 prefs（新用户继承） | 是；admin 写 `server_defaults` key 到 DB 配置表 |
| Q3 | `recommend_mode=off` 是返回空 hook 还是 200 + 一行文案告知用户已关闭 | 完全静默（空响应） |
| Q4 | skill 私有上传（用户上传到自己 namespace） | V2，V1 不做，UI 不出现入口 |
| Q5 | 跨设备身份同步是否上 server-side store（用户输个 passphrase 加密上传） | V2 探索，V1 export/import 手工同步 |
| Q6 | 镜像构建 auto=true 是否允许（即 install 一个 skill 时自动跑 `podman build`） | V1 关闭，admin 显式 `runai admin build <skill>` 才跑 |
| Q7 | `/skills/output/<exec_id>` 的 TTL | 24h 删；可在 prefs 加全局覆盖 |
| Q8 | 是否在 hook 输出里继续支持旧 `/skills/get` 协议（无 manifest 的 skill） | 是；V1 兼容期不强行迁移；V2 评估 |

---

## 15. 上线与回退方案

### 15.1 分支

- `main` HEAD 已打 tag `pre-multiuser-redesign`，作为回退锚点。
- 长期分支 `backup/pre-multiuser-redesign` 已切出，禁止合并新 commit。
- 开发分支 `feat/multiuser-server` 从 `main` 切，所有本次改动落这里。
- 合并方式：完成 + 物理 e2e 通过后，PR 回 `main`，挤压合并保留单 commit。

### 15.2 版本

- `Cargo.toml` 0.11.x → 0.12.0（minor，新执行模型属于行为变化，破坏向后兼容）。
- 打 annotated tag `v0.12.0`，tag message 写完整 changelog（GitHub release page 取自 tag message body）。

### 15.3 数据迁移

- 服务端 v0.11 → v0.12：DB schema v13 → v14 自动迁移；旧 `router_events` 行 `user_id=NULL` 视为 legacy；admin dashboard 显示时归入 “未归属” 桶。
- 客户端 hook 旧版本 → 新版本：旧 hook 不带 Bearer 头，服务端 `/recommend` 检测无 Bearer 时返回 200 + 提示文案 “请重装客户端 hook 以启用多用户”（不强行 401，避免老用户立即崩）。

### 15.4 回退

- 任何阶段：`git checkout backup/pre-multiuser-redesign` → 重新发 v0.11.x patch。
- DB 不可逆：v0.12 升级前自动 `~/.runai/backups/pre-v0.12-<ts>.db` snapshot；降级时必须从该 snapshot 恢复，文档明示。
- 文档警示：“v0.12+ 回 v0.11 必须 backup restore，DB schema 单向前进。”

### 15.5 上线开关

- `RUNAI_FEATURE_EXEC=0` env 可关闭服务端 exec 路由（返回 503），保留 `/recommend` + `/skills/get` 旧链路，方便事故时降级。
- `RUNAI_FEATURE_AUTH_STRICT=0` 临时允许未鉴权请求（仅过渡期用，灰度后必须打开严格模式才视为发布完成）。

---

## 附录 A：HTTP 路由总览

| Method | Path | 鉴权 | 说明 |
|---|---|---|---|
| GET | `/` | session | dashboard 主页（无 session 跳 `/login`） |
| GET | `/app.js` `/app.css` | 公开 | 静态资源 |
| GET | `/login` | 公开 | 登录页（接受 `?magic=<token>`） |
| POST | `/auth/login` | 公开 | body `{key}` → set cookie |
| POST | `/auth/magic-link` | Bearer | 生成 magic token |
| POST | `/auth/logout` | session | 清 cookie |
| POST | `/users/register` | Bearer | 自注册 / 续期；可选 `?invite=` |
| GET | `/install` `/install.ps1` `/uninstall` `/uninstall.ps1` | 公开 | 客户端脚本 |
| POST | `/recommend` | Bearer | router 路由（hook 调用） |
| POST | `/skills/get/{name}` | Bearer | 旧协议：返回 SKILL.md（manifest 无 exec 时） |
| POST | `/skills/exec/{name}` | Bearer | 沙箱执行 |
| GET | `/skills/output/{exec_id}/{*path}` | Bearer + scope check | 取 exec 输出文件 |
| POST | `/feedback` | Bearer | 用户反馈记录 |
| GET | `/api/summary` `/api/timeline` `/api/events` `/api/event/{id}` | session | router telemetry（user_id 过滤） |
| GET | `/api/skills` `/api/skill/{name}` `/api/skill/{name}/files` `/api/skill/{name}/file` | session | skill 元数据 |
| GET | `/api/prefs` | session | 取当前用户 prefs |
| PATCH | `/api/prefs` | session | 部分更新（合并 JSON） |
| GET | `/api/quota/usage` | session | 当前配额用量 4 项 |
| GET | `/api/identity` | session | user_id、最近 IP、created_at |
| POST | `/api/identity/rotate` | session | 旋转 secret |
| GET | `/api/admin/users` | session + admin | admin 用户列表 |
| PATCH | `/api/admin/users/{user_id}` | session + admin | 改配额 / disable |
| POST | `/api/admin/invites` | session + admin | 生成 invite |
| GET | `/api/admin/invites` | session + admin | invite 列表 |
| DELETE | `/api/admin/invites/{token}` | session + admin | 撤销 invite |
| GET | `/api/admin/audit` | session + admin | exec_audit 查询 |

## 附录 B：seccomp 默认白名单要点

最小 syscall 集（参考 docker default，加严）：

- 允许：read / write / open / openat / close / stat / fstat / lstat / poll / mmap / mprotect / munmap / brk / rt_sig* / ioctl(限 TCGETS) / execve / wait4 / fork / clone(限 CLONE_THREAD|CLONE_VM|CLONE_SIGHAND|CLONE_FS|CLONE_FILES|CLONE_SYSVSEM) / pipe / dup* / fcntl / getpid / getuid / getgid / readlink / lseek / getcwd / chdir / mkdir / unlink / rename
- 禁止：mount / umount / pivot_root / chroot / unshare / setns / kexec_load / init_module / delete_module / bpf / ptrace / process_vm_* / userfaultfd / perf_event_open / clone3(无限制 flags) / io_uring_setup

profile 文件由 `runai server` 启动时若 `/etc/runai/seccomp.json` 不存在则从 binary 释出。

## 附录 C：客户端 hook 输出新形状（示意）

```
runai 推荐 (mode=compatible)

router 判断：当前 prompt 涉及 HTML 解析，候选 fetch-html 命中度高。

候选：
- fetch-html · web 抓取 + DOM 解析
- markdown-distill · 把网页正文压成 markdown

激活方式：每个 skill 跑一次 Bash

  curl -s -X POST 'https://runai.example.com/skills/exec/fetch-html' \
    -H "Authorization: Bearer $RUNAI_KEY" \
    -H 'Content-Type: application/json' \
    -d '{"args":["--url","https://..."],"timeout_override":60}'

stdout 是 skill 执行结果 JSON。按 result.stdout / result.outputs 内容继续执行用户原 prompt。

激活后回复首行写 `激活 skill: fetch-html`，再按结果执行。

本 session runai 已经看过的 skill：markdown-distill, summarize
反馈协议（被动）：用户明确正向或负向评价时，在回复末尾跑：
  curl -s -X POST 'https://runai.example.com/feedback' \
    -H "Authorization: Bearer $RUNAI_KEY" \
    -H 'Content-Type: application/json' \
    -d '{"skill":"<skill-name>","note":"<场景或原话>"}'
```

当用户 prefs 关掉 tradeoff / session history / feedback protocol 时，对应块整段消失，不留空行或占位文案。

---

文档结束。
