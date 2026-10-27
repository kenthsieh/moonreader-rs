// 對應 Kotlin `text/TextSearch.kt`:全文搜尋,大小寫不分,結果有上限。
// 22MB 全檔不能每次按鍵都複製/材料化整份字串,故走位元組級掃描:
// - ASCII 查詢:大小寫不敏感的位元組比對(UTF-8 continuaton 位元組恒 >= 0x80,
//   故 ASCII 位元組不會誤命中多字元序列內部,安全)。
// - 非 ASCII 查詢(中文等):先比查詢首字元的位元組序列(首字元位元組不會
//   出現在別的序列中间),其餘位元組精确比對。
// 回傳位元組區間;字元索引由呼叫端按需換算(編輯器只在單頁內搜尋,可負荷)。
pub const MAX_RESULTS: usize = 5000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub byte_start: usize,
    pub byte_end: usize,
}

#[derive(Clone, Debug)]
pub struct Result {
    pub matches: Vec<Match>,
    pub truncated: bool,
}

pub fn find_all(text: &str, query: &str, max_results: usize) -> Result {
    let mut matches = Vec::new();
    if query.is_empty() {
        return Result {
            matches,
            truncated: false,
        };
    }
    let hay = text.as_bytes();
    let needle = query.as_bytes();
    let all_ascii = query.is_ascii();

    let mut i = 0usize;
    while i < hay.len() {
        let found = if all_ascii {
            find_ascii_ignore_case(hay, needle, i)
        } else {
            find_exact(hay, needle, i)
        };
        let Some(pos) = found else { break };
        matches.push(Match {
            byte_start: pos,
            byte_end: pos + needle.len(),
        });
        if matches.len() >= max_results {
            return Result {
                matches,
                truncated: true,
            };
        }
        i = pos + needle.len();
    }
    Result {
        matches,
        truncated: false,
    }
}

fn find_ascii_ignore_case(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from + needle.len() > hay.len() {
        return None;
    }
    let f0 = needle[0].to_ascii_lowercase();
    let mut i = from;
    while i + needle.len() <= hay.len() {
        if hay[i].to_ascii_lowercase() == f0 && hay[i..i + needle.len()].iter()
            .zip(needle.iter())
            .all(|(h, n)| h.to_ascii_lowercase() == n.to_ascii_lowercase())
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 迴圈版(不可遞迴:中文首位元組 0xE4..0xE9 極常見,「首位元組命中但整串不符」
/// 的次數可達百萬級,遞迴會 stack overflow 直接殺掉整個程式)。
fn find_exact(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < hay.len() {
        let pos = memchr(hay, needle[0], i)?;
        if pos + needle.len() > hay.len() {
            return None;
        }
        if &hay[pos..pos + needle.len()] == needle {
            return Some(pos);
        }
        i = pos + 1;
    }
    None
}

fn memchr(hay: &[u8], byte: u8, from: usize) -> Option<usize> {
    hay[from..].iter().position(|b| *b == byte).map(|p| p + from)
}

/// 位元組 offset → 字元 offset。
pub fn byte_to_char(text: &str, byte: usize) -> usize {
    text.get(..byte.min(text.len()))
        .map(|s| s.chars().count())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_case_insensitive_ascii() {
        let r = find_all("aBc abc ABC", "ABC", MAX_RESULTS);
        assert_eq!(r.matches.len(), 3);
        assert!(!r.truncated);
        assert_eq!(&"aBc abc ABC"[r.matches[2].byte_start..r.matches[2].byte_end], "ABC");
    }

    #[test]
    fn cjk_matches() {
        let text = "第一章　劫後夢醒\n內文第一章";
        let r = find_all(text, "第一章", MAX_RESULTS);
        assert_eq!(r.matches.len(), 2);
        assert_eq!(r.matches[0].byte_start, 0);
        assert_eq!(&text[r.matches[1].byte_start..r.matches[1].byte_end], "第一章");
    }

    #[test]
    fn empty_query_yields_empty() {
        assert!(find_all("abc", "", MAX_RESULTS).matches.is_empty());
    }

    #[test]
    fn truncates_at_cap() {
        let text = "ab".repeat(6000);
        let r = find_all(&text, "ab", MAX_RESULTS);
        assert_eq!(r.matches.len(), MAX_RESULTS);
        assert!(r.truncated);
    }

    #[test]
    fn ascii_needle_does_not_match_inside_multibyte() {
        // 「漢」= E6 BC A2;小寫 'a'(0x61)不應在其中誤命中。
        let text = "漢字漢";
        assert!(find_all(text, "a", MAX_RESULTS).matches.is_empty());
    }

    #[test]
    fn many_first_byte_misses_do_not_overflow_stack() {
        // 「一」= E4 B8 80、「七」= E4 B8 83:每個字都命中首位元組但整串不符。
        // 在 2MB 堆疊的背景執行緒跑(與 spawn_search 相同),舊的遞迴版會 overflow。
        let hay = format!("{}七", "一".repeat(300_000));
        let r = std::thread::spawn(move || find_all(&hay, "七", MAX_RESULTS))
            .join()
            .expect("search thread must not crash");
        assert_eq!(r.matches.len(), 1);
        assert_eq!(r.matches[0].byte_start, 300_000 * 3);
    }

    #[test]
    fn exact_match_at_very_end_and_partial_tail() {
        assert_eq!(find_all("中文中", "中", MAX_RESULTS).matches.len(), 2);
        // 尾端只剩部分位元組時不得越界。
        assert!(find_all("中文", "文字", MAX_RESULTS).matches.is_empty());
    }

    #[test]
    fn byte_to_char_conversion() {
        let text = "你好hello";
        assert_eq!(byte_to_char(text, 6), 2);
        assert_eq!(byte_to_char(text, text.len()), 7);
    }
}
