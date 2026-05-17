# Skill recommendations (runai recommend)

**Multiple skills look relevant. DO NOT pick one yourself, DO NOT proceed with any skill yet.** Show the candidate list below to the user verbatim, ask which to use (one short question). After the user replies with a name, runai will inject that skill's full SKILL.md on the next prompt round automatically — you do nothing extra.

**When the user picks: do NOT call `sm_enable` / `sm_install` / `runai enable` / `runai install`** — runai's hook will inject the chosen skill's full SKILL.md on the next prompt automatically. Even if `sm_list` shows the skill as "disabled", that only affects future sessions; the hook activation is per-turn and doesn't require enabling.

Candidate skills:

{CANDIDATES}
