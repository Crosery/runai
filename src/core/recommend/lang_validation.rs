//! Deterministic per-field language enforcement for enrich summaries.
//!
//! Load-bearing: the script-aware CJK / kana / hangul counters + the
//! task-anchor rule are what make a whole-English summary structurally
//! impossible to pass a `zh` index. Moved verbatim from the monolith — do not
//! "simplify" the field logic; aggregate ratios are wrong on both ends.

use anyhow::Result;

use crate::core::manager::SkillManager;

use super::config::RecommendConfig;

/// Iterate a parsed summary's prose fields as `(label, value)`, keeping only
/// `task` / `inputs` / `outputs` / `not-for`. `triggers` (intentionally zh/en
/// mixed keywords) and `score` are dropped — validating their language would
/// false-positive. Char-safe: the label/value split goes through
/// `split_once`, never a byte index, so a full-width `：` (3-byte UTF-8) can
/// never land mid-character and panic.
pub(super) fn prose_fields(summary: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in summary.lines() {
        let trimmed = line.trim();
        let (label, value) = match trimmed.split_once([':', '：']) {
            Some((l, v)) => (l.trim().to_ascii_lowercase(), v.trim()),
            None => continue,
        };
        if matches!(label.as_str(), "task" | "inputs" | "outputs" | "not-for") {
            out.push((label, value.to_string()));
        }
    }
    out
}

fn count_cjk(s: &str) -> usize {
    s.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count()
}

fn count_kana(s: &str) -> usize {
    s.chars().filter(|c| ('\u{3040}'..='\u{30ff}').contains(c)).count()
}

fn count_hangul(s: &str) -> usize {
    s.chars().filter(|c| ('\u{ac00}'..='\u{d7a3}').contains(c)).count()
}

/// English function words. Their presence as whole words marks a Latin-only
/// field as natural-language prose — as opposed to a comma-separated list of
/// identifiers / CLI flags / proper nouns (`URL, --baseline, package.json,
/// Vercel`), which carries none of them and is language-neutral. This is the
/// discriminator that stops the validator from re-flagging a perfectly good
/// Chinese summary just because one field lists tool names.
const EN_STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "with", "for", "to", "of", "in", "on", "that",
    "this", "your", "you", "via", "from", "into", "using", "when", "as", "by", "are",
    "is", "be", "it", "create", "edit", "generate", "build", "run", "use", "deploy",
    "write", "make", "get",
];

fn has_en_stopword(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower
        .split(|c: char| !c.is_ascii_alphabetic())
        .any(|w| EN_STOPWORDS.contains(&w))
}

/// Whether one prose field's value is written in `lang`.
///
/// `is_anchor` marks the `task` field — the one-line description of what the
/// skill does. A task is ALWAYS prose (never a bare identifier list), so in
/// `zh` it MUST carry Chinese: a zero-CJK task is a leak even when it dodges
/// every English stopword (e.g. `task: PDF extraction toolkit`). This anchor
/// rule is what makes a whole-English summary impossible to pass — the task
/// line can never be language-neutral. Non-anchor fields (inputs/outputs/
/// not-for) keep the lenient rule so a Chinese summary whose `inputs` is just
/// `URL, --baseline, package.json` is not falsely flagged.
///
/// - `zh`: any kana / hangul = foreign-script leak (Japanese / Korean source).
///   Any Chinese present = fine, even peppered with tool / API / path
///   identifiers. Zero Chinese in a non-anchor field = wrong only if it reads
///   as a sentence (an English stopword is present); zero Chinese in the
///   anchor `task` field = always wrong.
/// - `en`: rejects any CJK / kana / hangul in the prose (English prose has none).
/// - `ja`: requires kana (kanji alone is ambiguous with Chinese).
fn field_matches_lang(value: &str, lang: &str, is_anchor: bool) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return true;
    }
    let cjk = count_cjk(v);
    let kana = count_kana(v);
    let hangul = count_hangul(v);
    match lang {
        "" | "zh" => {
            if kana > 0 || hangul > 0 {
                return false;
            }
            if cjk > 0 {
                return true;
            }
            // Zero Chinese: the anchor task line is always a leak; a
            // non-anchor field is a leak only if it reads as a sentence.
            !is_anchor && !has_en_stopword(v)
        }
        "en" => cjk == 0 && kana == 0 && hangul == 0,
        "ja" => kana > 0,
        _ => true,
    }
}

/// Whether a summary's prose fields are ALL written in `summary_lang`.
///
/// Per-field, not aggregate: a single English `inputs:` line inside an
/// otherwise-Chinese summary fails — that mixed shape is exactly what a human
/// flags as broken, and an aggregate ratio over the whole body would let it
/// pass. `triggers` is never checked (intentionally cross-language for recall).
/// `bilingual` / any custom free-form language string skips enforcement (not
/// deterministically checkable).
pub fn summary_matches_lang(summary: &str, summary_lang: &str) -> bool {
    let lang = summary_lang.trim();
    if !matches!(lang, "" | "zh" | "en" | "ja") {
        return true;
    }
    prose_fields(summary)
        .iter()
        .all(|(label, value)| field_matches_lang(value, lang, label == "task"))
}

/// Names of skills whose stored summary's prose fields are NOT in the
/// configured `summary_lang` (language leaked from the source SKILL.md).
/// Powers `runai recommend enrich --fix-lang` — targeted repair of the
/// subset that violated the language contract, without a full `--force` pass.
pub fn find_language_mismatched_skills(mgr: &SkillManager) -> Result<Vec<String>> {
    let cfg = RecommendConfig::load(mgr.paths())?;
    let all = mgr.db().skill_ai_summary_all().unwrap_or_default();
    let mut out: Vec<String> = all
        .into_iter()
        .filter(|(_, summary)| !summary_matches_lang(summary, &cfg.summary_lang))
        .map(|(name, _)| name)
        .collect();
    out.sort();
    Ok(out)
}
