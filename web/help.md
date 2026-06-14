# runai 帮助手册

## 怎么从 Market 安装 skill

仪表板 Market 标签提供两条安装路径：skills.sh 聚合榜单（默认数据源，含 All Time / Trending / Hot 三个排序）和「+ GitHub」自助粘贴流。无论走哪条路径，安装下来的 skill 都会落进**当前登录用户的私有池**（`<data>/users/<uid>/skills/<name>/`），并自动订阅到「我的库」，后台立刻触发 enrich 生成 BM25 摘要。

仅在「我的库」可见，不会污染他人或公共池；同名 skill 在不同账号下可以独立存在。

### 步骤

1. 顶栏点 Market 标签（或按 `m`）。
2. 浏览 skills.sh 榜单，或在搜索框粘贴 `owner/repo`；也可点右上角「+ GitHub」走粘贴流。
3. 点击行内「安装到我的库」按钮（无需进详情）；走粘贴流时先点「解析」，再勾选目标 skill，点「导入选中 N 个」。
4. 回 Library 标签确认 skill 出现，摘要从「加载中」变成正文即代表 enrich 完成。

### CLI 命令

```bash
# 远程客户端（无 binary）：先装 hook + identity
curl -fsSL http://<server>:<port>/install | bash

# 本机有 runai binary：装到公共池（不是私有池）
runai install owner/repo
runai install owner/repo@branch
runai market-install <skill-name> --source skills.sh
```

### 常见坑

- 仪表板装 = 私有池；CLI `runai install` / `runai market-install` = 公共池，两者不可互换。
- 点击「安装」后 UI 立刻显示「在库」，是乐观更新；真实结果以 Library 列表为准，建议刷新页面确认。
- 「解析」GitHub 仓库后提示「没找到任何 skill」，多半是仓库无 SKILL.md 或分支不对，用 `owner/repo@branch` 指定分支。
- GFW 网络环境装失败，设 `RUNAI_GH_MIRROR=raw` 跳过 jsdelivr / ghfast.top，直连 raw.githubusercontent。
- GitHub API 60 req/h 限流，挂 `GITHUB_TOKEN` 升到 5000 req/h。

---

## 安装失败怎么办

安装失败最高频的两类错误：（1）「none of the selected skill names matched anything in owner/repo」——你勾选的 skill 名在仓库实际目录里找不到；（2）SKILL.md 全部镜像 404 级联——4 个 CDN 镜像（raw.githubusercontent / ghfast.top / jsdelivr / jsdmirror）全部返回 404。两者根因都是「runai 探测的 4 种 SKILL.md 路径布局」与仓库实际结构不匹配。

runai 默认探测 `<name>/SKILL.md`、`skills/<name>/SKILL.md`、`agent-skills/<name>/SKILL.md`、根目录 `SKILL.md`；用自定义前缀（例如 `skills-catalog/<name>/`）的仓库会全部 miss。

### 步骤

1. 打开浏览器控制台（F12 → Network），重试一次，看真实 HTTP 状态码和返回 body。
2. 验证仓库可达：`curl -I https://raw.githubusercontent.com/<owner>/<repo>/main/SKILL.md`，应为 200。
3. 在 Settings 标签点「刷新市场源」清掉过期市场缓存，再重新发起安装。
4. 改走「+ GitHub」粘贴流而非 Market 行内按钮，强制重新拉一次 git tree。
5. 切换镜像：设 `RUNAI_GH_MIRROR=raw` 跳过 CDN；若仓库私有，挂 `GITHUB_TOKEN`。
6. 上述都不行 → 检查仓库目录布局是否落在 4 种探测路径之外，若是则需仓库方调整结构，或向 runai 维护者反馈加 SourceEntry。

### CLI 命令

```bash
# 强制走 raw.githubusercontent（跳过 CDN 缓存）
RUNAI_GH_MIRROR=raw runai install owner/repo

# 直连验证 SKILL.md 是否存在
curl -I https://raw.githubusercontent.com/owner/repo/main/SKILL.md

# 私有仓库或限流时挂 token
GITHUB_TOKEN=ghp_xxx runai install owner/repo

# 刷新市场缓存
runai market --search <skill-name>
```

### 常见坑

- 「none matched」错误 = 仓库布局不符合 4 种默认探测路径，不是网络问题。
- 4 个镜像全 404 ≠ 仓库不存在；先 `curl -I` raw.githubusercontent 排除。
- 市场缓存过期会让 skill 名指向已删除的目录，先「刷新市场源」再装。
- GitHub API 返回 404 也可能是无 token + 限流，挂 `GITHUB_TOKEN` 再试。
- 不要重复点「安装」按钮，每次失败都会重新跑一遍 enrich，浪费 LLM 配额。

---

## 怎么从「我的库」移除 skill (跟「彻底删除」的区别)

「我的库移出」和「彻底删除」是**两个完全不同的操作**：

- **移出我的库**（普通用户）= 取消订阅，skill 本体仍在公共池或他人的私有池中存在，随时可重新加入。改的只是 `user_skill_library` 表。
- **移到垃圾桶**（管理员）= soft delete，skill 目录被移进 trash，但保留 group 成员、enabled targets、payload，可用 `runai trash restore` 恢复。
- **彻底删除**（任何持有 runai binary 的用户）= 永久 purge trash 条目，删除磁盘上的 payload，不可恢复。

仪表板上普通用户只能看到「移出我的库」按钮；管理员在选中模式下还能看到红色的「移到垃圾桶」。彻底删除目前只走 CLI。

### 步骤

1. **移出我的库**：Library 标签 → 进入选中模式 → 勾选要移出的 skill → 点「移出我的库」。
2. **移到垃圾桶**（仅管理员）：选中模式下勾选公共池 skill → 点红色「移到垃圾桶」→ 确认对话框。
3. **查看垃圾桶**：`runai trash list`，按 `deleted_at DESC` 列出所有 trash 条目。
4. **恢复**：`runai trash restore <name>`，回到原目录（公共/私有都正确），重建 symlink 与 group 成员。
5. **彻底删除**：`runai trash purge <name>` 单条，或 `runai trash empty` 清空整个垃圾桶。

### CLI 命令

```bash
runai trash list
runai trash restore <name-or-trash-id>
runai trash purge <name-or-trash-id>
runai trash empty
runai uninstall <name>     # 直接进垃圾桶，等同 soft delete
```

### 常见坑

- 「移出我的库」≠ 删除 skill 本体；想重装直接回 Market 再点一次「安装到我的库」即可。
- `runai uninstall` 是 soft delete，落地在 trash，不立刻释放磁盘空间；想永久清理跑 `runai trash purge` 或 `runai trash empty`。
- 仪表板的「移到垃圾桶」按钮只在管理员 + 选中模式 + 选中集非空时显示，普通用户看不到是正常的。
- `runai trash restore` 报 `resource already exists` = 同名 skill 已在 DB 或磁盘存在；先 uninstall 当前那个再 restore。
- MCP trash 条目无 `payload_path`（状态存 config 里），purge 时直接跳过文件操作，不是 bug。

---

## 怎么把自己的 skill 上传到社区市场

team 模式下，所有已认证用户都能上传 skill 到社区池；owner 模式下 `/api/community/*` 全部返回 404，没有此入口。上传内容存在 `<data>/community/<uploader_uid>/<name>/`，DB 主键 `(uploader_uid, name)`，同名同上传者再传一次 = 版本号自增（Unix 时间戳），`installs_total` 和 `created_at` 保留。

上传后其他用户在 Market 标签的「社区」子 tab 看得到，点「安装」会复制到他们自己的私有池，互不影响。

仪表板不做上传 UI，所有上传都走 CLI：服务端管理员用 `runai community upload`，远程客户端（无 binary）用 install 脚本生成的 `runai-client upload`。

### 步骤

1. 进入仪表板 Market 标签 → 「社区」子 tab → 浏览 / 安装 / 删除自己上传过的 skill。
2. 上传：终端跑 `runai community upload --path <skill-dir> --name <name>`，或 `runai-client upload` 走 fzf 选择器。
3. 安装他人 skill：`runai community install <uploader_uid> <name>`，落到 `~/.runai/users/<我的 uid>/skills/<name>/`。
4. 删除自己上传：`runai community delete <uploader_uid> <name>`，仅 uploader 或管理员有权。

### CLI 命令

```bash
# 本机有 runai binary
runai community upload --path ./my-skill --name my-skill
runai community list --sort installs
runai community install <uploader_uid> <skill-name>
runai community delete <uploader_uid> <skill-name>

# 远程用户（先跑 install 脚本装 runai-client）
curl -fsSL http://<server>:<port>/install | bash
runai-client upload                              # TUI 模式，fzf 选 skill
runai-client upload --path ./my-skill --name my-skill   # 非交互
runai-client list --sort created --limit 20
runai-client install <uploader_uid> <skill-name>
```

### 常见坑

- owner 模式 server 不开放社区端点；服务端必须用 `--mode team` 启动。
- 上传需登录态，先 `curl <server>/install | bash` 写好 `~/.runai-identity`。
- skill 名只接受 `[A-Za-z0-9_.-]{1,64}` 且不能以 `.` 开头，否则返回 `400 invalid skill name`。
- 打包必须含 SKILL.md（顶层或同名子目录均可），否则报 `uploaded archive does not contain SKILL.md`。
- 上传 `429` = 单用户每小时上限 10 次，等窗口过去。
- `runai-client upload` TUI 依赖 `fzf`，没装就退回 `--path/--name` 非交互模式。
- 删除只允许 uploader 或管理员，403 是设计，不是 bug。

---

## 我的库 vs 公共池 vs 私有 skill 的区别

| 概念 | 物理位置 | 谁能看到 | 用途 |
|---|---|---|---|
| 公共池 | `<data>/skills/<name>/` | 所有人 | CLI `runai install` 默认落点，本机 `~/.claude/skills/` 有 symlink，可被 recommend 路由直接使用 |
| 私有 skill | `<data>/users/<uid>/skills/<name>/` | 仅 uid 本人（管理员可见全部） | 仪表板 Market 安装、社区安装的落点，不创建本机 symlink，只能通过 `/skills/get` / `/skills/file` / `/skills/bundle` 远程取 |
| 我的库 | `user_skill_library` 表（仅订阅关系） | 仅本人 | 一组「我感兴趣的 skill」订阅列表，可指向公共池或自己的私有 skill；`/recommend` 默认只在这个集合里挑（除非打开 `allow_public_recommend`） |

简言之：**公共池** = 物理共享、所有人可见；**私有 skill** = 物理隔离、按 uid 分目录、跨 user 互不可见；**我的库** = 订阅列表，是一组指针，不持有物理文件。

### 步骤

1. 想让所有人都用上 → 用 CLI 装到公共池：`runai install owner/repo`。
2. 想只给自己用、不污染公共池 → 用仪表板 Market 装，落到私有池。
3. 想要 `/recommend` hook 推荐某些 skill → 加入我的库（仪表板 Library 「加入我的库」按钮，或仪表板 Market 安装时自动订阅）。
4. 想让 hook 在公共池里也能选 → Settings → 我的偏好 → 打开 `allow_public_recommend`。
5. 切换视野：Library 标签的范围条「全部 / 我的库 / 仅公共」切 client-side 过滤；不影响后端候选集。

### CLI 命令

```bash
# 公共池（共享）
runai install owner/repo
runai market-install <name>

# 查看「我的库」（dashboard 是主入口；CLI 暂只列全部资源）
runai list --kind skill

# 我的库范围由 server prefs 控制
# 在 Settings 标签里改 allow_public_recommend，或 POST /api/prefs
```

### 常见坑

- 仪表板装的 skill 不会出现在 `~/.claude/skills/` 的本机 symlink 里——这是设计；本机 Claude Code 用的是公共池 symlink，私有 skill 走 HTTP。
- 「我的库」里看不到某 skill ≠ 它没装；可能只是没订阅，回 Market 点「加入我的库」即可。
- 管理员能在 admin 视图看到所有人的私有 skill，但普通用户互相不可见，这是 owner-aware 过滤的硬契约。
- `/recommend` 默认只看「我的库」，新注册账号会自动预填 top-30 公共 skill 防止冷启动空集；想要全公共池候选必须开 `allow_public_recommend`。
- 同名 skill 在公共池和某 uid 私有池可以并存；该 uid 自己的查询会优先命中私有的（shadow public）。

