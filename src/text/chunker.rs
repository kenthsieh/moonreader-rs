// 對應 Kotlin `text/TextChunker.kt` + `Paragraph.kt`。
// 依「行」切成段落區塊,超長段落再切塊;附帶每個區塊在原文的位元組起始 offset。

/// 閱讀區塊:純文字版只有一種「段落」。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paragraph {
    pub text: String,
}

pub const MAX_PARAGRAPH_CHARS: usize = 2000;

pub struct BlocksWithOffsets {
    pub blocks: Vec<Paragraph>,
    /// 每個區塊在原文的起始位元組 offset(單調遞增)。
    pub offsets: Vec<usize>,
}

pub struct TextChunker;

impl TextChunker {
    pub fn blocks(source: &str) -> BlocksWithOffsets {
        let mut out: Vec<Paragraph> = Vec::new();
        let mut offs: Vec<usize> = Vec::new();
        for (start, end) in line_ranges(source) {
            let line = &source[start..end];
            // 僅去行尾空白:行首全形空格是中文小說的段首縮排,必須保留。
            let t = line.trim_end();
            if t.is_empty() {
                continue;
            }
            // 段落起點:跳過原行的 \r (若 \r\n 結尾則 trim_end 已去掉)。
            let para_start = start + (line.len() - line.trim_start_matches('\r').len());
            if t.chars().count() <= MAX_PARAGRAPH_CHARS {
                out.push(Paragraph {
                    text: t.to_string(),
                });
                offs.push(para_start);
            } else {
                for (rs, re) in chunk_ranges(t, MAX_PARAGRAPH_CHARS) {
                    out.push(Paragraph {
                        text: t[rs..re].to_string(),
                    });
                    offs.push(para_start + rs);
                }
            }
        }
        if out.is_empty() {
            out.push(Paragraph { text: String::new() });
            offs.push(0);
        }
        BlocksWithOffsets { blocks: out, offsets: offs }
    }
}

/// 依 \n 逐列切,回傳 (列起, 列尾不含 \n) 的位元組範圍。
fn line_ranges(s: &str) -> Vec<(usize, usize)> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (i, b) in s.bytes().enumerate() {
        if b == b'\n' {
            lines.push((start, i));
            start = i + 1;
        }
    }
    lines.push((start, s.len()));
    lines
}

/// 把字串依「字元上限」切塊,回傳每塊位元組區間(必在字元邊界上)。
pub(crate) fn chunk_ranges(s: &str, max_chars: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < s.len() {
        let mut end = i;
        let mut n = 0usize;
        while end < s.len() && n < max_chars {
            end += 1;
            while end < s.len() && !s.is_char_boundary(end) {
                end += 1;
            }
            n += 1;
        }
        out.push((i, end));
        i = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_lines_are_skipped() {
        let b = TextChunker::blocks("a\n\n \n b\n");
        assert_eq!(b.blocks.len(), 2);
        assert_eq!(b.blocks[0].text, "a");
        assert_eq!(b.blocks[1].text, " b");
    }

    #[test]
    fn leading_fullwidth_indent_preserved() {
        let b = TextChunker::blocks("　　正文一行。\n");
        assert_eq!(b.blocks[0].text, "　　正文一行。");
    }

    #[test]
    fn offsets_point_at_line_starts_in_original() {
        let src = "第一段\n\n第二段\n";
        let b = TextChunker::blocks(src);
        assert_eq!(b.offsets[0], 0);
        assert_eq!(&src[b.offsets[1]..b.offsets[1] + "第二段".len()], "第二段");
    }

    #[test]
    fn long_paragraph_is_chunked_without_breaking_chars() {
        let long = "漢".repeat(5000); // 每字 3 bytes
        let b = TextChunker::blocks(&long);
        assert_eq!(b.blocks.len(), 3); // 2000 + 2000 + 1000 chars
        assert_eq!(b.blocks.iter().map(|p| p.text.as_str()).collect::<String>(), long);
        assert_eq!(b.offsets.len(), 3);
        assert_eq!(b.offsets[2], 4000 * 3);
    }

    #[test]
    fn empty_text_yields_one_blank_block() {
        let b = TextChunker::blocks("");
        assert_eq!(b.blocks.len(), 1);
        assert_eq!(b.blocks[0].text, "");
    }

    #[test]
    fn chunk_ranges_respects_char_boundaries() {
        let s = "中文abc"; // 中/文 = 3 bytes each
        let r = chunk_ranges(s, 1);
        let parts: Vec<&str> = r.iter().map(|(a, b)| &s[*a..*b]).collect();
        assert_eq!(parts, vec!["中", "文", "a", "b", "c"]);
    }

    #[test]
    fn crlf_line_endings_trimmed() {
        let b = TextChunker::blocks("第一段\r\n第二段\r\n");
        assert_eq!(b.blocks.len(), 2);
        assert_eq!(b.blocks[0].text, "第一段");
        assert_eq!(b.blocks[1].text, "第二段");
    }
}
