<!-- prompt: hook_output | callers: recommend::hook_output::render_hook_output | vars: {MODE},{REASONING_BLOCK},{CANDIDATES_BLOCK},{SESSION_ID_ARG},{ACTIVATION_DIRECTIVE},{FEEDBACK_PROTOCOL_BLOCK} -->
runai 推荐 (mode={MODE})

{REASONING_BLOCK}候选：

{CANDIDATES_BLOCK}

激活：每个 skill 跑一次 `runai-client activate <skill_name>{SESSION_ID_ARG}`，stdout 是 SKILL.md 全文，按其内容执行用户原 prompt。skill bundle 内的 references / scripts / templates 用 `runai-client file <skill_name> <relpath>` 读；`~/.xxx`、绝对路径等运行时用户数据不在 bundle 里，按本机文件直接读。activate 自己读 `~/.runai-identity`，不带 server URL。

{ACTIVATION_DIRECTIVE}
激活后回复首行写 `激活 skill: <逗号分隔>`。
{FEEDBACK_PROTOCOL_BLOCK}
