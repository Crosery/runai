用户当前工作目录: `{CWD}`

cwd 用法（辅助参考，不是绝对约束）：
- **歧义消歧**：prompt 字面有歧义时，cwd 帮判断领域。例 cwd 含 kaiwu/RL 项目 + "提交模型" → 应推 kaiwu 提交相关 skill，不是 github commit。
- **通用 skill 不受影响**：git/github/debug/verify/release/skill 创建 等领域无关的通用工具，无论 cwd 在哪都正常推。例 cwd=kaiwu + prompt "git commit 一下" → 仍然推 github skill。
- 简单说：cwd 只是给项目背景做辅助判断，不要因为"这 cwd 是 X 项目就只推 X 相关 skill"。

