//! Minimal BM25 ranker for skill prefilter.
//!
//! Why not pull a crate: tantivy / sonic / meilisearch are persistent indices
//! built for millions of docs. We have ~343 small documents (skill name +
//! description) and rebuild the index on every router call. A self-contained
//! BM25 with a bilingual tokenizer is ~100 lines and zero new deps.
//!
//! Tokenizer is bilingual: latin/ascii words split by whitespace, punct,
//! AND dash/underscore (so `ppt-anything` becomes two tokens — querying
//! "ppt" or "anything" both match; skill names are intentionally compound
//! identifiers). CJK chars emitted as unigrams AND adjacent-pair bigrams so
//! "做ppt" still matches skills whose descriptions say "ppt" and skills that
//! say "做 ppt".

use std::collections::{HashMap, HashSet};

const K1: f64 = 1.5;
const B: f64 = 0.75;

pub fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let lower = text.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_ascii_alphanumeric() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphanumeric() {
                i += 1;
            }
            let tok: String = chars[start..i].iter().collect();
            tokens.push(tok);
        } else if is_cjk(c) {
            tokens.push(c.to_string());
            if i + 1 < chars.len() && is_cjk(chars[i + 1]) {
                let bigram: String = chars[i..i + 2].iter().collect();
                tokens.push(bigram);
            }
            i += 1;
        } else {
            i += 1;
        }
    }
    tokens
}

pub fn is_cjk(c: char) -> bool {
    let u = c as u32;
    (0x4E00..=0x9FFF).contains(&u)        // CJK Unified Ideographs
        || (0x3400..=0x4DBF).contains(&u) // CJK Extension A
        || (0x3040..=0x30FF).contains(&u) // Hiragana + Katakana
        || (0xAC00..=0xD7AF).contains(&u) // Hangul
}

/// Returns true if the string contains any CJK character. Used by the
/// recommender to detect cross-language queries that can't be safely
/// BM25-prefiltered against an English-described skill corpus.
pub fn contains_cjk(s: &str) -> bool {
    s.chars().any(is_cjk)
}

/// Rank `docs` by BM25 relevance to `query`. Returns (doc_index, score)
/// pairs sorted descending by score. Documents with zero score are still
/// included (caller decides how many to take); empty query returns empty
/// vec so the caller can fall back to no prefilter.
pub fn rank<T: AsRef<str>>(query: &str, docs: &[T]) -> Vec<(usize, f64)> {
    let n = docs.len();
    if n == 0 {
        return Vec::new();
    }
    let q_tokens = tokenize(query);
    if q_tokens.is_empty() {
        return Vec::new();
    }

    let doc_tokens: Vec<Vec<String>> = docs.iter().map(|d| tokenize(d.as_ref())).collect();
    let doc_lens: Vec<f64> = doc_tokens.iter().map(|t| t.len() as f64).collect();
    let avgdl: f64 = (doc_lens.iter().sum::<f64>() / (n as f64)).max(1.0);

    let q_unique: HashSet<&str> = q_tokens.iter().map(|s| s.as_str()).collect();

    let mut df: HashMap<&str, usize> = HashMap::new();
    for term in &q_unique {
        let mut c = 0;
        for d in &doc_tokens {
            if d.iter().any(|t| t == term) {
                c += 1;
            }
        }
        df.insert(term, c);
    }

    let mut scores: Vec<(usize, f64)> = Vec::with_capacity(n);
    for (i, doc) in doc_tokens.iter().enumerate() {
        let mut score = 0.0;
        let dl = doc_lens[i];
        for term in &q_unique {
            let df_val = *df.get(term).unwrap_or(&0);
            if df_val == 0 {
                continue;
            }
            let tf = doc.iter().filter(|t| t == term).count() as f64;
            if tf == 0.0 {
                continue;
            }
            let idf = ((n as f64 - df_val as f64 + 0.5) / (df_val as f64 + 0.5) + 1.0).ln();
            let numer = tf * (K1 + 1.0);
            let denom = tf + K1 * (1.0 - B + B * dl / avgdl);
            score += idf * (numer / denom);
        }
        scores.push((i, score));
    }
    scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scores
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_splits_dash_underscore() {
        // Compound identifiers split — "ppt-anything" must yield both "ppt"
        // and "anything" so a query of "ppt" hits the doc.
        assert_eq!(tokenize("ppt-anything"), vec!["ppt", "anything"]);
        assert_eq!(tokenize("write_doc"), vec!["write", "doc"]);
    }

    #[test]
    fn tokenize_lowercases() {
        assert_eq!(tokenize("Figma-Align"), vec!["figma", "align"]);
    }

    #[test]
    fn tokenize_cjk_unigram_plus_bigram() {
        let toks = tokenize("做 ppt");
        // "做" + "ppt"; bigram only formed when adjacent CJK pair
        assert!(toks.contains(&"做".to_string()));
        assert!(toks.contains(&"ppt".to_string()));
    }

    #[test]
    fn tokenize_cjk_bigram_for_adjacent() {
        let toks = tokenize("提交模型");
        assert!(toks.contains(&"提".to_string()));
        assert!(toks.contains(&"交".to_string()));
        assert!(toks.contains(&"提交".to_string()));
        assert!(toks.contains(&"模型".to_string()));
    }

    #[test]
    fn tokenize_mixed_skips_punct() {
        let toks = tokenize("hello, world!  re-run");
        assert_eq!(toks, vec!["hello", "world", "re", "run"]);
    }

    #[test]
    fn rank_empty_inputs() {
        let empty: Vec<&str> = Vec::new();
        assert!(rank("query", &empty).is_empty());
        let docs = vec!["doc one", "doc two"];
        assert!(rank("", &docs).is_empty());
    }

    #[test]
    fn rank_finds_exact_keyword_match() {
        let docs = vec![
            "ppt-anything: build illustrated slide deck",
            "github wrapper for gh cli",
            "figma alignment for vue h5",
        ];
        let scores = rank("ppt", &docs);
        assert_eq!(scores[0].0, 0, "ppt query must rank ppt-anything first");
        assert!(scores[0].1 > 0.0);
        // the others should have zero score
        assert_eq!(scores[1].1, 0.0);
    }

    #[test]
    fn rank_chinese_query_hits_chinese_doc() {
        let docs = vec![
            "ppt-anything: 做漂亮的 ppt 演示文稿",
            "git commit assistant",
            "kaiwu rl reward designer",
        ];
        let scores = rank("做 ppt", &docs);
        assert_eq!(scores[0].0, 0, "doc 0 must win for both 做 and ppt");
        assert!(scores[0].1 > scores[1].1);
    }

    #[test]
    fn contains_cjk_detects_chinese_and_skips_pure_ascii() {
        assert!(contains_cjk("做个 ppt"));
        assert!(contains_cjk("提交模型"));
        assert!(contains_cjk("hello 世界"));
        assert!(!contains_cjk("create a frontend page"));
        assert!(!contains_cjk("figma-alignment"));
        assert!(!contains_cjk(""));
    }

    #[test]
    fn rank_returns_all_docs_sorted() {
        let docs = vec!["alpha", "alpha beta", "alpha beta gamma"];
        let scores = rank("alpha beta gamma", &docs);
        // doc 2 has all 3 terms, doc 1 has 2, doc 0 has 1 -> descending
        assert_eq!(scores.len(), 3);
        assert_eq!(scores[0].0, 2);
        assert!(scores[0].1 >= scores[1].1);
        assert!(scores[1].1 >= scores[2].1);
    }
}
