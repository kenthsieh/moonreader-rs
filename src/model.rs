// 資料模型:對應 Kotlin `data/model/Models.kt` + `ReaderPrefs.kt`。
use serde::{Deserialize, Serialize};

/// 閱讀主題(獨立於 App 深淺色)。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ReadingThemeName {
    #[default]
    Light,
    Dark,
    Sepia,
}

/// 排版對齊。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TextAlignName {
    #[default]
    Left,
    Justify,
}

/// 書庫紀錄。`path` 對應 Kotlin 的 `uri`(Rust 版直接用檔案路徑),
/// content_hash / last_modified_at 用於外部修改衝突偵測。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Book {
    pub path: String,
    pub title: String,
    #[serde(default)]
    pub size_bytes: i64,
    #[serde(default = "default_charset")]
    pub charset_name: String,
    /// 上次閱讀位置:第一個可見段落 index。
    #[serde(default)]
    pub scroll_index: usize,
    #[serde(default)]
    pub last_opened_at: i64,
    #[serde(default)]
    pub added_at: i64,
    /// 載入/上次存檔時的檔案指紋,用於衝突偵測。
    #[serde(default)]
    pub content_hash: String,
    #[serde(default)]
    pub last_modified_at: i64,
}

fn default_charset() -> String {
    "UTF-8".to_string()
}

impl Book {
    pub fn new(path: impl Into<String>, title: impl Into<String>) -> Self {
        let now = now_millis();
        Book {
            path: path.into(),
            title: title.into(),
            size_bytes: 0,
            charset_name: "UTF-8".into(),
            scroll_index: 0,
            last_opened_at: now,
            added_at: now,
            content_hash: String::new(),
            last_modified_at: 0,
        }
    }
}

/// 開檔結果:解碼後文字 + 實際採用的編碼。
#[derive(Clone, Debug)]
pub struct LoadResult {
    pub text: String,
    pub charset_name: String,
    pub size_bytes: i64,
    pub content_hash: String,
    pub last_modified_at: i64,
}

/// 存檔結果(供 UI 決定提示/後續動作)。
#[derive(Clone, Debug)]
pub enum SaveOutcome {
    Success {
        backup_created: bool,
        new_hash: String,
        new_last_modified: i64,
        new_size_bytes: i64,
    },
    /// 磁碟上檔案已被外部改動,需使用者決定。
    Conflict,
    /// 文字含目標編碼無法表示的字元(未寫入;需改選編碼,例如 UTF-8)。
    Unencodable { charset_name: String, chars: Vec<char> },
    Failure(String),
}

/// 閱讀偏好(持久化)。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReaderPrefs {
    #[serde(default = "d_font")]
    pub font_size_sp: i32,
    #[serde(default = "d_line")]
    pub line_height_multiplier: f32,
    #[serde(default = "d_para")]
    pub paragraph_spacing_dp: i32,
    #[serde(default = "d_pad")]
    pub horizontal_padding_dp: i32,
    #[serde(default)]
    pub theme: ReadingThemeName,
    #[serde(default)]
    pub text_align: TextAlignName,
    /// 可為空(用系統預設字型),或字型系列名。
    #[serde(default)]
    pub font_family: String,
}

fn d_font() -> i32 {
    18
}
fn d_line() -> f32 {
    1.4
}
fn d_para() -> i32 {
    12
}
fn d_pad() -> i32 {
    16
}

impl Default for ReaderPrefs {
    fn default() -> Self {
        ReaderPrefs {
            font_size_sp: 18,
            line_height_multiplier: 1.4,
            paragraph_spacing_dp: 12,
            horizontal_padding_dp: 16,
            theme: ReadingThemeName::Light,
            text_align: TextAlignName::Left,
            font_family: String::new(),
        }
    }
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
