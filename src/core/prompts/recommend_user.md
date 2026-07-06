<!-- prompt: recommend_user | callers: recommend::router::recommend_for_user_with_client | vars: {USER_PROMPT},{INTENT_SUMMARY},{BM25_CANDIDATE_LIMIT},{CWD_BLOCK},{PROJECT_CONTEXT_BLOCK},{HISTORY_BLOCK},{ALREADY_ROUTED_BLOCK},{CANDIDATE_LISTING},{TOP_K} -->
## 用户当前 prompt (最高优先级，必须先看这段判断意图)

```
{USER_PROMPT}
```

{CWD_BLOCK}{PROJECT_CONTEXT_BLOCK}{HISTORY_BLOCK}{ALREADY_ROUTED_BLOCK}## 意图摘要（BM25 查询来源）

下面是 runai 根据当前输入和当前 session 短记忆整理出的检索摘要。BM25 默认 {BM25_CANDIDATE_LIMIT} 个 skill 候选来自这段摘要；最终判断仍以用户当前 prompt 为最高优先级。

```text
{INTENT_SUMMARY}
```

候选 skill:
{CANDIDATE_LISTING}

---

回到用户当前 prompt 做最终判断：

```
{USER_PROMPT}
```

输出格式（严格）：
第一行：`COMPATIBLE` 或 `EXCLUSIVE`
第二行（**必填**，缺则视为格式错误）：`reasoning: <用户意图 + 为什么推这套，必含因果链，20-50 字>`
之后：每行一个 skill name，第一行最相关。

## 候选数量：宁多勿少，互补优先

**不要默认推 3 个**。按 prompt 实际涉及的工作流维度推：

- 用户 prompt 含 N 个并列要求（"视觉冲击 + 细节 + 动效 + 思考"= 4 维）→ 同 group 候选里每维各推一个，N 个都给
- 用户 prompt 是单点请求（"做 ppt"）→ EXCLUSIVE 选 2-3 个不同实现让用户挑
- COMPATIBLE 工作流型 → 推 3-6 个互补 skill 形成完整链路（启动器 + 主工具 + 验证 + 配套），第一行最核心
- 硬上限 {TOP_K}，但**不要为了不到上限就保守只推 3 个**——能凑 4-5 个真互补就推 4-5 个

宁多勿少：漏配套比多推一两个无用 skill 代价大得多（漏了用户得自己找，多推用户可以忽略）。

完全不相关：第一行 `EXCLUSIVE`，下面无 skill。
