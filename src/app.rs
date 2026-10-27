//! MoonReader — Rust/egui 桌面版(由 Android Compose app 改寫)。
//!
//! 對應關係:
//! - `ui/navigation/Routes.kt` → `Route` 狀態機
//! - `ui/library/*`            → `render_library` / `add_book`
//! - `ui/reader/*`             → `render_reader` / `ReaderState`
//! - `ui/editor/*`             → `render_editor` / `EditorState`
//! - coroutines + Dispatchers.IO → 背景執行緒 + mpsc(`spawn_load` / `spawn_search`)
use crate::encoding::SUPPORTED_CHARSETS;
use crate::fileio;
use crate::model::{Book, ReadingThemeName, ReaderPrefs, SaveOutcome, TextAlignName, now_millis};
use crate::repo;
use crate::store::{AppPaths, LibraryStore, ReaderPrefsStore};
use crate::text::page_history::{Edit, PageEditHistory};
use crate::text::{Paragraph, chunker::TextChunker, page_splitter, search};
use crate::toc::{ChapterIndexer, TocEntry};
use egui::text::{CCursor, CCursorRange, LayoutJob, LayoutSection, TextFormat, TextWrapping};
use egui::{
    Align, Align2, Color32, FontData, FontDefinitions, FontFamily, FontId, Galley, Key,
    KeyboardShortcut, Modifiers, PointerButton, RichText, ScrollArea, TextEdit, Ui, pos2, vec2,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

/// 捲動條寬度預留(估算可用文字寬度用)。
const SCROLLBAR_W: f32 = 14.0;

// ─── 主題(對應 ui/theme/ReadingTheme.kt)───────────────────────────────────

#[derive(Clone)]
pub struct Palette {
    pub bg: Color32,
    pub text: Color32,
    pub subtle: Color32,
    pub accent: Color32,
    pub code_bg: Color32,
}

pub fn palette_of(name: ReadingThemeName) -> Palette {
    use ReadingThemeName as T;
    match name {
        T::Light => Palette {
            bg: Color32::from_rgb(255, 255, 255),
            text: Color32::from_rgb(0x1A, 0x1A, 0x1A),
            subtle: Color32::from_rgb(0x6B, 0x6B, 0x6B),
            accent: Color32::from_rgb(0x1B, 0x5E, 0x20),
            code_bg: Color32::from_rgb(0xF2, 0xF2, 0xF2),
        },
        T::Dark => Palette {
            bg: Color32::from_rgb(0x12, 0x12, 0x12),
            text: Color32::from_rgb(0xE6, 0xE6, 0xE6),
            subtle: Color32::from_rgb(0x9A, 0x9A, 0x9A),
            accent: Color32::from_rgb(0x81, 0xC7, 0x84),
            code_bg: Color32::from_rgb(0x1E, 0x1E, 0x1E),
        },
        T::Sepia => Palette {
            bg: Color32::from_rgb(0xF4, 0xEC, 0xD8),
            text: Color32::from_rgb(0x3B, 0x2F, 0x1E),
            subtle: Color32::from_rgb(0x7A, 0x6A, 0x50),
            accent: Color32::from_rgb(0x8B, 0x5A, 0x2B),
            code_bg: Color32::from_rgb(0xEA, 0xDF, 0xC6),
        },
    }
}

fn accent_alpha(c: Color32, a: u8) -> Color32 {
    let [r, g, b, _] = c.to_array();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

fn format_size(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

// ─── 背景工作(對應 coroutines;載入/全文搜尋不阻塞 UI)─────────────────────

struct LoadedDoc {
    path: PathBuf,
    text: String,
    charset_name: String,
    size_bytes: i64,
    content_hash: String,
    last_modified_at: i64,
}

#[derive(Clone, Copy, Debug)]
struct SearchHit {
    block_index: usize,
    /// 段內位元組區間(供 LayoutJob 高亮)。
    start_in_block: usize,
    end_in_block: usize,
}

enum Job {
    Load {
        gen: u64,
        rx: Receiver<Option<LoadedDoc>>,
    },
    Search {
        gen: u64,
        rx: Receiver<(Vec<SearchHit>, bool)>,
    },
}

fn spawn_load(
    ctx: &egui::Context,
    backups: PathBuf,
    path: PathBuf,
    charset: Option<String>,
    gen: u64,
) -> Job {
    let (tx, rx) = channel();
    let ui_ctx = ctx.clone();
    std::thread::spawn(move || {
        let r = repo::BookRepository::new(backups).load(&path, charset.as_deref());
        let doc = r.ok().map(|l| LoadedDoc {
            path,
            text: l.text,
            charset_name: l.charset_name,
            size_bytes: l.size_bytes,
            content_hash: l.content_hash,
            last_modified_at: l.last_modified_at,
        });
        let _ = tx.send(doc);
        ui_ctx.request_repaint();
    });
    Job::Load { gen, rx }
}

/// 全文搜尋背景掃描 → 命中映射回段落(對應 ReaderViewModel.runSearch)。
fn spawn_search(
    ctx: &egui::Context,
    text: String,
    query: String,
    offs: Vec<usize>,
    gen: u64,
) -> Job {
    let (tx, rx) = channel();
    let ui_ctx = ctx.clone();
    std::thread::spawn(move || {
        let r = search::find_all(&text, &query, search::MAX_RESULTS);
        let hits: Vec<SearchHit> = r
            .matches
            .iter()
            .map(|m| {
                let b = block_index_for_offset(&offs, m.byte_start);
                let bs = offs[b];
                SearchHit {
                    block_index: b,
                    start_in_block: m.byte_start - bs,
                    end_in_block: (m.byte_end - bs).max(m.byte_start - bs),
                }
            })
            .collect();
        let _ = tx.send((hits, r.truncated));
        ui_ctx.request_repaint();
    });
    Job::Search { gen, rx }
}

/// 二分:位元組 offset → 段落 index(offs 單調遞增;對應 blockIndexForOffset)。
fn block_index_for_offset(offs: &[usize], offset: usize) -> usize {
    if offs.is_empty() {
        return 0;
    }
    let mut lo = 0usize;
    let mut hi = offs.len() - 1;
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if offs[mid] <= offset {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// tops 為各段「累積末端」y;回傳含 y 的段落 index(y 恰在段末 = 下一段開頭)。
/// 可能回傳 tops.len()(y 超出全文),呼叫端自行夾住。
fn block_at(tops: &[f32], y: f32) -> usize {
    tops.partition_point(|t| *t <= y)
}

/// 各段高度 → 累積末端 y。
fn prefix_tops(heights: &[f32]) -> (Vec<f32>, f32) {
    let mut acc = 0f32;
    let tops = heights
        .iter()
        .map(|h| {
            acc += h;
            acc
        })
        .collect();
    (tops, acc)
}

/// 段落高度估算(尚未實際排版的段落用;中文全形 1em、半形 0.55em)。
fn estimate_height(text: &str, per_em: f32, line_h: f32, spacing: f32) -> f32 {
    let w_em: f32 = text
        .chars()
        .map(|ch| if (ch as u32) >= 0x2E80 { 1.0 } else { 0.55 })
        .sum();
    (w_em / per_em).ceil().max(1.0) * line_h + spacing
}

/// 翻頁目標捲動位置:捲一個視口高、保留一行重疊,夾在 [0, total - view_h]。
fn page_turn_target(scroll: f32, view_h: f32, line_h: f32, total: f32, dir: i32) -> f32 {
    let step = (view_h - line_h).max(line_h.max(1.0));
    let max_scroll = (total - view_h).max(0.0);
    (scroll + step * dir as f32).clamp(0.0, max_scroll)
}

// ─── 閱讀狀態(對應 ReaderUiState)──────────────────────────────────────────

#[derive(Default)]
struct ReaderState {
    loading: bool,
    error: Option<String>,
    book: Option<Book>,
    text: String,
    blocks: Vec<Paragraph>,
    /// 區塊 → 原文位元組 offset。
    block_off: Vec<usize>,
    total_chars: usize,
    toc: Vec<TocEntry>,
    search_open: bool,
    search_query: String,
    /// 搜尋 debounce 截止時間(對應 delay(250))。
    search_deadline: Option<f64>,
    search_hits: Vec<SearchHit>,
    search_active: isize,
    search_truncated: bool,
    searching: bool,
    controls_open: bool,
    toc_open: bool,
    /// 目錄開啟後要捲到目前章節(對應 TocSheet 的 initialFirstVisibleItemIndex)。
    toc_scroll_pending: bool,
    /// 長按設定的「編輯定位段」;None = 未定位。
    mark: Option<usize>,
    /// 各段高度(含段距):先估算,畫過的段改為實際排版高度。
    heights: Vec<f32>,
    /// 各段累積末端高度(虛擬化用;= heights 的前綴和)。
    tops: Vec<f32>,
    total_height: f32,
    metrics_sig: (f32, f32, f32, f32),
    /// 上一幀實際排版寬度(估算/量測用同一寬度)。
    layout_wrap: Option<f32>,
    /// 目前捲動位置與視口高(翻頁、長按命中測試用)。
    scroll_y: f32,
    view_h: f32,
    /// 下一幀要跳到的段落(目錄/搜尋/翻頁/開檔還原)。
    focus_block: Option<usize>,
    /// 下一幀要設定的捲動位置(content 座標)。
    scroll_override: Option<f32>,
    /// 第一個可見段落(渲染回寫;對應 firstVisibleItemIndex)。
    first_visible: usize,
    saved_scroll_index: usize,
    scroll_save_deadline: Option<f64>,
    /// 內容區螢幕矩形(手勢分區用)。
    content_rect: Option<egui::Rect>,
    /// 按壓追蹤:(時間, 位置)。
    press: Option<(f64, egui::Pos2)>,
}

// ─── 編輯狀態(對應 EditorUiState)──────────────────────────────────────────

#[derive(Clone, Copy)]
struct PageMatch {
    byte_start: usize,
    byte_end: usize,
}

struct EditorState {
    loading: bool,
    error: Option<String>,
    path: PathBuf,
    pages: Vec<String>,
    page: usize,
    /// 最後存檔/載入全文(髒基準)。
    baseline: String,
    expected_hash: String,
    charset: String,
    /// TextEdit 緩衝 = 目前頁。
    buffer: String,
    last_buffer: String,
    dirty: bool,
    conflict: bool,
    /// 程式化游標定位(開檔/切頁/undo/搜尋命中)。
    focus_pending: Option<(usize, usize)>,
    /// debounce 提交(對應 recordJob delay(450))。
    commit_deadline: Option<f64>,
    commit_sel: (usize, usize),
    history: PageEditHistory,
    search_open: bool,
    search_query: String,
    search_hits: Vec<PageMatch>,
    search_active: isize,
    search_truncated: bool,
    leave_dialog: bool,
    jump_input: String,
}

impl EditorState {
    fn new() -> Self {
        EditorState {
            loading: false,
            error: None,
            path: PathBuf::new(),
            pages: vec![String::new()],
            page: 0,
            baseline: String::new(),
            expected_hash: String::new(),
            charset: "UTF-8".into(),
            buffer: String::new(),
            last_buffer: String::new(),
            dirty: false,
            conflict: false,
            focus_pending: None,
            commit_deadline: None,
            commit_sel: (0, 0),
            history: PageEditHistory::new(60),
            search_open: false,
            search_query: String::new(),
            search_hits: Vec::new(),
            search_active: -1,
            search_truncated: false,
            leave_dialog: false,
            jump_input: String::new(),
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Route {
    Library,
    Reading,
    Editing,
}

pub struct MoonReader {
    paths: AppPaths,
    library: LibraryStore,
    prefs_store: ReaderPrefsStore,
    repo: repo::BookRepository,
    route: Route,
    reader: ReaderState,
    editor: EditorState,
    jobs: Vec<Job>,
    gen: u64,
    font_state: u8,
    toast: Option<(String, f64)>,
    pending_editor_offset: Option<usize>,
    exit_to_reader: bool,
    cli_file: Option<PathBuf>,
    /// 使用者關窗時有未存檔變更:先問,處理完再關。
    quit_pending: bool,
    /// 已確認可關閉(下一個 close_requested 不再攔截)。
    allow_close: bool,
}

/// 面板收集的動作(避免在 UI 閉包中重複借用 self)。
#[derive(Default)]
struct ReaderActs {
    back: bool,
    close_controls: bool,
    toggle_controls: bool,
    clear_mark: bool,
    open_search: bool,
    close_search: bool,
    search_prev: bool,
    search_next: bool,
    open_toc: bool,
    toc_select: Option<usize>,
    edit: bool,
    seek: Option<f32>,
    reload_charset: Option<String>,
    theme: Option<ReadingThemeName>,
    font_size: Option<i32>,
    line_height: Option<f32>,
    para_spacing: Option<i32>,
    h_padding: Option<i32>,
    align: Option<TextAlignName>,
}

impl MoonReader {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let paths = AppPaths::open();
        let library = LibraryStore::load(&paths);
        let prefs_store = ReaderPrefsStore::load(&paths);
        let backups = paths.backups_dir();
        MoonReader {
            paths,
            library,
            prefs_store,
            repo: repo::BookRepository::new(backups),
            route: Route::Library,
            reader: ReaderState::default(),
            editor: EditorState::new(),
            jobs: Vec::new(),
            gen: 0,
            font_state: 0,
            toast: None,
            pending_editor_offset: None,
            exit_to_reader: false,
            cli_file: std::env::args().nth(1).map(PathBuf::from).filter(|p| p.is_file()),
            quit_pending: false,
            allow_close: false,
        }
    }

    fn prefs(&self) -> ReaderPrefs {
        self.prefs_store.prefs().clone()
    }

    fn set_prefs<F: FnOnce(&mut ReaderPrefs)>(&mut self, f: F) {
        self.prefs_store.update(f);
    }

    fn next_gen(&mut self) -> u64 {
        self.gen += 1;
        self.gen
    }

    fn toast(&mut self, ctx: &egui::Context, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ctx.input(|i| i.time) + 2.6));
    }

    // ── 背景結果落地 ──────────────────────────────────────────────────────────

    fn poll_jobs(&mut self) {
        let mut keep = Vec::new();
        let mut loaded: Option<LoadedDoc> = None;
        let mut load_failed = false;
        let mut searched: Option<(Vec<SearchHit>, bool)> = None;
        for job in self.jobs.drain(..) {
            match &job {
                Job::Load { gen, rx } => match rx.try_recv() {
                    Ok(doc) => {
                        if *gen == self.gen {
                            match doc {
                                Some(d) => loaded = Some(d),
                                None => load_failed = true,
                            }
                        }
                    }
                    Err(TryRecvError::Disconnected) => {
                        if *gen == self.gen {
                            load_failed = true;
                        }
                    }
                    Err(TryRecvError::Empty) => keep.push(job),
                },
                Job::Search { gen, rx } => match rx.try_recv() {
                    Ok(res) => {
                        if *gen == self.gen {
                            searched = Some(res);
                        }
                    }
                    Err(TryRecvError::Disconnected) => {}
                    Err(TryRecvError::Empty) => keep.push(job),
                },
            }
        }
        self.jobs = keep;

        if let Some(d) = loaded {
            self.apply_loaded(d);
        }
        if load_failed {
            if self.route == Route::Editing {
                self.editor.loading = false;
                self.editor.error = Some("讀取失敗".into());
            } else {
                self.reader.loading = false;
                self.reader.error = Some("讀取失敗".into());
            }
        }
        if let Some((hits, truncated)) = searched {
            let r = &mut self.reader;
            r.searching = false;
            r.search_truncated = truncated;
            r.search_hits = hits;
            r.search_active = if r.search_hits.is_empty() { -1 } else { 0 };
            if r.search_active >= 0 {
                let b = r.search_hits[0].block_index;
                r.focus_block = Some(b);
            }
        }
    }

    fn apply_loaded(&mut self, d: LoadedDoc) {
        let path_str = d.path.to_string_lossy().into_owned();
        if self.route == Route::Editing {
            let pages = page_splitter::split(&d.text, page_splitter::PAGE_CHARS);
            let initial = self.pending_editor_offset.take().unwrap_or(0);
            let (pg, cur) = page_splitter::locate(&pages, initial);
            let buffer = pages.get(pg).cloned().unwrap_or_default();
            let ed = &mut self.editor;
            ed.pages = pages;
            ed.page = pg;
            ed.baseline = d.text;
            ed.expected_hash = d.content_hash;
            ed.charset = d.charset_name;
            ed.buffer = buffer.clone();
            ed.last_buffer = buffer;
            ed.loading = false;
            ed.error = None;
            ed.dirty = false;
            ed.conflict = false;
            ed.focus_pending = Some((cur, cur));
            ed.commit_deadline = None;
            ed.history.reset();
            ed.search_open = false;
            ed.search_query.clear();
            ed.search_hits.clear();
            ed.search_active = -1;
            let cs = ed.charset.clone();
            let eh = ed.expected_hash.clone();
            let (sb, lm) = (d.size_bytes, d.last_modified_at);
            self.library.update(&path_str, |b| Book {
                charset_name: cs,
                content_hash: eh,
                size_bytes: sb,
                last_modified_at: lm,
                ..b.clone()
            });
            self.library.save();
        } else {
            let with_offsets = TextChunker::blocks(&d.text);
            let n = with_offsets.blocks.len();
            // 全書字元總數(offsets 是「位元組」,不可與字元數相加)。
            let total_chars = d.text.chars().count();
            let toc = ChapterIndexer::toc_of(&with_offsets.blocks);
            let mut book = self
                .library
                .find(&path_str)
                .cloned()
                .unwrap_or_else(|| Book::new(&path_str, fileio::display_name(&d.path)));
            if book.title.is_empty() {
                book.title = fileio::display_name(&d.path);
            }
            book.charset_name = d.charset_name;
            book.size_bytes = d.size_bytes;
            book.content_hash = d.content_hash;
            book.last_modified_at = d.last_modified_at;
            book.last_opened_at = now_millis();
            self.library.upsert(book.clone());
            self.library.save();

            // 還原上次閱讀位置(對應 LaunchedEffect 的 scrollToItem(idx))。
            let idx = book.scroll_index.min(n.saturating_sub(1));
            let r = &mut self.reader;
            r.loading = false;
            r.error = None;
            r.book = Some(book);
            r.total_chars = total_chars.max(1);
            r.toc = toc;
            r.block_off = with_offsets.offsets;
            r.blocks = with_offsets.blocks;
            r.text = d.text;
            r.metrics_sig = (-1.0, -1.0, -1.0, -1.0);
            r.tops.clear();
            r.heights.clear();
            r.first_visible = idx;
            r.saved_scroll_index = idx;
            r.focus_block = if idx > 0 { Some(idx) } else { None };
            r.scroll_override = None;
            r.search_open = false;
            r.search_query.clear();
            r.search_hits.clear();
            r.search_active = -1;
            r.searching = false;
            r.search_deadline = None;
            r.controls_open = false;
            r.toc_open = false;
            r.mark = None;
            r.press = None;
        }
    }

    // ── 開啟 / 位置換算 ───────────────────────────────────────────────────────

    fn open_reader(&mut self, ctx: &egui::Context, path: &Path) {
        let rect = self.reader.content_rect;
        self.reader = ReaderState::default();
        self.reader.content_rect = rect;
        self.reader.loading = true;
        self.route = Route::Reading;
        let gen = self.next_gen();
        self.jobs.push(spawn_load(
            ctx,
            self.paths.backups_dir(),
            path.to_path_buf(),
            None,
            gen,
        ));
    }

    fn open_editor(&mut self, ctx: &egui::Context, path: &Path, initial_char_offset: usize) {
        self.editor = EditorState::new();
        self.editor.path = path.to_path_buf();
        self.editor.loading = true;
        self.route = Route::Editing;
        let gen = self.next_gen();
        self.jobs.push(spawn_load(
            ctx,
            self.paths.backups_dir(),
            path.to_path_buf(),
            None,
            gen,
        ));
        self.pending_editor_offset = Some(initial_char_offset);
    }

    /// 「重新以 X 編碼開啟」:偵測錯誤的後援(對應 reloadWithCharset)。
    fn reload_reader_with_charset(&mut self, ctx: &egui::Context, charset: &str) {
        let Some(path) = self.reader.book.as_ref().map(|b| PathBuf::from(&b.path)) else {
            return;
        };
        let keep = self.reader.first_visible;
        let gen = self.next_gen();
        self.reader.loading = true;
        self.reader.search_open = false;
        self.reader.search_hits.clear();
        self.jobs.push(spawn_load(
            ctx,
            self.paths.backups_dir(),
            path,
            Some(charset.to_string()),
            gen,
        ));
        if keep > 0 {
            self.reader.focus_block = Some(keep);
        }
    }

    fn exit_editor_to_reader(&mut self, ctx: &egui::Context) {
        let p = self.editor.path.clone();
        let rect = self.reader.content_rect;
        self.route = Route::Reading;
        self.reader = ReaderState::default();
        self.reader.content_rect = rect;
        self.reader.loading = true;
        let gen = self.next_gen();
        self.jobs.push(spawn_load(ctx, self.paths.backups_dir(), p, None, gen));
    }

    fn char_offset_of_block(&self, block_index: usize) -> usize {
        let r = &self.reader;
        if r.block_off.is_empty() {
            return 0;
        }
        let i = block_index.min(r.block_off.len() - 1);
        search::byte_to_char(&r.text, r.block_off[i])
    }

    /// 段落 index → 全書比例(以原文字元 offset 為準;對應 progressOf)。
    fn progress_of_block(&self, block_index: usize) -> f32 {
        let r = &self.reader;
        if r.block_off.is_empty() {
            return 0.0;
        }
        (self.char_offset_of_block(block_index) as f32 / r.total_chars.max(1) as f32).clamp(0.0, 1.0)
    }

    /// 全書比例 → 段落 index(對應 blockIndexForProgress)。
    fn block_index_for_progress(&self, fraction: f32) -> usize {
        let r = &self.reader;
        if r.block_off.is_empty() {
            return 0;
        }
        let target = (fraction.clamp(0.0, 1.0) * r.total_chars as f32) as usize;
        let (mut lo, mut hi) = (0usize, r.block_off.len() - 1);
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if self.char_offset_of_block(mid) <= target {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        lo
    }

    // ── 高度表:虛擬化用的估算(中文全形 1em、半形 0.55em)────────────────────

    /// 以實際排版高度更新高度表;只更新到目前可見段(其上方的 tops 不變,畫面不會跳)。
    fn apply_measured_heights(&mut self, measured: &[(usize, f32)]) {
        let r = &mut self.reader;
        let mut changed = false;
        for &(i, h) in measured {
            if let Some(slot) = r.heights.get_mut(i) {
                if (*slot - h).abs() > 0.5 {
                    *slot = h;
                    changed = true;
                }
            }
        }
        if changed {
            let (tops, total) = prefix_tops(&r.heights);
            r.tops = tops;
            r.total_height = total.max(1.0);
        }
    }

    fn rebuild_heights(&mut self, wrap: f32, font_px: f32, line_h: f32, spacing: f32) {
        let sig = (wrap, font_px, line_h, spacing);
        if self.reader.metrics_sig == sig && !self.reader.tops.is_empty() {
            return;
        }
        let per_em = (wrap / font_px.max(1.0)).max(4.0);
        self.reader.heights = self
            .reader
            .blocks
            .iter()
            .map(|b| estimate_height(&b.text, per_em, line_h, spacing))
            .collect();
        let (tops, total) = prefix_tops(&self.reader.heights);
        self.reader.tops = tops;
        self.reader.total_height = total.max(1.0);
        self.reader.metrics_sig = sig;
    }

    // ── 書庫(對應 LibraryScreen + LibraryViewModel)──────────────────────────

    fn add_book(&mut self, ctx: &egui::Context, p: &Path) {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let lower = name.to_lowercase();
        if lower.ends_with(".md") || lower.ends_with(".markdown") {
            self.toast(ctx, "已取消 Markdown 支援,請選 TXT 文字檔");
            return;
        }
        let path_str = p.to_string_lossy().into_owned();
        if self.library.find(&path_str).is_some() {
            self.toast(ctx, format!("已在書庫:{name}"));
            return;
        }
        let mut book = Book::new(path_str, name.clone());
        book.size_bytes = fileio::size_bytes(p).max(0);
        self.library.upsert(book);
        self.library.save();
        self.toast(ctx, format!("已加入:{name}"));
    }

    fn render_library(&mut self, ctx: &egui::Context) {
        let pal = palette_of(self.prefs().theme);
        let mut picked: Option<PathBuf> = None;
        egui::TopBottomPanel::top("lib_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("書庫").heading());
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("＋ 新增檔案").clicked() {
                        picked = rfd::FileDialog::new()
                            .add_filter("文字檔", &["txt", "text"])
                            .pick_file();
                    }
                });
            });
        });
        if let Some(p) = picked {
            self.add_book(ctx, &p);
        }

        let mut open: Option<String> = None;
        let mut remove: Option<String> = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            let books = self.library.books().to_vec();
            if books.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    ui.label(RichText::new("尚無書籍").size(16.0));
                    ui.add_space(4.0);
                    ui.label(RichText::new("點右上角「新增檔案」選 TXT 文字檔").color(pal.subtle));
                });
                return;
            }
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(4.0);
                for book in &books {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&book.title).strong().size(15.0));
                            ui.label(
                                RichText::new(format!(
                                    "文字 · {} · {}",
                                    book.charset_name,
                                    format_size(book.size_bytes)
                                ))
                                .color(pal.subtle)
                                .size(12.0),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("🗑 移除").clicked() {
                                remove = Some(book.path.clone());
                            }
                            if ui.button("打開").clicked() {
                                open = Some(book.path.clone());
                            }
                        });
                    });
                    ui.separator();
                }
            });
        });
        if let Some(p) = remove {
            self.library.remove(&p);
            self.library.save();
        }
        if let Some(p) = open {
            self.open_reader(ctx, Path::new(&p));
        }
    }

    // ── 閱讀畫面(對應 ReaderScreen + ReaderControlsPanel + SearchBar)────────

    fn render_reader(&mut self, ctx: &egui::Context) {
        let prefs = self.prefs();
        let pal = palette_of(prefs.theme);
        let line_h = prefs.font_size_sp as f32 * prefs.line_height_multiplier;
        let spacing = prefs.paragraph_spacing_dp as f32;
        let pad = prefs.horizontal_padding_dp as f32;
        let font = FontId::proportional(prefs.font_size_sp as f32);
        let justify = prefs.text_align == TextAlignName::Justify;

        let screen_w = ctx.screen_rect().width();
        let wrap = self
            .reader
            .layout_wrap
            .unwrap_or_else(|| (screen_w - pad * 2.0 - SCROLLBAR_W).max(40.0));
        let busy = self.reader.loading
            || self.reader.error.is_some()
            || self.reader.blocks.is_empty();
        if !busy {
            self.rebuild_heights(wrap, prefs.font_size_sp as f32, line_h, spacing);
        }

        let current = self.reader.first_visible;
        let progress = self.progress_of_block(current);
        let chapter = self.current_chapter_title();
        let cur_charset = self
            .reader
            .book
            .as_ref()
            .map(|b| b.charset_name.clone())
            .unwrap_or_else(|| "UTF-8".into());
        let book_title = self
            .reader
            .book
            .as_ref()
            .map(|b| b.title.clone())
            .unwrap_or_else(|| "載入中…".into());
        let search_label = if self.reader.searching {
            "搜尋中…".to_string()
        } else if self.reader.search_query.is_empty() {
            String::new()
        } else if self.reader.search_hits.is_empty() {
            "無符合".to_string()
        } else {
            format!(
                "{}/{}{}",
                (self.reader.search_active.max(0) + 1) as usize,
                self.reader.search_hits.len(),
                if self.reader.search_truncated { "+" } else { "" }
            )
        };
        let search_open = self.reader.search_open;
        let controls_open = self.reader.controls_open;
        let has_mark = self.reader.mark.is_some();
        let hit_count = self.reader.search_hits.len();

        let mut a = ReaderActs::default();

        // 頂欄:chrome(控制面板開 / 搜尋開)時顯示完整列,否則精簡列 + 目前章節。
        if search_open || controls_open {
            egui::TopBottomPanel::top("reader_top").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if search_open {
                        if ui.button("✕").on_hover_text("關閉搜尋").clicked() {
                            a.close_search = true;
                        }
                        ui.label(RichText::new("搜尋").strong());
                        let resp = ui.add_sized(
                            [280.0, 20.0],
                            TextEdit::singleline(&mut self.reader.search_query)
                                .hint_text("搜尋全文…"),
                        );
                        resp.request_focus();
                        if resp.changed() {
                            self.reader.search_deadline = Some(ui.input(|i| i.time) + 0.25);
                            if self.reader.search_query.is_empty() {
                                self.reader.search_hits.clear();
                                self.reader.search_active = -1;
                                self.reader.searching = false;
                            }
                        }
                        ui.label(RichText::new(search_label).color(pal.subtle).size(12.0));
                        ui.add_enabled_ui(hit_count > 0, |ui| {
                            if ui.button("↑").on_hover_text("上一個").clicked() {
                                a.search_prev = true;
                            }
                            if ui.button("↓").on_hover_text("下一個").clicked() {
                                a.search_next = true;
                            }
                        });
                    } else {
                        if ui.button("← 書庫").clicked() {
                            a.back = true;
                        }
                        ui.label(RichText::new(book_title).strong());
                        if has_mark && ui.button("取消定位").clicked() {
                            a.clear_mark = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("✕").clicked() {
                                a.close_controls = true;
                            }
                        });
                    }
                });
            });
        } else {
            egui::TopBottomPanel::top("reader_top_min").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("← 書庫").clicked() {
                        a.back = true;
                    }
                    if let Some(c) = chapter.clone() {
                        ui.label(RichText::new(c).color(pal.subtle).size(12.0));
                    }
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(format!("{:.1}%", progress * 100.0));
                        if ui.button("⚙").on_hover_text("控制面板").clicked() {
                            a.toggle_controls = true;
                        }
                    });
                });
            });
        }

        // 底面板:位置拉把 + 工具(對應 ReaderControlsPanel)。
        if controls_open {
            egui::TopBottomPanel::bottom("reader_controls").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let mut v = progress;
                    if ui
                        .add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false))
                        .changed()
                    {
                        a.seek = Some(v);
                    }
                    ui.label(format!("{:.1}%", v * 100.0));
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("工具").color(pal.subtle));
                    if ui.button("🔍 搜尋").clicked() {
                        a.open_search = true;
                    }
                    if ui.button("☰ 目錄").clicked() {
                        a.open_toc = true;
                    }
                    if ui.button("✎ 編輯").clicked() {
                        a.edit = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("主題").color(pal.subtle));
                    for (label, t) in [
                        ("明亮", ReadingThemeName::Light),
                        ("夜間", ReadingThemeName::Dark),
                        ("米色", ReadingThemeName::Sepia),
                    ] {
                        if ui.selectable_label(prefs.theme == t, label).clicked() {
                            a.theme = Some(t);
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("字型大小:{} sp", prefs.font_size_sp)).color(pal.text),
                    );
                    let mut fs = prefs.font_size_sp as f32;
                    if ui
                        .add(egui::Slider::new(&mut fs, 12.0..=40.0).show_value(false))
                        .changed()
                    {
                        a.font_size = Some(fs.round().clamp(12.0, 40.0) as i32);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("行距:{:.2}", prefs.line_height_multiplier))
                            .color(pal.text),
                    );
                    let mut lh = prefs.line_height_multiplier;
                    if ui
                        .add(egui::Slider::new(&mut lh, 1.0..=2.2).show_value(false))
                        .changed()
                    {
                        a.line_height = Some(lh);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("段落間距:{} dp", prefs.paragraph_spacing_dp))
                            .color(pal.text),
                    );
                    let mut sp = prefs.paragraph_spacing_dp as f32;
                    if ui
                        .add(egui::Slider::new(&mut sp, 0.0..=40.0).show_value(false))
                        .changed()
                    {
                        a.para_spacing = Some(sp.round().clamp(0.0, 40.0) as i32);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("左右邊距:{} dp", prefs.horizontal_padding_dp))
                            .color(pal.text),
                    );
                    let mut hp = prefs.horizontal_padding_dp as f32;
                    if ui
                        .add(egui::Slider::new(&mut hp, 0.0..=48.0).show_value(false))
                        .changed()
                    {
                        a.h_padding = Some(hp.round().clamp(0.0, 48.0) as i32);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("對齊:").color(pal.subtle));
                    for (label, ta) in
                        [("靠左", TextAlignName::Left), ("左右對齊", TextAlignName::Justify)]
                    {
                        if ui.selectable_label(prefs.text_align == ta, label).clicked() {
                            a.align = Some(ta);
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("重新以其他編碼開啟(目前:{cur_charset})"))
                            .color(pal.subtle),
                    );
                    for cs in SUPPORTED_CHARSETS {
                        if ui.small_button(cs).clicked() {
                            a.reload_charset = Some(cs.to_string());
                        }
                    }
                });
            });
        } else {
            egui::TopBottomPanel::bottom("reader_status").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("中央點按:控制面板 · 左右側:翻頁 · 長按:編輯定位")
                            .color(pal.subtle)
                            .size(11.0),
                    );
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        ui.label(format!("{:.1}%", progress * 100.0));
                    });
                });
            });
        }

        self.apply_reader_acts(a, ctx);

        // 目錄彈窗(對應 TocSheetContent)。
        if self.reader.toc_open {
            let toc = self.reader.toc.clone();
            let cur = self.current_toc_index();
            let mut still_open = true;
            let mut select: Option<usize> = None;
            let scroll_pending = std::mem::take(&mut self.reader.toc_scroll_pending);
            let accent = pal.accent;
            egui::Window::new("章節目錄")
                .open(&mut still_open)
                .resizable(true)
                .default_width(440.0)
                .default_height(420.0)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    if toc.is_empty() {
                        ui.label("未偵測到章節(章節行請以「第X章/回/節/卷…」開頭、前後留空行)");
                    } else {
                        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                            for (i, t) in toc.iter().enumerate() {
                                let is_cur = Some(i) == cur;
                                let text: RichText = if is_cur {
                                    RichText::new(t.title.clone()).color(accent).strong()
                                } else {
                                    RichText::new(t.title.clone())
                                };
                                let resp = ui.selectable_label(false, text);
                                if is_cur {
                                    // 目前章節:畫一個勾(不靠字型字形,避免缺字形變方塊)。
                                    paint_check(ui, resp.rect, accent);
                                }
                                if scroll_pending && is_cur {
                                    // 開啟目錄時捲到目前章節(對應 initialFirstVisibleItemIndex)。
                                    resp.scroll_to_me(Some(Align::Center));
                                }
                                if resp.clicked() {
                                    select = Some(t.block_index);
                                }
                            }
                        });
                    }
                });
            self.reader.toc_open = still_open;
            if let Some(b) = select {
                self.reader.toc_open = false;
                self.reader.focus_block = Some(b);
            }
        }

        if busy {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    if self.reader.loading {
                        ui.spinner();
                    } else if let Some(err) = self.reader.error.clone() {
                        ui.label(err);
                        ui.label(RichText::new("可返回書庫重試").color(pal.subtle));
                        if ui.button("返回書庫").clicked() {
                            self.route = Route::Library;
                        }
                    } else {
                        ui.label(RichText::new("(空檔案)").color(pal.subtle));
                    }
                });
            });
            if self.reader.loading {
                ctx.request_repaint_after(std::time::Duration::from_millis(80));
            }
            return;
        }

        // 命中 → 段落分組(供高亮)。
        let mut hits_by_block: HashMap<usize, Vec<(usize, usize, bool)>> = HashMap::new();
        for (i, h) in self.reader.search_hits.iter().enumerate() {
            let active = i as isize == self.reader.search_active;
            hits_by_block
                .entry(h.block_index)
                .or_default()
                .push((h.start_in_block, h.end_in_block, active));
        }

        // 程式化跳轉(目錄/搜尋/開檔還原/翻頁)。
        let mut override_y: Option<f32> = self.reader.scroll_override.take();
        if let Some(b) = self.reader.focus_block.take() {
            let i = b.min(self.reader.tops.len().saturating_sub(1));
            let top = if i == 0 {
                0.0
            } else {
                self.reader.tops[i - 1]
            };
            override_y = Some(top.max(0.0));
        }

        let blocks = std::mem::take(&mut self.reader.blocks);
        let tops = std::mem::take(&mut self.reader.tops);
        let total = self.reader.total_height;
        let mark = self.reader.mark;
        let accent = pal.accent;
        let text_color = pal.text;

        let mut sa = ScrollArea::vertical()
            .id_salt("reader_content")
            .auto_shrink([false, false]);
        if let Some(y) = override_y {
            sa = sa.scroll_offset(vec2(0.0, y));
        }

        let mut scroll_now = self.reader.scroll_y;
        let mut first_visible = self.reader.first_visible;
        let mut view_h = self.reader.view_h;
        let mut content_rect: Option<egui::Rect> = None;
        let mut measured: Vec<(usize, f32)> = Vec::new();
        let mut layout_wrap = wrap;

        egui::CentralPanel::default().show(ctx, |ui| {
            let out = sa.show_viewport(ui, |ui: &mut Ui, viewport: egui::Rect| {
                ui.set_height(total);
                let max_rect = ui.max_rect();
                let origin = max_rect.min;
                let width = max_rect.width();
                let scroll = viewport.min.y;
                let vis_h = viewport.height();
                let n = blocks.len();
                let wrap_w = (width - pad * 2.0).max(40.0);
                let first = block_at(&tops, scroll).min(n.saturating_sub(1));
                // 從第一個可見段的頂端起,以「實際排版高度」逐段往下排,
                // 不靠估算的 tops 定位後續段落(估算偏差會造成重疊或空白)。
                let mut top = if first == 0 { 0.0 } else { tops[first - 1] };
                let mut i = first;
                while i < n && top < scroll + vis_h {
                    let p = &blocks[i];
                    let job = paragraph_job(
                        &p.text,
                        font.clone(),
                        line_h,
                        text_color,
                        justify,
                        wrap_w,
                        hits_by_block.get(&i).map(|v| v.as_slice()).unwrap_or(&[]),
                        accent,
                        mark == Some(i),
                    );
                    let g: Arc<Galley> = ui.fonts(|f| f.layout_job(job));
                    let h = g.size().y.max(line_h) + spacing;
                    let y = origin.y + top + spacing * 0.5;
                    ui.painter().galley(pos2(origin.x + pad, y), g, text_color);
                    measured.push((i, h));
                    top += h;
                    i += 1;
                }
                layout_wrap = wrap_w;
                scroll_now = scroll;
                first_visible = first;
                view_h = vis_h;
                (scroll, first)
            });
            // 手勢感應區:可見視口矩形(排除捲動條)。
            content_rect = Some(out.inner_rect);
        });

        self.reader.blocks = blocks;
        self.reader.tops = tops;
        self.reader.first_visible = first_visible;
        self.reader.content_rect = content_rect;
        self.reader.scroll_y = scroll_now;
        self.reader.view_h = view_h;
        // 排版寬度變了 → 下一幀以新寬度重估(metrics_sig 不符會重建);
        // 否則把本幀量到的實際高度寫回高度表。
        if (layout_wrap - wrap).abs() > 0.5 {
            self.reader.layout_wrap = Some(layout_wrap);
        } else {
            self.apply_measured_heights(&measured);
        }

        // 手勢:中央 40% 切換控制面板、左右各 30% 翻頁、長按設定編輯定位段。
        self.handle_reader_gesture(ctx);

        // 存位置 debounce 500ms(對應 saveScrollIndex)。
        if first_visible != self.reader.saved_scroll_index {
            self.reader.saved_scroll_index = first_visible;
            self.reader.scroll_save_deadline = Some(ctx.input(|i| i.time) + 0.5);
        }
        if let Some(deadline) = self.reader.scroll_save_deadline {
            if ctx.input(|i| i.time) >= deadline {
                self.flush_scroll_save();
            }
        }

        // 搜尋 debounce → 背景掃描(對應 runSearch 的 delay(250))。
        if let Some(deadline) = self.reader.search_deadline {
            if ctx.input(|i| i.time) >= deadline
                && !self.reader.search_query.is_empty()
                && !self.reader.searching
            {
                self.reader.search_deadline = None;
                self.reader.searching = true;
                let gen = self.next_gen();
                let q = self.reader.search_query.clone();
                let text = self.reader.text.clone();
                let offs = self.reader.block_off.clone();
                self.jobs.push(spawn_search(ctx, text, q, offs, gen));
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    /// 把待存的閱讀位置寫回書庫(debounce 到期或關窗時)。
    fn flush_scroll_save(&mut self) {
        if self.reader.scroll_save_deadline.take().is_none() {
            return;
        }
        let idx = self.reader.saved_scroll_index;
        let p = self.reader.book.as_ref().map(|b| b.path.clone());
        if let Some(b) = self.reader.book.as_mut() {
            b.scroll_index = idx;
        }
        if let Some(p) = p {
            self.library.update(&p, |b| Book {
                scroll_index: idx,
                ..b.clone()
            });
            self.library.save();
        }
    }

    /// 點按分區翻頁/開面板 + 長按定位(對應 detectTapGestures)。
    /// 直接讀 raw pointer 事件(不靠 widget 的 clicked,避免被 ScrollArea 吞掉),
    /// 用可見視口矩形 `content_rect` 做命中測試。
    fn handle_reader_gesture(&mut self, ctx: &egui::Context) {
        let Some(rect) = self.reader.content_rect else {
            return;
        };
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let now = ctx.input(|i| i.time);
        let pressed = ctx.input(|i| i.pointer.button_pressed(PointerButton::Primary));
        let released = ctx.input(|i| i.pointer.button_released(PointerButton::Primary));

        if pressed {
            if let Some(p) = ctx.input(|i| i.pointer.press_origin()) {
                if rect.contains(p) {
                    self.reader.press = Some((now, p));
                }
            }
            return;
        }
        if released {
            let Some((t0, p0)) = self.reader.press.take() else {
                return;
            };
            let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) else {
                return;
            };
            let dt = now - t0;
            let moved = (pos - p0).length();
            if !rect.contains(p0) || !rect.contains(pos) {
                return;
            }
            if moved > 12.0 {
                return; // 拖曳=捲動,不當作手勢
            }
            if dt >= 0.45 {
                // 長按:設定/取消「編輯定位段」。
                let y = pos.y - rect.min.y + self.reader.scroll_y;
                let idx = block_at(&self.reader.tops, y)
                    .min(self.reader.blocks.len().saturating_sub(1));
                self.reader.mark = if self.reader.mark == Some(idx) {
                    None
                } else {
                    Some(idx)
                };
                self.toast(
                    ctx,
                    if self.reader.mark.is_some() {
                        "已設編輯定位"
                    } else {
                        "已取消定位"
                    },
                );
            } else {
                // 點按:中央 40% 切換控制面板、左右各 30% 翻頁。
                let fx = (pos.x - rect.min.x) / rect.width();
                if fx < 0.30 {
                    self.page_turn(rect.height(), -1);
                } else if fx > 0.70 {
                    self.page_turn(rect.height(), 1);
                } else {
                    self.reader.controls_open = !self.reader.controls_open;
                }
            }
        }
    }

    /// 以目前捲動位置為基準捲一個視口高(對應 pageTurn 的 scrollBy)。
    /// 舊版把「視口可放幾行」當成「往後跳幾段」,一段多行時會一次跳過好幾頁。
    fn page_turn(&mut self, view_h: f32, dir: i32) {
        let prefs = self.prefs_store.prefs();
        let line_h = prefs.font_size_sp as f32 * prefs.line_height_multiplier;
        let r = &mut self.reader;
        let target = page_turn_target(r.scroll_y, view_h, line_h, r.total_height, dir);
        r.focus_block = None;
        r.scroll_override = Some(target);
    }

    fn apply_reader_acts(&mut self, a: ReaderActs, ctx: &egui::Context) {
        if a.back {
            self.flush_scroll_save();
            self.route = Route::Library;
            return;
        }
        if a.close_controls || a.open_search || a.open_toc {
            self.reader.controls_open = false;
        }
        if a.toggle_controls {
            self.reader.controls_open = !self.reader.controls_open;
        }
        if a.close_search {
            self.reader.search_open = false;
            self.reader.search_query.clear();
            self.reader.search_hits.clear();
            self.reader.search_active = -1;
            self.reader.searching = false;
            self.reader.search_deadline = None;
        }
        if a.clear_mark {
            self.reader.mark = None;
        }
        if a.open_search {
            self.reader.search_open = true;
        }
        if a.open_toc {
            self.reader.toc_open = true;
            self.reader.toc_scroll_pending = true;
        }
        if a.search_prev {
            self.search_prev();
        }
        if a.search_next {
            self.search_next();
        }
        if let Some(t) = a.theme {
            self.set_prefs(|p| p.theme = t);
        }
        if let Some(v) = a.font_size {
            self.set_prefs(|p| p.font_size_sp = v);
        }
        if let Some(v) = a.line_height {
            self.set_prefs(|p| p.line_height_multiplier = v);
        }
        if let Some(v) = a.para_spacing {
            self.set_prefs(|p| p.paragraph_spacing_dp = v);
        }
        if let Some(v) = a.h_padding {
            self.set_prefs(|p| p.horizontal_padding_dp = v);
        }
        if let Some(t) = a.align {
            self.set_prefs(|p| p.text_align = t);
        }
        if let Some(cs) = a.reload_charset {
            self.reload_reader_with_charset(ctx, &cs);
        }
        if let Some(f) = a.seek {
            let b = self.block_index_for_progress(f);
            self.reader.focus_block = Some(b);
        }
        if let Some(b) = a.toc_select {
            self.reader.focus_block = Some(b);
        }
        if a.edit {
            let p = self.reader.book.as_ref().map(|b| b.path.clone());
            if let Some(p) = p {
                let target = self.reader.mark.unwrap_or(self.reader.first_visible);
                let off = self.char_offset_of_block(target);
                self.open_editor(ctx, Path::new(&p), off);
            }
        }
    }

    fn current_chapter_title(&self) -> Option<String> {
        let idx = self.current_toc_index()?;
        self.reader.toc.get(idx).map(|t| t.title.clone())
    }

    /// 目前閱讀位置對應的章節(最後一個 blockIndex ≤ 可見段;對應 currentTocIndex)。
    fn current_toc_index(&self) -> Option<usize> {
        let vis = self.reader.first_visible;
        let mut idx: Option<usize> = None;
        for (i, t) in self.reader.toc.iter().enumerate() {
            if t.block_index <= vis {
                idx = Some(i);
            } else {
                break;
            }
        }
        idx
    }

    fn search_next(&mut self) {
        let n = self.reader.search_hits.len();
        if n == 0 {
            return;
        }
        let next = (self.reader.search_active.max(0) as usize + 1) % n;
        self.reader.search_active = next as isize;
        self.reader.focus_block = Some(self.reader.search_hits[next].block_index);
    }

    fn search_prev(&mut self) {
        let n = self.reader.search_hits.len();
        if n == 0 {
            return;
        }
        let prev = if self.reader.search_active <= 0 {
            n - 1
        } else {
            self.reader.search_active as usize - 1
        };
        self.reader.search_active = prev as isize;
        self.reader.focus_block = Some(self.reader.search_hits[prev].block_index);
    }
}

// ─── 編輯畫面(對應 EditorScreen + EditorViewModel)──────────────────────────

#[derive(Default)]
struct EditorActs {
    back: bool,
    save: bool,
    undo: bool,
    redo: bool,
    open_search: bool,
    close_search: bool,
    search_prev: bool,
    search_next: bool,
    charset: Option<String>,
    prev_page: bool,
    next_page: bool,
    jump: bool,
    conflict_overwrite: bool,
    conflict_reload: bool,
    conflict_cancel: bool,
    leave_save: bool,
    leave_discard: bool,
    leave_cancel: bool,
}

impl MoonReader {
    fn render_editor(&mut self, ctx: &egui::Context) {
        let prefs = self.prefs();
        let pal = palette_of(prefs.theme);
        let ed = &self.editor;
        let title = format!("編輯 · 文字{}", if ed.dirty { " ●未存檔" } else { "" });
        let (page, page_count) = (ed.page, ed.pages.len());
        let (can_undo, can_redo) = (ed.history.can_undo(), ed.history.can_redo());
        let charset = ed.charset.clone();
        let search_open = ed.search_open;
        let search_label = if ed.search_query.is_empty() {
            String::new()
        } else if ed.search_hits.is_empty() {
            "無符合".to_string()
        } else {
            format!(
                "{}/{}{}",
                (ed.search_active.max(0) + 1) as usize,
                ed.search_hits.len(),
                if ed.search_truncated { "+" } else { "" }
            )
        };
        let hit_count = ed.search_hits.len();
        let jump_ok = ed
            .jump_input
            .parse::<usize>()
            .map(|n| (1..=page_count).contains(&n))
            .unwrap_or(false);

        let mut a = EditorActs::default();

        egui::TopBottomPanel::top("editor_top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if search_open {
                    if ui.button("✕").on_hover_text("關閉搜尋").clicked() {
                        a.close_search = true;
                    }
                    ui.label(RichText::new("搜尋(本頁)").strong());
                } else {
                    if ui.button("← 閱讀").clicked() {
                        a.back = true;
                    }
                    ui.label(RichText::new(title).strong().size(13.0));
                    if ui.button("🔍").on_hover_text("搜尋 (Ctrl+F)").clicked() {
                        a.open_search = true;
                    }
                    ui.add_enabled_ui(can_undo, |ui| {
                        if ui.button("↶ 還原").on_hover_text("Ctrl+Z").clicked() {
                            a.undo = true;
                        }
                    });
                    ui.add_enabled_ui(can_redo, |ui| {
                        if ui.button("↷ 重做").on_hover_text("Ctrl+Y").clicked() {
                            a.redo = true;
                        }
                    });
                    egui::ComboBox::from_id_salt("editor_charset")
                        .selected_text(charset.clone())
                        .show_ui(ui, |ui| {
                            for cs in SUPPORTED_CHARSETS {
                                if ui.selectable_label(cs == charset, cs).clicked() {
                                    a.charset = Some(cs.to_string());
                                }
                            }
                        });
                    if ui.button("💾 存檔").on_hover_text("Ctrl+S").clicked() {
                        a.save = true;
                    }
                }
            });
            if search_open {
                ui.horizontal(|ui| {
                    let resp = ui.add_sized(
                        [320.0, 20.0],
                        TextEdit::singleline(&mut self.editor.search_query).hint_text("搜尋本頁…"),
                    );
                    resp.request_focus();
                    if resp.changed() {
                        let q = self.editor.search_query.clone();
                        self.editor_update_search(&q, true);
                    }
                    ui.label(RichText::new(search_label).color(pal.subtle).size(12.0));
                    ui.add_enabled_ui(hit_count > 0, |ui| {
                        if ui.button("↑").clicked() {
                            a.search_prev = true;
                        }
                        if ui.button("↓").clicked() {
                            a.search_next = true;
                        }
                    });
                });
            }
        });

        // 分頁導覽列(對應 PageBar)。
        if page_count > 1 {
            egui::TopBottomPanel::bottom("editor_pagebar").show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(page > 0, |ui| {
                        if ui.button("◀ 上一頁").clicked() {
                            a.prev_page = true;
                        }
                    });
                    ui.label(format!("第 {} / {} 頁", page + 1, page_count));
                    ui.add_enabled_ui(page + 1 < page_count, |ui| {
                        if ui.button("下一頁 ▶").clicked() {
                            a.next_page = true;
                        }
                    });
                    ui.add_sized(
                        [64.0, 20.0],
                        TextEdit::singleline(&mut self.editor.jump_input).hint_text("頁碼"),
                    );
                    if ui.add_enabled(jump_ok, egui::Button::new("跳")).clicked() {
                        a.jump = true;
                    }
                });
            });
        }

        // 編輯主區:一次只掛一頁(≤6 萬字),避開大檔卡頓。
        let hits: Vec<(usize, usize, bool)> = {
            let mut v: Vec<(usize, usize, bool)> =
                self.editor.search_hits.iter().map(|m| (m.byte_start, m.byte_end, false)).collect();
            if let Some(i) = usize::try_from(self.editor.search_active).ok() {
                if let Some(m) = v.get_mut(i) {
                    m.2 = true;
                }
            }
            v
        };
        let accent = pal.accent;
        let text_color = pal.text;
        let editor_font = FontId::proportional(16.0);
        let editor_line_h = 26.0;
        let mut buffer_changed = false;

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.editor.loading {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.spinner();
                });
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
                return;
            }
            if let Some(err) = self.editor.error.clone() {
                ui.vertical_centered(|ui| {
                    ui.add_space(60.0);
                    ui.label(err);
                });
                return;
            }

            let id = ui.id().with("editor_text");
            // 程式化游標定位:show 前寫入 state(對應 cursorFocusToken)。
            if let Some((s, e)) = self.editor.focus_pending.take() {
                set_cursor(ui.ctx(), id, Some((s, e)));
            }

            let mut layouter = |ui: &Ui, text: &str, wrap_width: f32| {
                let mut job = LayoutJob::simple(
                    text.to_string(),
                    editor_font.clone(),
                    text_color,
                    wrap_width,
                );
                job.wrap = TextWrapping {
                    max_width: wrap_width,
                    break_anywhere: true,
                    ..Default::default()
                };
                job.break_on_newline = true;
                let base = TextFormat {
                    font_id: editor_font.clone(),
                    color: text_color,
                    line_height: Some(editor_line_h),
                    ..Default::default()
                };
                job.sections = build_sections(text, &hits, base, accent);
                ui.fonts(|f| f.layout_job(job))
            };

            let out = TextEdit::multiline(&mut self.editor.buffer)
                .id(id)
                .font(editor_font.clone())
                .margin(6.0)
                .desired_width(f32::INFINITY)
                .layouter(&mut layouter)
                .show(ui);

            if let Some(cr) = out.cursor_range {
                let n = self.editor.buffer.chars().count();
                let s = cr.primary.ccursor.index.min(n);
                let e = cr.secondary.ccursor.index.min(n);
                self.editor.commit_sel = (s, e);
            }
            buffer_changed = self.editor.buffer != self.editor.last_buffer;
        });

        if buffer_changed {
            // 打字:debounce 450ms 後把穩定結果記進歷史並併入底稿。
            self.editor.last_buffer = self.editor.buffer.clone();
            self.editor.commit_deadline = Some(ctx.input(|i| i.time) + 0.45);
            // 命中區間是 buffer 的位元組 offset,內容一變就過期:重算(不搶游標)。
            if self.editor.search_open && !self.editor.search_query.is_empty() {
                let q = self.editor.search_query.clone();
                self.editor_update_search(&q, false);
            }
        }

        // debounce 提交 + 髒重算。
        if let Some(deadline) = self.editor.commit_deadline {
            if ctx.input(|i| i.time) >= deadline {
                self.editor.commit_deadline = None;
                let (s, e) = self.editor.commit_sel;
                self.editor_commit(s, e);
            }
        }
        self.editor.refresh_dirty();

        // 鍵盤:Ctrl+S / Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y / Ctrl+F。
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::S))) {
            a.save = true;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Z))) {
            a.undo = true;
        }
        if ctx.input_mut(|i| {
            i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::Z))
                || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::Y))
        }) {
            a.redo = true;
        }
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::CTRL, Key::F))) {
            a.open_search = true;
        }

        self.apply_editor_acts(a, ctx);

        // 衝突對話框(對應 AlertDialog「外部修改衝突」)。
        if self.editor.conflict {
            let mut acts = EditorActs::default();
            egui::Window::new("外部修改衝突")
                .id(egui::Id::new("conflict_modal"))
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("這個檔案在你編輯期間已被其他程式修改。\n請選擇:");
                    ui.add_space(6.0);
                    if ui.button("覆蓋(我的);先備份外部版").clicked() {
                        acts.conflict_overwrite = true;
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui.button("改用外部版").clicked() {
                            acts.conflict_reload = true;
                        }
                        if ui.button("取消").clicked() {
                            acts.conflict_cancel = true;
                        }
                    });
                });
            if acts.conflict_overwrite {
                self.editor.conflict = false;
                self.editor_save(ctx, true);
            } else if acts.conflict_reload {
                self.editor.conflict = false;
                self.quit_pending = false;
                let p = self.editor.path.clone();
                self.open_editor(ctx, &p, 0);
            } else if acts.conflict_cancel {
                self.editor.conflict = false;
                self.quit_pending = false;
            }
        }

        // 未存檔離開對話框。
        if self.editor.leave_dialog {
            let mut acts = EditorActs::default();
            egui::Window::new("有未儲存的變更")
                .id(egui::Id::new("leave_modal"))
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("是否要先儲存再離開?");
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("儲存").clicked() {
                            acts.leave_save = true;
                        }
                        if ui.button("不儲存,直接離開").clicked() {
                            acts.leave_discard = true;
                        }
                        if ui.button("取消").clicked() {
                            acts.leave_cancel = true;
                        }
                    });
                });
            // 只有按下按鈕才關閉(舊版每幀無條件關閉,對話框只閃一幀)。
            if acts.leave_save || acts.leave_discard || acts.leave_cancel {
                self.editor.leave_dialog = false;
            }
            if acts.leave_save {
                self.editor_save(ctx, false);
            } else if acts.leave_discard {
                if std::mem::take(&mut self.quit_pending) {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    self.exit_editor_to_reader(ctx);
                }
            } else if acts.leave_cancel {
                self.quit_pending = false;
            }
        }

        if self.editor.commit_deadline.is_some() || self.editor.loading {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
        }
        let _ = prefs;
    }

    fn apply_editor_acts(&mut self, a: EditorActs, ctx: &egui::Context) {
        if a.close_search {
            self.editor.search_open = false;
            self.editor.search_query.clear();
            self.editor.search_hits.clear();
            self.editor.search_active = -1;
            self.editor.search_truncated = false;
        }
        if a.open_search && !self.editor.search_open {
            self.editor.search_open = true;
            let q = self.editor.search_query.clone();
            self.editor_update_search(&q, true);
        }
        if a.search_prev {
            self.editor_search_prev();
        }
        if a.search_next {
            self.editor_search_next();
        }
        if let Some(cs) = a.charset {
            self.editor.charset = cs;
        }
        if a.undo {
            self.editor_undo();
        }
        if a.redo {
            self.editor_redo();
        }
        if a.prev_page {
            self.editor_go_to_page(self.editor.page.saturating_sub(1));
        }
        if a.next_page {
            self.editor_go_to_page(self.editor.page + 1);
        }
        if a.jump {
            if let Ok(n) = self.editor.jump_input.parse::<usize>() {
                let target = n.saturating_sub(1).min(self.editor.pages.len().saturating_sub(1));
                self.editor_go_to_page(target);
                self.editor.jump_input.clear();
            }
        }
        if a.save {
            self.editor_save(ctx, false);
        }
        if a.back {
            if self.editor.dirty {
                self.editor.leave_dialog = true;
            } else {
                self.exit_editor_to_reader(ctx);
            }
        }
    }

    /// 內容沒變不進歷史;有變則記頁級節點(對應 commitPage)。
    fn editor_commit(&mut self, sel_start: usize, sel_end: usize) {
        let idx = self.editor.page;
        let Some(old) = self.editor.pages.get(idx).cloned() else {
            return;
        };
        let new = self.editor.buffer.clone();
        if old == new {
            return;
        }
        self.editor.history.record(Edit {
            page_index: idx,
            old_text: old,
            new_text: new.clone(),
            sel_start,
            sel_end,
        });
        self.editor.pages[idx] = new;
    }

    /// 把未 flush 的目前頁編輯先結算(切頁/undo/存檔前;對應 flushPending)。
    fn editor_flush_now(&mut self) {
        if self.editor.commit_deadline.take().is_some() {
            let (s, e) = self.editor.commit_sel;
            self.editor_commit(s, e);
        }
        self.editor.refresh_dirty();
    }

    fn editor_go_to_page(&mut self, target: usize) {
        self.editor_flush_now();
        if target == self.editor.page || target >= self.editor.pages.len() {
            return;
        }
        self.editor.page = target;
        self.editor.buffer = self.editor.pages[target].clone();
        self.editor.last_buffer = self.editor.buffer.clone();
        self.editor.focus_pending = Some((0, 0));
    }

    fn editor_undo(&mut self) {
        self.editor_flush_now();
        if let Some(e) = self.editor.history.undo() {
            self.editor_apply_edit(&e, true);
        }
    }

    fn editor_redo(&mut self) {
        self.editor_flush_now();
        if let Some(e) = self.editor.history.redo() {
            self.editor_apply_edit(&e, false);
        }
    }

    fn editor_apply_edit(&mut self, e: &Edit, old_side: bool) {
        let content = if old_side { &e.old_text } else { &e.new_text };
        if e.page_index < self.editor.pages.len() {
            self.editor.pages[e.page_index] = content.clone();
        }
        self.editor.page = e.page_index;
        let len = content.chars().count();
        self.editor.buffer = content.clone();
        self.editor.last_buffer = content.clone();
        self.editor.focus_pending = Some((e.sel_start.min(len), e.sel_end.min(len)));
    }

    /// focus = 是否把游標移到第一個命中(打字觸發的重算不搶游標)。
    fn editor_update_search(&mut self, q: &str, focus: bool) {
        if q.is_empty() {
            self.editor.search_hits.clear();
            self.editor.search_active = -1;
            self.editor.search_truncated = false;
            return;
        }
        // 編輯器是分頁制(≤6 萬字),同步掃即毫秒級。
        let r = search::find_all(&self.editor.buffer, q, search::MAX_RESULTS);
        self.editor.search_hits = r
            .matches
            .iter()
            .map(|m| PageMatch {
                byte_start: m.byte_start,
                byte_end: m.byte_end,
            })
            .collect();
        self.editor.search_truncated = r.truncated;
        let n = self.editor.search_hits.len() as isize;
        self.editor.search_active = if n == 0 {
            -1
        } else if focus {
            0
        } else {
            self.editor.search_active.clamp(0, n - 1)
        };
        if focus {
            self.editor_focus_active();
        }
    }

    /// 把游標移到聚焦命中處(對應 focusActiveHit)。
    fn editor_focus_active(&mut self) {
        let i = usize::try_from(self.editor.search_active.max(0)).unwrap_or(0);
        let Some(m) = self.editor.search_hits.get(i) else {
            return;
        };
        let s = search::byte_to_char(&self.editor.buffer, m.byte_start);
        let e = search::byte_to_char(&self.editor.buffer, m.byte_end);
        self.editor.focus_pending = Some((s, e));
        self.editor.commit_deadline = None;
        self.editor.commit_sel = (s, e);
    }

    fn editor_search_next(&mut self) {
        let n = self.editor.search_hits.len();
        if n == 0 {
            return;
        }
        self.editor.search_active = ((self.editor.search_active.max(0) as usize + 1) % n) as isize;
        self.editor_focus_active();
    }

    fn editor_search_prev(&mut self) {
        let n = self.editor.search_hits.len();
        if n == 0 {
            return;
        }
        self.editor.search_active = if self.editor.search_active <= 0 {
            (n - 1) as isize
        } else {
            self.editor.search_active - 1
        };
        self.editor_focus_active();
    }

    /// 存檔:衝突偵測 → 備份 → 回寫(對應 doSave)。
    fn editor_save(&mut self, ctx: &egui::Context, force: bool) {
        self.editor_flush_now();
        let doc: String = self.editor.pages.concat();
        if doc == self.editor.baseline && !force {
            self.toast(ctx, "沒有需要儲存的變更");
            return;
        }
        let path = self.editor.path.clone();
        let outcome = if force {
            self.repo.save_overwrite(&path, &doc, &self.editor.charset, true)
        } else {
            let h = self.editor.expected_hash.clone();
            self.repo.save(&path, &doc, &self.editor.charset, &h, true)
        };
        match outcome {
            SaveOutcome::Conflict => {
                self.editor.conflict = true;
            }
            SaveOutcome::Success {
                backup_created,
                new_hash,
                new_last_modified,
                new_size_bytes,
            } => {
                self.editor.baseline = doc;
                self.editor.expected_hash = new_hash.clone();
                self.editor.dirty = false;
                self.editor.conflict = false;
                let p = path.to_string_lossy().into_owned();
                let cs = self.editor.charset.clone();
                self.library.update(&p, |b| Book {
                    content_hash: new_hash,
                    last_modified_at: new_last_modified,
                    size_bytes: new_size_bytes,
                    charset_name: cs,
                    ..b.clone()
                });
                self.library.save();
                self.toast(
                    ctx,
                    if backup_created {
                        "已儲存(已備份原檔)"
                    } else {
                        "已儲存"
                    },
                );
                // 存檔成功 → 返回閱讀頁(對應 savedSignal);關窗途中則接著關閉。
                if self.quit_pending {
                    self.quit_pending = false;
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    self.exit_to_reader = true;
                }
            }
            SaveOutcome::Unencodable { charset_name, chars } => {
                self.quit_pending = false;
                let sample: String = chars.iter().collect();
                self.toast(
                    ctx,
                    format!("未存檔:{charset_name} 無法表示「{sample}」等字元,請改選 UTF-8 等編碼"),
                );
            }
            SaveOutcome::Failure(m) => {
                self.quit_pending = false;
                self.toast(ctx, format!("存檔失敗:{m}"));
            }
        }
    }
}

impl EditorState {
    /// 含「尚未 debounce 進歷史的目前頁」在內的全文是否與基準不同(對應 dirtyNow)。
    fn refresh_dirty(&mut self) {
        let mut projected: String = String::with_capacity(self.baseline.len());
        for (i, p) in self.pages.iter().enumerate() {
            if i == self.page {
                projected.push_str(&self.buffer);
            } else {
                projected.push_str(p);
            }
        }
        self.dirty = projected != self.baseline;
    }
}

// ─── 排版輔助 ─────────────────────────────────────────────────────────────────

/// 建段落 LayoutJob;命中區間以背景色分段(對應 ParagraphView 的 SpanStyle)。
#[allow(clippy::too_many_arguments)]
fn paragraph_job(
    text: &str,
    font: FontId,
    line_h: f32,
    color: Color32,
    justify: bool,
    wrap: f32,
    highlights: &[(usize, usize, bool)],
    accent: Color32,
    marked: bool,
) -> LayoutJob {
    let mut job = LayoutJob::simple(text.to_string(), font.clone(), color, wrap);
    job.wrap = TextWrapping {
        max_width: wrap,
        break_anywhere: true, // 中文連排:任何字元皆可換行
        ..Default::default()
    };
    job.break_on_newline = true;
    job.justify = justify;
    let base = TextFormat {
        font_id: font,
        color,
        line_height: Some(line_h),
        background: if marked {
            accent_alpha(accent, 38)
        } else {
            Color32::TRANSPARENT
        },
        ..Default::default()
    };
    job.sections = build_sections(text, highlights, base, accent);
    job
}

/// 依命中邊界把 [0,len) 切成 sections;active 深底、soft 淡底。
/// 命中區間可能過期(編輯器打字後、重算前的那一幀),邊界一律夾到 len 並對齊到
/// 字元邊界:egui 排版會以 byte_range 切字串,落在多位元組字元中間會 panic。
fn build_sections(
    text: &str,
    highlights: &[(usize, usize, bool)],
    base: TextFormat,
    accent: Color32,
) -> Vec<LayoutSection> {
    let len = text.len();
    let snap = |b: usize| {
        let mut b = b.min(len);
        while !text.is_char_boundary(b) {
            b -= 1;
        }
        b
    };
    if highlights.is_empty() {
        return vec![LayoutSection {
            leading_space: 0.0,
            byte_range: 0..len,
            format: base,
        }];
    }
    let mut bounds: Vec<usize> = vec![0, len];
    let highlights: Vec<(usize, usize, bool)> =
        highlights.iter().map(|&(s, e, a)| (snap(s), snap(e), a)).collect();
    for (s, e, _) in &highlights {
        bounds.push(*s);
        bounds.push(*e);
    }
    bounds.sort_unstable();
    bounds.dedup();

    let mut sections = Vec::with_capacity(bounds.len());
    for w in bounds.windows(2) {
        let (s, e) = (w[0], w[1]);
        if s >= e {
            continue;
        }
        let mut fmt = base.clone();
        let mut soft = false;
        let mut active = false;
        for (hs, he, is_active) in &highlights {
            if s >= *hs && e <= *he {
                if *is_active {
                    active = true;
                } else {
                    soft = true;
                }
            }
        }
        if active {
            fmt.background = accent_alpha(accent, 166);
        } else if soft {
            fmt.background = accent_alpha(accent, 76);
        }
        sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: s..e,
            format: fmt,
        });
    }
    if sections.is_empty() {
        sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: 0..len,
            format: base,
        });
    }
    sections
}

fn set_cursor(ctx: &egui::Context, id: egui::Id, range: Option<(usize, usize)>) {
    let mut st = TextEdit::load_state(ctx, id).unwrap_or_default();
    st.cursor.set_char_range(range.map(|(a, b)| CCursorRange::two(CCursor::new(a), CCursor::new(b))));
    TextEdit::store_state(ctx, id, st);
}

/// 在 widget 右側畫一個勾(不靠字型字形,避免缺字形時顯示成方塊)。
fn paint_check(ui: &Ui, rect: egui::Rect, color: Color32) {
    let r = rect.height().min(16.0) * 0.42f32;
    let c = egui::pos2(rect.right() + r + 4.0f32, rect.center().y);
    let stroke = egui::Stroke::new(2.0f32, color);
    // 兩段折線構成勾形:↙ 短段 + ↘ 長段。
    let p1 = egui::pos2(c.x - r, c.y);
    let p2 = egui::pos2(c.x - r * 0.35f32, c.y + r * 0.65f32);
    let p3 = egui::pos2(c.x + r, c.y - r * 0.75f32);
    ui.painter().line_segment([p1, p2], stroke);
    ui.painter().line_segment([p2, p3], stroke);
}

/// 載入 CJK 字型(中文小說必需;對應 Android 系統字型)。
fn install_cjk_font(ctx: &egui::Context) -> bool {
    const CANDIDATES: [&str; 7] = [
        "C:/Windows/Fonts/DENG.TTF", // DengXian:純 TTF、含繁體+簡體,ab_glyph 最穩
        "C:/Windows/Fonts/simsunb.ttf",
        "C:/Windows/Fonts/DENGL.TTF",
        "C:/Windows/Fonts/NotoSerifTC-VF.ttf",
        "C:/Windows/Fonts/NotoSansTC-VF.ttf",
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simsun.ttc",
    ];
    let Some(bytes) = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok()) else {
        return false;
    };
    let mut defs = FontDefinitions::default();
    defs.font_data
        .insert("moon-cjk".to_string(), FontData::from_owned(bytes));
    for fam in [FontFamily::Proportional, FontFamily::Monospace] {
        defs.families.entry(fam).or_default().push("moon-cjk".to_string());
    }
    ctx.set_fonts(defs);
    true
}

fn apply_visuals(ctx: &egui::Context, pal: &Palette, theme: ReadingThemeName) {
    let mut v = if theme == ReadingThemeName::Dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = pal.bg;
    v.window_fill = pal.bg;
    v.faint_bg_color = pal.code_bg;
    v.extreme_bg_color = pal.code_bg;
    v.override_text_color = Some(pal.text);
    ctx.set_visuals(v);
}

impl MoonReader {
    /// 關窗攔截:編輯中有未存檔變更 → 取消關閉、改跳「有未儲存的變更」對話框。
    /// 閱讀中則先把尚未 debounce 的閱讀位置寫回書庫。
    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if !ctx.input(|i| i.viewport().close_requested()) || self.allow_close {
            return;
        }
        match self.route {
            Route::Editing if !self.editor.loading => {
                self.editor_flush_now();
                if self.editor.dirty {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    self.quit_pending = true;
                    self.editor.leave_dialog = true;
                }
            }
            Route::Reading => self.flush_scroll_save(),
            _ => {}
        }
    }
}

// ─── eframe::App ─────────────────────────────────────────────────────────────

impl eframe::App for MoonReader {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.font_state == 0 {
            self.font_state = if install_cjk_font(ctx) { 1 } else { 2 };
        }
        let pal = palette_of(self.prefs().theme);
        apply_visuals(ctx, &pal, self.prefs().theme);

        // CLI 傳入檔案:自動加入並打開(對應 Intent ACTION_VIEW 的 externalUri)。
        if let Some(p) = self.cli_file.take() {
            self.add_book(ctx, &p);
            self.open_reader(ctx, &p);
        }

        self.poll_jobs();
        self.handle_close_request(ctx);

        match self.route {
            Route::Library => self.render_library(ctx),
            Route::Reading => self.render_reader(ctx),
            Route::Editing => self.render_editor(ctx),
        }

        if std::mem::replace(&mut self.exit_to_reader, false) {
            self.exit_editor_to_reader(ctx);
        }

        // toast(對應 Snackbar)。
        if let Some((msg, deadline)) = self.toast.clone() {
            if ctx.input(|i| i.time) > deadline {
                self.toast = None;
            } else {
                egui::Area::new(egui::Id::new("toast_area"))
                    .anchor(Align2::CENTER_BOTTOM, [0.0, -40.0])
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        egui::Frame::none()
                            .fill(Color32::from_rgba_unmultiplied(40, 40, 40, 235))
                            .rounding(6.0)
                            .inner_margin(10.0)
                            .show(ui, |ui| {
                                ui.label(RichText::new(msg).color(Color32::WHITE));
                            });
                    });
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> TextFormat {
        TextFormat::default()
    }

    #[test]
    fn stale_highlights_inside_multibyte_chars_are_snapped() {
        // 「中文」= 6 bytes;過期命中 (1,4) 與 (5,99) 都不在字元邊界/超出長度。
        let text = "中文";
        let secs = build_sections(text, &[(1, 4, true), (5, 99, false)], base(), Color32::RED);
        for s in &secs {
            assert!(text.is_char_boundary(s.byte_range.start), "{:?}", s.byte_range);
            assert!(text.is_char_boundary(s.byte_range.end), "{:?}", s.byte_range);
            assert!(s.byte_range.end <= text.len());
        }
        assert_eq!(secs.first().unwrap().byte_range.start, 0);
        assert_eq!(secs.last().unwrap().byte_range.end, text.len());
    }

    #[test]
    fn stale_highlights_survive_real_egui_layout() {
        // 實際走 egui 排版(舊版這裡會在切字串時 panic)。
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let job = paragraph_job(
                "編輯後變短",
                FontId::proportional(16.0),
                24.0,
                Color32::BLACK,
                false,
                200.0,
                &[(2, 7, true), (13, 40, false)],
                Color32::RED,
                false,
            );
            let g = ctx.fonts(|f| f.layout_job(job));
            assert!(g.size().y > 0.0);
        });
    }

    #[test]
    fn highlight_sections_mark_active_and_soft() {
        let text = "abcdef";
        let secs = build_sections(text, &[(1, 2, true), (3, 5, false)], base(), Color32::RED);
        let ranges: Vec<_> = secs.iter().map(|s| s.byte_range.clone()).collect();
        assert_eq!(ranges, vec![0..1, 1..2, 2..3, 3..5, 5..6]);
        assert_eq!(secs[1].format.background, accent_alpha(Color32::RED, 166));
        assert_eq!(secs[3].format.background, accent_alpha(Color32::RED, 76));
        assert_eq!(secs[0].format.background, Color32::TRANSPARENT);
    }

    #[test]
    fn block_at_treats_block_end_as_next_block_start() {
        let tops = [10.0, 30.0, 60.0];
        assert_eq!(block_at(&tops, 0.0), 0);
        assert_eq!(block_at(&tops, 9.9), 0);
        assert_eq!(block_at(&tops, 10.0), 1);
        assert_eq!(block_at(&tops, 59.0), 2);
        assert_eq!(block_at(&tops, 1000.0), 3); // 超出 → 呼叫端夾住
    }

    #[test]
    fn prefix_tops_accumulates() {
        let (tops, total) = prefix_tops(&[10.0, 20.0, 5.0]);
        assert_eq!(tops, vec![10.0, 30.0, 35.0]);
        assert_eq!(total, 35.0);
    }

    #[test]
    fn page_turn_scrolls_one_viewport_regardless_of_paragraph_count() {
        // 舊版以段落數翻頁:一段很長時一次跳過好幾頁。新版以高度計。
        let (view_h, line_h, total) = (600.0, 25.0, 10_000.0);
        assert_eq!(page_turn_target(0.0, view_h, line_h, total, 1), 575.0);
        assert_eq!(page_turn_target(575.0, view_h, line_h, total, -1), 0.0);
        // 夾在可捲動範圍內。
        assert_eq!(page_turn_target(9_300.0, view_h, line_h, total, 1), total - view_h);
        assert_eq!(page_turn_target(100.0, view_h, line_h, total, -1), 0.0);
        // 內容比視口短 → 不動。
        assert_eq!(page_turn_target(0.0, view_h, line_h, 300.0, 1), 0.0);
    }

    #[test]
    fn estimate_height_counts_wrapped_lines() {
        // 每行 10em;20 個全形字 = 2 行。
        assert_eq!(estimate_height(&"中".repeat(20), 10.0, 25.0, 12.0), 2.0 * 25.0 + 12.0);
        assert_eq!(estimate_height("", 10.0, 25.0, 12.0), 25.0 + 12.0);
    }
}
