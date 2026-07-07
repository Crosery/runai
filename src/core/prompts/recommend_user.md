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

## 候选数量：最小充分集合，精准优先

不要凑数量。硬上限 {TOP_K} 是上限，不是目标。只输出通过准入判断的 skill：

- 单点请求且一个 skill 直接覆盖 → EXCLUSIVE 只输出 1 个
- 单点请求但多个 skill 是替代方案 → EXCLUSIVE 输出少量最不同的直接命中项
- 工作流请求 → COMPATIBLE 只输出完成链路必需的互补项
- 弱相关、只同组、只 BM25 高、只历史高频 → 不输出
- 没有直接命中 → EXCLUSIVE 空列表，并保留 reasoning 行说明原因

完全不相关：第一行 `EXCLUSIVE`，第二行写 `reasoning:`，下面无 skill。
