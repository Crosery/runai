<!-- prompt: recommend_user | callers: recommend::router::recommend_for_user_with_client | vars: {USER_PROMPT},{INTENT_SUMMARY},{BM25_CANDIDATE_LIMIT},{CWD_BLOCK},{PROJECT_CONTEXT_BLOCK},{HISTORY_BLOCK},{CANDIDATE_LISTING} -->
## 用户当前 prompt (最高优先级，必须先看这段判断意图)

```
{USER_PROMPT}
```

{CWD_BLOCK}{PROJECT_CONTEXT_BLOCK}{HISTORY_BLOCK}## 意图摘要（BM25 查询来源）

下面是 runai 根据当前输入和当前 session 短记忆整理出的检索摘要。BM25 默认 {BM25_CANDIDATE_LIMIT} 个 skill 候选来自这段摘要；最终判断仍以上面的用户当前 prompt 为最高优先级。

```text
{INTENT_SUMMARY}
```

候选 skill:
{CANDIDATE_LISTING}

---

回到上面的用户当前 prompt，按 system 里的准入、数量与输出规则做最终判断。
