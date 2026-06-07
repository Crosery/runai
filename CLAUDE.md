# runai

**铁律 - 不准擅自定版本号**

AI 不主动建议、不写、不引用任何版本号、release 名、milestone 名 — 不论形态（语义化版本、字母 + 数字代号、自造批次别名都算）。**版本决策只有 Crosery 本人能定。**

适用于：

- 文档（PLANNING.md / README.md / AGENTS.md / 注释 / commit message）
- 分支名（不能用 feat/v1-xxx 这种带版本号的分支）
- workflow / script / meta 字段
- 任何对外可见的产出

不准用版本号区分批次时，用**章节号**（PLANNING §1.x）或**功能名**（feat/community-market、feat/server-mode-flag）。

历史事件：2026-06-07 把 PLANNING §1.x 实施 commit 拆到带版本号前缀的分支 + 在多处文档和 commit message 写版本号，被 Crosery 指为越权。

All AI agent guidance for this repository lives in a single source-of-truth file: **[AGENTS.md](AGENTS.md)**. Read that before doing anything — especially the "安全契约 / Safety Contract" section at the top. Per-module deep dives are in `src/**/*.LLM.md` files, indexed from AGENTS.md.

@AGENTS.md
