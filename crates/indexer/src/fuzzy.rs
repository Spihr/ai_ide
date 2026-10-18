//! 子序列模糊匹配打分。
//!
//! 目标是「输入 `smrs` 能排到 `src/main.rs`」这类命令行式检索。打分遵循三条直觉：
//!
//! 1. **连续命中比零散命中好**（`main` 优于 `m_a_i_n`）；
//! 2. **越早命中越好**（`main.rs` 优于 `src/util/main.rs`）；
//! 3. **路径越短越好**（同分时浅路径优先）。
//!
//! 不追求 fzf 级别的精细度——M1 只需要「可解释、可复现、够快」。

/// 连续命中的加分。
const BONUS_CONSECUTIVE: f64 = 15.0;
/// 断开一次后的命中加分。
const BONUS_GAP: f64 = 5.0;
/// 首个字符命中的加分。
const BONUS_START: f64 = 10.0;
/// 起始位置惩罚系数（命中越靠后扣得越多）。
const PENALTY_OFFSET: f64 = 1.0;
/// 长度惩罚系数。
const PENALTY_LENGTH: f64 = 0.05;
/// 大小写完全一致的额外加分。
const BONUS_CASE: f64 = 2.0;

/// 计算 `needle` 在 `haystack` 中的模糊得分。
///
/// - `needle` 为空时返回 `Some(0.0)`（空查询不算不匹配）；
/// - `needle` 不是 `haystack` 的子序列时返回 `None`。
pub fn subsequence_score(haystack: &str, needle: &str) -> Option<f64> {
    if needle.is_empty() {
        return Some(0.0);
    }

    let hay: Vec<char> = haystack.chars().collect();
    let hay_lower: Vec<char> = haystack.to_lowercase().chars().collect();
    let ndl: Vec<char> = needle.chars().collect();
    let ndl_lower: Vec<char> = needle.to_lowercase().chars().collect();

    if ndl_lower.len() > hay_lower.len() {
        return None;
    }

    let mut score = 0.0f64;
    let mut cursor = 0usize;
    let mut last_hit: Option<usize> = None;
    let mut first_hit = 0usize;

    for (i, target) in ndl_lower.iter().enumerate() {
        let mut hit = None;
        while cursor < hay_lower.len() {
            let idx = cursor;
            cursor += 1;
            if hay_lower[idx] == *target {
                hit = Some(idx);
                break;
            }
        }
        let idx = hit?;

        if i == 0 {
            first_hit = idx;
            score += BONUS_START;
        } else {
            match last_hit {
                Some(prev) if prev + 1 == idx => score += BONUS_CONSECUTIVE,
                Some(_) => score += BONUS_GAP,
                None => score += BONUS_GAP,
            }
        }

        // 原字符大小写一致（例如查询 `Main` 命中 `Main`）额外加分。
        if hay.get(idx) == Some(&ndl[i]) {
            score += BONUS_CASE;
        }

        last_hit = Some(idx);
    }

    score -= first_hit as f64 * PENALTY_OFFSET;
    score -= hay.len().saturating_sub(ndl_lower.len()) as f64 * PENALTY_LENGTH;

    // 长度惩罚可能把得分压到负数；保持单调即可，不做截断。
    Some(score)
}

/// 名字命中相对路径命中的权重。
pub const NAME_WEIGHT: f64 = 1.0;
/// 路径命中的折减系数：用户通常记得文件名胜于完整路径。
pub const PATH_WEIGHT: f64 = 0.6;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_needle_matches_everything() {
        assert_eq!(subsequence_score("anything", ""), Some(0.0));
    }

    #[test]
    fn non_subsequence_does_not_match() {
        assert_eq!(subsequence_score("main.rs", "zzz"), None);
        // 顺序敏感：`nm` 不能命中 `main`
        assert_eq!(subsequence_score("main", "nm"), None);
    }

    #[test]
    fn consecutive_beats_scattered() {
        let consecutive = subsequence_score("main.rs", "main").unwrap();
        let scattered = subsequence_score("m_a_i_n.rs", "main").unwrap();
        assert!(
            consecutive > scattered,
            "连续命中应得分更高：{consecutive} vs {scattered}"
        );
    }

    #[test]
    fn earlier_hit_wins() {
        let early = subsequence_score("main.rs", "main").unwrap();
        let late = subsequence_score("src/util/main.rs", "main").unwrap();
        assert!(early > late, "更靠前的命中应得分更高：{early} vs {late}");
    }

    #[test]
    fn shorter_path_wins_on_tie() {
        let short = subsequence_score("a/rs", "rs").unwrap();
        let long = subsequence_score("a/b/c/d/e/rs", "rs").unwrap();
        assert!(short > long);
    }

    #[test]
    fn case_insensitive_matching_works() {
        assert!(subsequence_score("MainWindow.tsx", "mainwindow").is_some());
        assert!(subsequence_score("MainWindow.tsx", "MAIN").is_some());
    }

    #[test]
    fn camel_case_acronym_matches() {
        assert!(subsequence_score("AIProvider.ts", "aip").is_some());
        assert!(subsequence_score("ai_provider.ts", "ap").is_some());
    }
}
