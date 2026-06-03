runai 推荐 (mode={MODE})

{REASONING_BLOCK}候选：

{CANDIDATES_BLOCK}

激活方式：每个 skill 跑一次 Bash

  curl -s -X POST '{SERVER_URL}/skills/get/<skill_name>'{USER_HEADER}

stdout 是 SKILL.md 全文，按内容执行用户原 prompt。runai 自动记 usage_count 并把当前 session 标记为已推过。

{ACTIVATION_DIRECTIVE}
{SKIP_REMINDER_BLOCK}
激活后回复首行写 `激活 skill: <逗号分隔>`，再按 SKILL.md 内容执行用户原 prompt。

{SESSION_HISTORY_BLOCK}{FEEDBACK_PROTOCOL_BLOCK}
