{HISTORY_BLOCK}{ALREADY_ROUTED_BLOCK}候选 skill:
{CANDIDATE_LISTING}

用户当前 prompt:
{USER_PROMPT}

输出格式（严格）：
第一行：`COMPATIBLE` 或 `EXCLUSIVE`
之后：每行一个 skill name，最多 {TOP_K} 个，第一行最相关。
完全不相关：第一行 `EXCLUSIVE`，下面无 skill。
