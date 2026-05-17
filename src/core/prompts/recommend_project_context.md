当前项目背景（从 cwd/CLAUDE.md 及其 @ 引用的文件读取，告诉你这个项目是什么、用什么工具、有哪些专属命令）：

{PROJECT_DOCS}

用上面的项目背景判断 prompt 的真实意图。例：项目背景里出现 `kaiwu submit` 命令 → 用户说"提交模型"应该走 kaiwu 提交流程对应的 skill，不是 git commit。

