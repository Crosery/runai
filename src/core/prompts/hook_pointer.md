# Skill recommendation (runai recommend)

runai 给当前 prompt 推了一个 skill。**这是建议，不是强制激活 — 由你判断要不要采用**。

- **Name**: {NAME}
- **What it does**: {DESC}
- **SKILL.md path**: `{PATH}`

**采用流程**：
1. 先判断这个 skill 跟用户当前 prompt 是否真的对口
2. **对口** → Read 上面 SKILL.md 路径一次，按内容执行；回复第一行写 `激活 skill: {NAME}` 让用户知道你采用了
3. **不对口 / 你已经知道怎么解 / 这个 skill 一般** → 当作没看到，正常回应用户，不用提它，更不要 Read 这个 SKILL.md

**runai 自动计数**：你 Read 了 SKILL.md 这个动作会被 Claude Code 写进 transcript jsonl，runai 下次 hook 触发时会自己扫到，自动 +1 usage_count + 本 session 不再重复推这个 skill。不需要你跑额外命令上报。

**不要做的事**：别调 `sm_enable` / `sm_install` / `runai enable` / `runai install` / 任何 "activate" 工具。Read SKILL.md 路径本身就是采用动作。即使 `sm_list` 显示这个 skill 是 disabled 也无所谓，那只影响下次 session。
