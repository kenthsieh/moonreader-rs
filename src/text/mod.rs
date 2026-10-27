// 對應 Kotlin `text/` 目錄。
// 偏移一律採 UTF-8 位元組索引(Kotlin 版是 UTF-16 字元索引);
// 切分一律落在字元邊界上(對應 surrogate-pair 防護)。
// 游標/undo 需要字元索引的地方,用 text::search::byte_to_char 換算。
pub mod chunker;
pub mod page_history;
pub mod page_splitter;
pub mod search;
pub mod undo_redo;

pub use chunker::Paragraph;
