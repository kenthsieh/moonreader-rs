// 對應 Kotlin `text/PageSplitter.kt`:把超大文字切成編輯頁。
// pages 串接(無分隔)可無損還原原文。

pub const PAGE_CHARS: usize = 60_000;

/// max_chars 以「字元數」計(與 Kotlin 版一致),內部在字元邊界上切。
pub fn split(text: &str, max_chars: usize) -> Vec<String> {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return vec![text.to_string()];
    }
    // 預切位點:每 max_chars 個字元的窗口內找最後一個換行。
    let mut pages = Vec::with_capacity(char_count / max_chars + 2);
    let chars: Vec<char> = text.chars().collect(); // 大檔用 char 陣列换取索引簡單
    let mut start = 0usize;
    while start < chars.len() {
        let mut end = (start + max_chars).min(chars.len());
        if end < chars.len() {
            // 在窗口尾部找最後一個換行,切在其後。
            let cut = chars[start..end].iter().rposition(|c| *c == '\n');
            if let Some(rel) = cut {
                let abs = start + rel;
                if abs > start {
                    end = abs + 1;
                }
            }
        }
        pages.push(chars[start..end].iter().collect());
        start = end;
    }
    pages
}

/// 字元 offset → (頁 idx, 頁內 offset)。頁界 offset 歸下一頁開頭。
pub fn locate(pages: &[String], offset: usize) -> (usize, usize) {
    if pages.is_empty() {
        return (0, 0);
    }
    let total: usize = pages.iter().map(|p| p.chars().count()).sum();
    let o = offset.min(total);
    let mut acc = 0usize;
    for (i, p) in pages.iter().enumerate() {
        let len = p.chars().count();
        if o < acc + len || i == pages.len() - 1 {
            return (i, o.saturating_sub(acc).min(len));
        }
        acc += len;
    }
    (pages.len() - 1, pages.last().map(|p| p.chars().count()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_single_page() {
        let pages = split("hello", 100);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0], "hello");
    }

    #[test]
    fn pages_join_back_losslessly() {
        let src: String = (0..2000).map(|i| format!("第{i} 行內容。\n")).collect();
        let pages = split(&src, 500);
        assert_eq!(pages.concat(), src);
    }

    #[test]
    fn each_page_within_cap() {
        let src: String = (0..5000)
            .map(|i| format!("line {i}\n"))
            .collect::<Vec<_>>()
            .join("");
        let src = src.trim_end_matches('\n').to_string();
        let pages = split(&src, 1000);
        assert!(pages.iter().all(|p| p.chars().count() <= 1000));
    }

    #[test]
    fn cuts_at_newline_when_possible() {
        let src = "aaaa\nbbbb\ncccc\ndddd";
        let pages = split(src, 6);
        let last = pages.len() - 1;
        for (i, p) in pages.iter().enumerate() {
            assert!(p.is_empty() || p.ends_with('\n') || i == last, "[{p}]");
        }
        assert_eq!(pages.concat(), src);
    }

    #[test]
    fn empty_text_still_one_page() {
        assert_eq!(split("", 60_000), vec!["".to_string()]);
    }

    #[test]
    fn locate_maps_offset_to_page_and_cursor() {
        let pages = vec!["aaaa".to_string(), "bbbb".to_string(), "cccc".to_string()];
        assert_eq!(locate(&pages, 6), (1, 2));
        assert_eq!(locate(&pages, 8), (2, 0));
        assert_eq!(locate(&pages, 0), (0, 0));
    }
}
