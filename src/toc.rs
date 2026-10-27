// 對應 Kotlin `toc/ChapterIndexer.kt`:從段落清單萃取章節目錄。
// 章節 = 獨立一行(段內無換行)且符合「第+數字/中文數字+章回節集部卷篇」或「Chapter N」。
use crate::text::Paragraph;
use regex::Regex;

#[derive(Clone, Debug)]
pub struct TocEntry {
    pub title: String,
    pub block_index: usize,
}

pub struct ChapterIndexer;

impl ChapterIndexer {
    pub fn toc_of(paragraphs: &[Paragraph]) -> Vec<TocEntry> {
        // 第[〇零一二三四五六七八九十百千萬兩0-9]+[章回節集部卷篇],允許「第 三 章」式空白。
        let cjk = Regex::new(r"^第\s*[〇零一二三四五六七八九十百千萬兩\d]+\s*[章回節集部卷篇]")
            .expect("static regex");
        let en = Regex::new(r"(?i)^chapter\s+[0-9IVXLC]+").expect("static regex");
        let mut entries = Vec::new();
        for (i, p) in paragraphs.iter().enumerate() {
            // 段首可能有全形縮排空白,判斷章節用 trim 後的行。
            let line = p.text.trim();
            if !line.is_empty()
                && !line.contains('\n')
                && (cjk.is_match(line) || en.is_match(line))
            {
                entries.push(TocEntry {
                    title: line.to_string(),
                    block_index: i,
                });
            }
        }
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::chunker::TextChunker;

    fn txt_paras(text: &str) -> Vec<Paragraph> {
        TextChunker::blocks(text).blocks
    }

    #[test]
    fn standalone_chapter_lines_detected() {
        let text = "前言文字\n\n第一章　開始\n\n這裡是內文。\n\n第三章　最終\n\n內文。";
        let toc = ChapterIndexer::toc_of(&txt_paras(text));
        assert_eq!(toc.len(), 2);
        assert_eq!(toc[0].title, "第一章　開始");
        assert_eq!(toc[1].title, "第三章　最終");
    }

    #[test]
    fn all_units_recognized() {
        let units = ["章", "回", "節", "集", "部", "卷", "篇"];
        let text: String = units
            .iter()
            .enumerate()
            .map(|(i, u)| format!("第{}{u}　標題\n\n", i + 1))
            .collect();
        let toc = ChapterIndexer::toc_of(&txt_paras(&text));
        assert_eq!(toc.len(), units.len());
    }

    #[test]
    fn numeric_and_chinese_numerals() {
        let text = "第3章　阿拉伯數字\n\n正文\n\n第二百三十六回　中文數字\n\n正文";
        assert_eq!(ChapterIndexer::toc_of(&txt_paras(text)).len(), 2);
    }

    #[test]
    fn multi_line_paragraph_not_chapter() {
        let p = vec![Paragraph {
            text: "第一章　開始\n這裡是第一章的內文。".into(),
        }];
        assert_eq!(ChapterIndexer::toc_of(&p).len(), 0);
    }

    #[test]
    fn english_chapter_detected() {
        let text = "Chapter 1 The Beginning\n\nrest of para\n\nChapter 2\nmore body";
        assert_eq!(ChapterIndexer::toc_of(&txt_paras(text)).len(), 2);
    }

    #[test]
    fn indent_prefixed_chapter_still_detected() {
        let text = "　　第一章　劫後夢醒\n\n　　正文內容。";
        let toc = ChapterIndexer::toc_of(&txt_paras(text));
        assert_eq!(toc.len(), 1);
        assert_eq!(toc[0].title, "第一章　劫後夢醒");
    }

    #[test]
    fn block_index_points_at_right_paragraph() {
        let text = "開頭\n\n第二章　甲\n\n內文\n\n第五章　乙";
        let paras = txt_paras(text);
        let toc = ChapterIndexer::toc_of(&paras);
        assert_eq!(toc.len(), 2);
        assert_eq!(paras[toc[0].block_index].text.trim(), "第二章　甲");
        assert_eq!(paras[toc[1].block_index].text.trim(), "第五章　乙");
    }
}
