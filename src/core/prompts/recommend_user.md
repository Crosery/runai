<!-- prompt: recommend_user | callers: recommend::router::recommend_for_user_with_client | vars: {TASK_ANCHOR},{INTENT_SUMMARY},{BM25_CANDIDATE_LIMIT},{CWD_BLOCK},{PROJECT_CONTEXT_BLOCK},{HISTORY_BLOCK},{CANDIDATE_LISTING} -->
## 当前任务锚点

```text
{TASK_ANCHOR}
```

## 检索意图 / expansion

```text
{INTENT_SUMMARY}
```

{CWD_BLOCK}{PROJECT_CONTEXT_BLOCK}{HISTORY_BLOCK}候选 skill（最多 {BM25_CANDIDATE_LIMIT} 个；只能选择当前列表中的短 ID）:
{CANDIDATE_LISTING}

只返回 JSON：`{"mode":"exclusive|compatible","selected":["C01"],"reasoning":"..."}`。真无关时 `selected` 为空数组。
