<!-- prompt: hook_output | callers: recommend::hook_output::render_hook_output | vars: {MODE},{REASONING_BLOCK},{CANDIDATES_BLOCK},{ACTIVATION_DIRECTIVE},{SKIP_REMINDER_BLOCK},{SESSION_HISTORY_BLOCK},{FEEDBACK_PROTOCOL_BLOCK} -->
runai 推荐 (mode={MODE})

{REASONING_BLOCK}候选：

{CANDIDATES_BLOCK}

激活方式：每个 skill 跑一次 Bash

  runai-client activate <skill_name> --session-id "$CLAUDE_SESSION_ID"

stdout 是 SKILL.md 全文，按内容执行用户原 prompt。runai-client 会向 server 记 usage_count 并把当前 session 标记为已推过；server 不可达时，usage event 写入本地 outbox（`~/.runai/client-cache/servers/<server-key>/skills/<skill-key>/.outbox/`），缓存命中也会先确保 usage 已 ACK 或已入队再打印 SKILL.md。激活会把整个 skill 目录缓存到本地；如果 SKILL.md 要求读取 references / scripts / templates 等附属文件，用 `runai-client file <skill_name> <relpath>` 获取文件内容。激活指令本身不带 server URL — runai-client 自己读 `~/.runai-identity`。

{ACTIVATION_DIRECTIVE}
{SKIP_REMINDER_BLOCK}
激活后回复首行写 `激活 skill: <逗号分隔>`，再按 SKILL.md 内容执行用户原 prompt。

{SESSION_HISTORY_BLOCK}{FEEDBACK_PROTOCOL_BLOCK}
