# 推荐命中率 + 多维反馈 + 延迟优化 交付说明

分支 `feat/skill-feedback-radar`（基线 `6f13d86`），全部 commit 经 `./scripts/ci-local.sh` 全绿 + 独立 agent 对抗验收。已部署本机：`~/.cargo/bin/runai` 已替换、server 已重启、`runai-client` 已更新、数据库已备份（`~/.runai/runai.db.bak-20260708-phase-feedback`）并迁移到 schema v26。

## 做了什么（6 个 commit + 2 个合并）

| commit | 内容 |
|---|---|
| `8843476` | 数据层：`skill_feedback` 事件表（好评/差评溯源）+ `skill_router_stats` 漏斗统计（进候选/被选/被采纳）+ `skill_metrics` 五轴纯函数模块 |
| `84e7974` | server：`POST /feedback` 支持 `verdict`（±1 / good / bad）；`GET /api/skill/{name}` 返回五轴雷达、全库均值、反馈统计与最近反馈 |
| `862a8b9` | router 命中率核心：排序公式 `bm25×0.35 + llm×0.45 + 反馈×0.20`（零数据中性、库故障降级）；候选行加 `[adopt:NN%]` `[fb:+P/-N]` 真实行为标记；提示词准入校准为"动作形态命中"；harness 系统消息前置门控（零 LLM 调用） |
| `2d0e1b3` | dashboard：skill 详情"反馈画像"手写 SVG 动态雷达（本 skill vs 全库均值，动画插值，5s 轮询刷新）+ 好评/差评按钮；事件弹窗 chosen skill 旁准/不准按钮；BM25 候选改为可点击 chip |
| `f1dceba` | 延迟/token：两阶段 system prompt 字节级固定（吃前缀缓存）；消灭双重注入（Stage-1 user message −92%：778→59 字符）；hook 注入瘦身 −39%；对话历史截断 4 条×250 字符；temperature=0 + max_tokens 收口；移除 session 内不重复推荐 |
| `b9fe4bc` | 反馈闭环：任何反馈（含 agent 的 `runai-client feedback --verdict good\|bad`）异步触发该 skill 重富集，立即标记"富集中"（dashboard 轮询可见），同 skill 富集中时合并不重复烧 LLM |

五轴雷达定义（0-10）：采纳率（被推荐后真被激活）、触发精准（进候选后被选中）、用户口碑（好差评 Laplace 平滑）、摘要质量（llm_score）、使用热度（usage 对数归一）。前两轴来自 router 遥测漏斗——这是此前完全缺失的负反馈信号（"推了但没被用"）。

## 实测（隔离环境 + 真实库快照 + 真实 LLM，12 场景 × 2 轮）

- 应空推场景（闲聊/致谢/harness 消息）：**8/8 正确空推**——准入校准没有放水
- harness 系统消息门控：**19ms / 5ms 返回，零 LLM 调用**（此前这类消息占近 7 天流量约七成，每条烧两次 LLM、平均 4.4s）
- 应命中场景：12/16 命中（含把 playwright 判给浏览器测试这类合理等价命中）；此前必空推的"强化学习深度研究写文档"案例现可命中 deep-research。弱模型在边界领域仍有轮间波动——这正是反馈飞轮要持续收敛的部分：采纳与好差评信号会自动回流进排序
- Stage-1 输入均值 1121 → **108 字符**；固定 system prompt 前缀缓存跨请求命中

## 你怎么用

- dashboard skill 详情页：看雷达、点好评/差评（差评会自动触发重富集，"富集中"标记实时可见）
- Activity 事件详情：BM25 候选 chip 可点；chosen skill 旁直接投准/不准（带 event 归因）
- agent 侧：对推荐说"好用/不对"，宿主会跑 `runai-client feedback <skill> --verdict good|bad --note "..."`
- 逃生门：`RUNAI_FEEDBACK_DISABLED=1` 恢复旧排序权重并隐藏标记

## 上线后排障（当天第二波，均已修复部署）

| 症状 | 根因 | 修复 |
|---|---|---|
| 事件全部双倍记录 | `~/.claude/settings.json` 同时挂了本地直连 `runai recommend` 和远程 `~/.runai-hook.sh` 两个 hook，每条 prompt 双路由 | 立即摘除旧条目（备份 `settings.json.bak-20260708-dedup-hook`）；`66904db` 让两个安装器双向互斥，后装者胜 |
| 页面很卡、雷达每 5 秒从中心重播 | 详情视图轮询走整页重建（文件树/事件表/动画全推倒重画） | `a19ec53` 轮询改轻量路径：数据没变零 DOM 操作，变了只插值多边形和数字；入场展开动画只在点进时播一次 |
| 页面彻底加载不进去（连静态路由都挂） | skills/telemetry 共 8 个 handler 在 tokio 异步线程直接跑同步 SQLite，Overview 轮询+用量面板分页把 worker 池饿死 | `d0515ba` 全部改走 spawn_blocking + 新增 45s TTL 统计缓存（详情轮询不再每次全表扫 router_events） |

第二波的"提示词重复"观感是设计行为：固定 system prompt 每次 4608 token 全部前缀缓存命中（按缓存价计费），未命中部分只是每条 prompt 都不同的候选列表；Stage-2 的 session 记忆已经通过 Stage-1 意图摘要注入。

## 第三波（当天，针对实测反馈，均已修复部署）

| 反馈 | 根因与修复 |
|---|---|
| BM25 硬凑 30 个无关候选（U盘/Ventoy 案例） | `27b3242`：零词法重叠的 skill 禁止靠 llm/反馈先验分进候选；全部截掉时跳过 Stage-2 LLM 调用秒回空推（该案例实测 24→13 候选；真正零命中时 Stage-2 token 归零）。诚实边界：`移动文件`与文件类 skill 是真实词法重叠，纯 BM25 不解语义鸿沟 |
| Stage-2 注入提示词繁杂、要求删"硬上限 3" | `ddfbc9a`：输出格式/数量规则块整体移入固定 system prompt（吃前缀缓存），数字帽删除，保留最小充分集合原则 |
| 长 prompt 头部截断导致意图识别错 | `e8d9756`：截断改头 1/3 + 标记 + 尾 2/3（真实请求几乎总在末尾） |
| 推荐忽然变到 10 秒、页面再度卡顿 | 排序引入的统计查询有 N+1（每次推荐数千次同步往返）；`02cff56` 改两遍单表扫描 + 内存聚合（基准 204→95ms，真实库秒级→百毫秒），缓存改 stale-while-revalidate 后台刷新，请求路径零等待 |

## 遗留（不阻断）

- issue #36：session-history/skip-reminder 死代码读取清理、`format_for_hook_full` 签名收敛、观察 EXCLUSIVE 空推率
- issue #37：library 标签页轮询仍是整行重建（同类问题，涉及筛选/选中状态，需单独重构）
- `AppState::db()` 每请求新开连接并重跑迁移检查（固定开销，未来连接池化）

## 第四波（性能救火，实测反馈驱动，均已部署）

用户实测报"页面卡死/加载不进/推荐变慢/用量面板空/输入被截断",逐条定位根治:

| 症状 | 根因 | 修复 |
|---|---|---|
| 详情页/首页卡死、连静态资源都挂 | skills+telemetry 共 8 个 handler 在 tokio 异步线程直接跑同步 SQLite,Overview 5s 轮询把 worker 池饿死 | 全部改 spawn_blocking + 45s 统计缓存(`d0515ba`) |
| 每请求重开连接重跑 26 个迁移 | `Database::open` 每次无条件 `init_schema` | 迁移每进程每库只跑一次 + 读优化 PRAGMA(16ms→1.4ms),`foreign_keys` 每连接单设(`perf(db)` commit) |
| skill 详情首入 3 秒 | `skill_router_stats` N+1(每 session-skill 对一次 COUNT) | 两遍单表扫 + 内存聚合,缓存 stale-while-revalidate(`02cff56`) |
| /api/summary 首访 17 秒 | 聚合走非覆盖索引、回表读 262MB 宽行 | schema v27 两个覆盖索引,index-only 扫描,17s→0.7s(`7c9dc86`) |
| 模型用量面板空白 | 前端每 5s 分页拉 20 次 /api/events 浏览器聚合 7000 行,高负载超时清空 | per_model 加 avg_latency/hits,前端改用 summary 一个请求(`7c9dc86`) |
| 事件双倍记录 | 本地 hook + 远程 hook 并存双路由 | 安装器双向互斥(`66904db`) |
| agent 间通信也触发推荐、挤占限流 | harness 门控漏了"Another Claude session"前缀 | 补前缀门控(`c4bc780`) |
| 长 prompt 被头部截断丢意图 | 只留头 2000 字符 | 头 1/3 + 尾 2/3 双保留(`e8d9756`) |
| Stage-2 冗余吞用户原文 | 两波职责未分清 | Stage-2 只用意图摘要,Stage-1 预算 2000→4000(`18e6376`) |
| BM25 硬凑无关候选 | 零重叠 skill 靠先验分垫入 | 相关性截断 + 全无关跳过 Stage-2(`27b3242`) |

recommend 延迟的底是主力免费模型 poolside 两次串行调用(平均 ~10s,实测部分请求已降到 5-6s);代码侧(DB/N+1/连接/token/截断)已优化到位,再快需换付费模型或调小候选数(牺牲召回,未擅动)。
