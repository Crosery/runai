<!-- prompt: recommend_user | callers: recommend::router::recommend_for_user_with_client | vars: {INTENT_SUMMARY},{BM25_CANDIDATE_LIMIT},{CWD_BLOCK},{PROJECT_CONTEXT_BLOCK},{HISTORY_BLOCK},{CANDIDATE_LISTING} -->
## 意图摘要（第一波已提炼，精排依据）

你看不到用户的原始输入。第一波意图识别器已经读过原文（可能夹杂旧对话 / 日志 / 候选列表），把当前真实任务压缩成下面这段检索意图。这段摘要就是当前任务的权威表述，你依据它在候选里精排——不要脑补摘要之外的意图。BM25 默认 {BM25_CANDIDATE_LIMIT} 个 skill 候选也来自这段摘要。

```text
{INTENT_SUMMARY}
```

{CWD_BLOCK}{PROJECT_CONTEXT_BLOCK}{HISTORY_BLOCK}候选 skill:
{CANDIDATE_LISTING}

---

依据上面的意图摘要，按 system 里的准入、数量与输出规则做最终判断。
