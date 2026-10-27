//! 資料層整合測試:讀寫、衝突偵測、備份、還原、持久化。
//! 對應 Kotlin 版 `BookRepository` + `LibraryStore` + `ReaderPrefsStore` 的行為。
use moonreader_rs::encoding;
use moonreader_rs::model::{Book, ReadingThemeName, ReaderPrefs, SaveOutcome, TextAlignName};
use moonreader_rs::repo::BookRepository;
use moonreader_rs::store::{AppPaths, LibraryStore, ReaderPrefsStore};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// 每個測試獨立暫存目錄,測完自行清除。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "moonreader_test_{}_{}_{}",
            tag,
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create temp dir");
        TempDir(p)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, bytes).expect("write fixture");
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn repo_in(dir: &Path) -> BookRepository {
    BookRepository::new(dir.join("backups"))
}

// ─── 載入 ────────────────────────────────────────────────────────────────────

#[test]
fn load_utf8_and_reports_fingerprint() {
    let t = TempDir::new("load_utf8");
    let text = "第一章　劫後夢醒\n　　這是內文。\n";
    let p = t.file("book.txt", text.as_bytes());
    let r = repo_in(t.path());

    let l = r.load(&p, None).expect("load ok");
    assert_eq!(l.text, text);
    assert_eq!(l.charset_name, "UTF-8");
    assert_eq!(l.size_bytes, text.len() as i64);
    assert_eq!(l.content_hash.len(), 64); // sha256 hex
    assert!(l.content_hash.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn load_big5_file_auto_detects() {
    let t = TempDir::new("load_big5");
    let text = "第一章　劫後夢醒\n　　這是內文。\n";
    let bytes = encoding::encode(text, "Big5");
    let p = t.file("big5.txt", &bytes);
    let r = repo_in(t.path());

    let l = r.load(&p, None).expect("load ok");
    assert_eq!(l.charset_name, "Big5");
    assert_eq!(l.text, text);
}

#[test]
fn charset_override_skips_detection() {
    let t = TempDir::new("override");
    // 檔案內容是 Big5,但使用者要求以 UTF-8 解:不該報錯,且 charsetName = UTF-8。
    let text = "中文內容";
    let bytes = encoding::encode(text, "Big5");
    let p = t.file("big5.txt", &bytes);
    let r = repo_in(t.path());

    let l = r.load(&p, Some("UTF-8")).expect("load ok");
    assert_eq!(l.charset_name, "UTF-8");
    // 強解 UTF-8 會是亂碼,但不得 panic。
    assert!(!l.text.is_empty());

    // 以正確編碼重開 → 還原原文(對應「重新以 X 編碼開啟」)。
    let l2 = r.load(&p, Some("Big5")).expect("load ok");
    assert_eq!(l2.charset_name, "Big5");
    assert_eq!(l2.text, text);
}

#[test]
fn load_missing_file_errors() {
    let t = TempDir::new("missing");
    let r = repo_in(t.path());
    assert!(r.load(&t.path().join("nope.txt"), None).is_err());
}

// ─── 衝突偵測與存檔 ──────────────────────────────────────────────────────────

#[test]
fn save_writes_back_in_original_encoding() {
    let t = TempDir::new("save_big5");
    let original = "第一章\n";
    let p = t.file("big5.txt", &encoding::encode(original, "Big5"));
    let r = repo_in(t.path());

    let l = r.load(&p, None).expect("load");
    assert_eq!(l.charset_name, "Big5");

    let edited = format!("{original}新增的一行\n");
    let out = r.save(&p, &edited, "Big5", &l.content_hash, true);
    let SaveOutcome::Success { new_hash, new_size_bytes, .. } = out else {
        panic!("expected Success, got {out:?}");
    };
    assert!(new_hash != l.content_hash);
    assert_eq!(new_size_bytes as usize, encoding::encode(&edited, "Big5").len());

    // 磁碟內容仍是合法 Big5 且等於編輯結果。
    let raw = std::fs::read(&p).unwrap();
    assert_eq!(encoding::detect(&raw).text, edited);
    // 再載入的指紋 = 存檔回傳的 new_hash。
    assert_eq!(r.load(&p, None).unwrap().content_hash, new_hash);
}

#[test]
fn save_detects_external_modification_as_conflict() {
    let t = TempDir::new("conflict");
    let p = t.file("book.txt", b"original".as_slice());
    let r = repo_in(t.path());
    let l = r.load(&p, None).unwrap();

    // 外部程式改動磁碟。
    std::fs::write(&p, b"changed by another program").unwrap();

    let out = r.save(&p, "my edit", "UTF-8", &l.content_hash, true);
    assert!(matches!(out, SaveOutcome::Conflict), "got {out:?}");
    // 衝突時不得寫入。
    assert_eq!(std::fs::read(&p).unwrap(), b"changed by another program");
    let disk_hash = moonreader_rs::fileio::sha256_hex(b"changed by another program");
    assert!(r.matches_disk_state(&p, &disk_hash));
    assert!(!r.matches_disk_state(&p, &l.content_hash));
    assert!(!r.matches_disk_state(&p, ""));
}

#[test]
fn save_overwrite_ignores_hash_and_still_backs_up() {
    let t = TempDir::new("overwrite");
    let p = t.file("book.txt", b"external version".as_slice());
    let r = repo_in(t.path());

    let out = r.save_overwrite(&p, "my version", "UTF-8", true);
    let SaveOutcome::Success { backup_created, .. } = out else {
        panic!("expected Success, got {out:?}");
    };
    assert!(backup_created);
    assert_eq!(std::fs::read(&p).unwrap(), b"my version");
    // 備份保留的是「外部版」(覆寫前的磁碟現況)。
    let backups = std::fs::read_dir(t.path().join("backups")).unwrap().count();
    assert_eq!(backups, 1);
}

#[test]
fn save_without_backup_skips_backup_file() {
    let t = TempDir::new("no_backup");
    let p = t.file("book.txt", b"original".as_slice());
    let r = repo_in(t.path());
    let l = r.load(&p, None).unwrap();

    let out = r.save(&p, "edited", "UTF-8", &l.content_hash, false);
    let SaveOutcome::Success { backup_created, .. } = out else {
        panic!("expected Success, got {out:?}");
    };
    assert!(!backup_created);
    // backup=false 時完全不碰備份目錄。
    assert!(r.list_backups(p.to_str().unwrap()).is_empty());
}

#[test]
fn empty_file_first_write_makes_no_backup() {
    let t = TempDir::new("empty_first_write");
    let p = t.file("empty.txt", b"");
    let r = repo_in(t.path());
    let l = r.load(&p, None).unwrap();
    assert_eq!(l.content_hash, moonreader_rs::fileio::sha256_hex(b""));

    // 空檔第一次寫:即使 backup=true 也不該產生備份(對應 diskBytes.isNotEmpty())。
    let out = r.save(&p, "first content", "UTF-8", &l.content_hash, true);
    let SaveOutcome::Success { backup_created, .. } = out else {
        panic!("expected Success, got {out:?}");
    };
    assert!(!backup_created);
}

// ─── 備份清單與還原 ──────────────────────────────────────────────────────────

#[test]
fn list_backups_sorted_newest_first_and_scoped_to_book() {
    let t = TempDir::new("list_backups");
    let a = t.file("bookA.txt", b"a1");
    let b = t.file("bookB.txt", b"b1");
    let r = repo_in(t.path());

    let ha = r.load(&a, None).unwrap().content_hash;
    let hb = r.load(&b, None).unwrap().content_hash;
    assert!(matches!(r.save(&a, "a2", "UTF-8", &ha, true), SaveOutcome::Success { .. }));
    let ha2 = r.load(&a, None).unwrap().content_hash;
    assert!(matches!(r.save(&a, "a3", "UTF-8", &ha2, true), SaveOutcome::Success { .. }));
    assert!(matches!(r.save(&b, "b2", "UTF-8", &hb, true), SaveOutcome::Success { .. }));

    let a_backups = r.list_backups(a.to_str().unwrap());
    assert_eq!(a_backups.len(), 2, "同一秒內兩次存檔也該各留一份備份");
    assert!(
        a_backups[0].created_at >= a_backups[1].created_at,
        "新→舊排序"
    );
    assert_eq!(r.list_backups(b.to_str().unwrap()).len(), 1);
    assert!(a_backups.iter().all(|bi| bi.original_name.contains("bookA")));
}

#[test]
fn restore_backup_roundtrips_through_conflict_check() {
    let t = TempDir::new("restore");
    let p = t.file("book.txt", b"version 1");
    let r = repo_in(t.path());
    let h1 = r.load(&p, None).unwrap().content_hash;

    // 存檔產生「version 1」的備份,磁碟變成 version 2。
    assert!(matches!(
        r.save(&p, "version 2", "UTF-8", &h1, true),
        SaveOutcome::Success { .. }
    ));
    let backups = r.list_backups(p.to_str().unwrap());
    assert_eq!(backups.len(), 1);
    let backup = &backups[0];
    assert_eq!(std::fs::read(&backup.file).unwrap(), b"version 1");

    // 以「目前磁碟指紋」還原 → 成功回到 version 1。
    let h2 = r.load(&p, None).unwrap().content_hash;
    let out = r.restore_backup(&p, &backup.file, &h2);
    assert!(matches!(out, SaveOutcome::Success { .. }), "got {out:?}");
    assert_eq!(std::fs::read(&p).unwrap(), b"version 1");

    // 還原前會自動再備份一次(version 2)。
    assert_eq!(r.list_backups(p.to_str().unwrap()).len(), 2);

    // 指紋過期(檔案又被改動)→ 還原應報 Conflict,且不寫入。
    std::fs::write(&p, b"version 3 by someone else").unwrap();
    let out = r.restore_backup(&p, &backup.file, &h2);
    assert!(matches!(out, SaveOutcome::Conflict), "got {out:?}");
    assert_eq!(std::fs::read(&p).unwrap(), b"version 3 by someone else");
}

#[test]
fn restore_backup_redetects_backup_encoding() {
    let t = TempDir::new("restore_big5");
    let original = "第一章　內容";
    let p = t.file("big5.txt", &encoding::encode(original, "Big5"));
    let r = repo_in(t.path());
    let h1 = r.load(&p, None).unwrap().content_hash;
    assert!(matches!(
        r.save(&p, "改過", "Big5", &h1, true),
        SaveOutcome::Success { .. }
    ));

    let backups = r.list_backups(p.to_str().unwrap());
    assert_eq!(backups.len(), 1);
    let h2 = r.load(&p, None).unwrap().content_hash;
    assert!(matches!(
        r.restore_backup(&p, &backups[0].file, &h2),
        SaveOutcome::Success { .. }
    ));
    // 還原後磁碟仍是合法 Big5 的原文。
    let raw = std::fs::read(&p).unwrap();
    assert_eq!(encoding::detect(&raw).charset_name, "Big5");
    assert_eq!(encoding::detect(&raw).text, original);
}

// ─── 書庫持久化 ──────────────────────────────────────────────────────────────

#[test]
fn library_roundtrips_and_upserts_by_path() {
    let t = TempDir::new("library");
    let paths = AppPaths::at(t.path());

    let mut store = LibraryStore::load(&paths);
    assert!(store.books().is_empty());

    let mut book = Book::new("/books/a.txt", "書 A");
    book.size_bytes = 1234;
    book.charset_name = "Big5".into();
    assert!(store.upsert(book.clone()), "首次 upsert 應為新檔");
    store.save();

    // 重新載入 → 內容一致。
    let reloaded = LibraryStore::load(&paths);
    assert_eq!(reloaded.books().len(), 1);
    assert_eq!(reloaded.find("/books/a.txt"), Some(&book));

    // 同路徑再 upsert → 取代而非新增。
    let mut store2 = LibraryStore::load(&paths);
    let mut updated = book.clone();
    updated.title = "書 A(改名)".into();
    updated.scroll_index = 42;
    assert!(!store2.upsert(updated.clone()), "已存在應非新檔");
    store2.save();
    let again = LibraryStore::load(&paths);
    assert_eq!(again.books().len(), 1);
    assert_eq!(again.find("/books/a.txt").unwrap().title, "書 A(改名)");
    assert_eq!(again.find("/books/a.txt").unwrap().scroll_index, 42);
}

#[test]
fn library_update_and_remove() {
    let t = TempDir::new("library_update");
    let paths = AppPaths::at(t.path());
    let mut store = LibraryStore::load(&paths);
    store.upsert(Book::new("/books/a.txt", "A"));
    store.upsert(Book::new("/books/b.txt", "B"));

    store.update("/books/a.txt", |b| Book {
        scroll_index: 7,
        content_hash: "abc".into(),
        ..b.clone()
    });
    let a = store.find("/books/a.txt").unwrap().clone();
    assert_eq!(a.scroll_index, 7);
    assert_eq!(a.content_hash, "abc");
    assert_eq!(a.title, "A"); // 其他欄位不受影響

    store.remove("/books/a.txt");
    assert!(store.find("/books/a.txt").is_none());
    assert_eq!(store.books().len(), 1);
    store.save();
    assert_eq!(LibraryStore::load(&paths).books().len(), 1);
}

#[test]
fn library_ignores_corrupt_json() {
    let t = TempDir::new("library_corrupt");
    let paths = AppPaths::at(t.path());
    std::fs::write(t.path().join("library.json"), "{not json").unwrap();
    // 損毀時退回空書庫,不得 panic(對應 Kotlin 的 catch → emptyList)。
    assert!(LibraryStore::load(&paths).books().is_empty());
}

#[test]
fn book_defaults_match_kotlin_schema() {
    let b = Book::new("/p", "T");
    assert_eq!(b.charset_name, "UTF-8");
    assert_eq!(b.size_bytes, 0);
    assert_eq!(b.scroll_index, 0);
    assert_eq!(b.content_hash, "");
    assert_eq!(b.last_modified_at, 0);
    assert!(b.added_at > 0);
    assert_eq!(b.last_opened_at, b.added_at);
}

#[test]
fn book_json_tolerates_missing_fields() {
    // 舊版書庫檔缺欄位時,靠 serde(default) 補齊。
    let raw = r#"[{"path":"/p","title":"T"}]"#;
    let books: Vec<Book> = serde_json::from_str(raw).expect("lenient decode");
    assert_eq!(books[0].charset_name, "UTF-8");
    assert_eq!(books[0].scroll_index, 0);
}

// ─── 偏好持久化 ──────────────────────────────────────────────────────────────

#[test]
fn prefs_roundtrip_and_update() {
    let t = TempDir::new("prefs");
    let paths = AppPaths::at(t.path());

    let mut store = ReaderPrefsStore::load(&paths);
    assert_eq!(*store.prefs(), ReaderPrefs::default());

    store.update(|p| {
        p.font_size_sp = 26;
        p.line_height_multiplier = 1.8;
        p.theme = ReadingThemeName::Sepia;
        p.text_align = TextAlignName::Justify;
        p.paragraph_spacing_dp = 20;
        p.horizontal_padding_dp = 30;
    });

    let reloaded = ReaderPrefsStore::load(&paths);
    assert_eq!(reloaded.prefs().font_size_sp, 26);
    assert!((reloaded.prefs().line_height_multiplier - 1.8).abs() < 1e-5);
    assert_eq!(reloaded.prefs().theme, ReadingThemeName::Sepia);
    assert_eq!(reloaded.prefs().text_align, TextAlignName::Justify);
    assert_eq!(reloaded.prefs().paragraph_spacing_dp, 20);
    assert_eq!(reloaded.prefs().horizontal_padding_dp, 30);
}

#[test]
fn prefs_defaults_match_kotlin() {
    let p = ReaderPrefs::default();
    assert_eq!(p.font_size_sp, 18);
    assert!((p.line_height_multiplier - 1.4).abs() < 1e-5);
    assert_eq!(p.paragraph_spacing_dp, 12);
    assert_eq!(p.horizontal_padding_dp, 16);
    assert_eq!(p.theme, ReadingThemeName::Light);
    assert_eq!(p.text_align, TextAlignName::Left);
    assert_eq!(p.font_family, "");
}

#[test]
fn prefs_corrupt_file_falls_back_to_defaults() {
    let t = TempDir::new("prefs_corrupt");
    let paths = AppPaths::at(t.path());
    std::fs::write(t.path().join("reader_prefs.json"), "{{{").unwrap();
    assert_eq!(*ReaderPrefsStore::load(&paths).prefs(), ReaderPrefs::default());
}

// ─── 端到端:書庫 + 讀寫 + 位置記憶 ─────────────────────────────────────────

#[test]
fn end_to_end_open_edit_save_reopen_keeps_position() {
    let t = TempDir::new("e2e");
    let paths = AppPaths::at(t.path());
    let r = repo_in(t.path());
    let text = "第一章　開始\n　　內文一。\n\n第二章　結束\n　　內文二。\n";
    let p = t.file("novel.txt", text.as_bytes());
    let path_str = p.to_string_lossy().into_owned();

    // 1) 加入書庫並開啟。
    let mut lib = LibraryStore::load(&paths);
    let l = r.load(&p, None).unwrap();
    lib.upsert(Book {
        title: "novel.txt".into(),
        size_bytes: l.size_bytes,
        charset_name: l.charset_name.clone(),
        content_hash: l.content_hash.clone(),
        last_modified_at: l.last_modified_at,
        ..Book::new(path_str.clone(), "novel.txt")
    });
    lib.save();

    // 2) 記住閱讀位置。
    lib.update(&path_str, |b| Book {
        scroll_index: 3,
        ..b.clone()
    });
    lib.save();
    assert_eq!(LibraryStore::load(&paths).find(&path_str).unwrap().scroll_index, 3);

    // 3) 編輯存檔(沿用原編碼與指紋)。
    let edited = text.replace("內文一", "內文一(已改)");
    let out = r.save(&p, &edited, &l.charset_name, &l.content_hash, true);
    let SaveOutcome::Success { new_hash, .. } = out else {
        panic!("expected Success, got {out:?}");
    };
    lib.update(&path_str, |b| Book {
        content_hash: new_hash.clone(),
        ..b.clone()
    });
    lib.save();

    // 4) 重開:內容=編輯結果、指紋=新指紋、位置=3 仍保留。
    let reopened = r.load(&p, None).unwrap();
    assert_eq!(reopened.text, edited);
    assert_eq!(reopened.content_hash, new_hash);
    let b = LibraryStore::load(&paths).find(&path_str).unwrap().clone();
    assert_eq!(b.scroll_index, 3);
    assert_eq!(b.content_hash, new_hash);

    // 5) 存檔時有自動備份原檔。
    assert_eq!(r.list_backups(&path_str).len(), 1);
}

// ─── 編碼往返(BOM / 無法表示的字元)──────────────────────────────────────────

#[test]
fn utf16_file_survives_save_and_reopen() {
    // 舊版存 UTF-16 不帶 BOM → 重開被判成 UTF-8(夾 NUL)或 windows-1252 亂碼。
    for cs in ["UTF-16LE", "UTF-16BE"] {
        let t = TempDir::new("utf16_roundtrip");
        let bom: &[u8] = if cs == "UTF-16LE" { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] };
        let original = "第一章\nhello\n";
        let p = t.file("u16.txt", &[bom, &encoding::encode(original, cs)].concat());
        let r = repo_in(t.path());

        let l = r.load(&p, None).unwrap();
        assert_eq!(l.charset_name, cs);
        assert_eq!(l.text, original);

        let edited = format!("{original}新的一行 abc\n");
        let out = r.save(&p, &edited, cs, &l.content_hash, true);
        let SaveOutcome::Success { new_hash, .. } = out else {
            panic!("expected Success, got {out:?}");
        };
        let again = r.load(&p, None).unwrap();
        assert_eq!(again.charset_name, cs);
        assert_eq!(again.text, edited);
        assert_eq!(again.content_hash, new_hash);
    }
}

#[test]
fn utf8_bom_is_preserved_on_save() {
    let t = TempDir::new("utf8_bom");
    let p = t.file("bom.txt", &[&encoding::UTF8_BOM[..], "原文".as_bytes()].concat());
    let r = repo_in(t.path());
    let l = r.load(&p, None).unwrap();
    assert_eq!(l.text, "原文");

    assert!(matches!(r.save(&p, "改過", "UTF-8", &l.content_hash, true), SaveOutcome::Success { .. }));
    let raw = std::fs::read(&p).unwrap();
    assert_eq!(raw, [&encoding::UTF8_BOM[..], "改過".as_bytes()].concat());

    // 原檔沒有 BOM 就不加。
    let q = t.file("plain.txt", "原文".as_bytes());
    let lq = r.load(&q, None).unwrap();
    assert!(matches!(r.save(&q, "改過", "UTF-8", &lq.content_hash, false), SaveOutcome::Success { .. }));
    assert_eq!(std::fs::read(&q).unwrap(), "改過".as_bytes());
}

#[test]
fn unencodable_text_is_refused_without_touching_disk() {
    // 舊版會把 😀 默默寫成 "&#128512;"。
    let t = TempDir::new("unencodable");
    let original_bytes = encoding::encode("第一章", "Big5");
    let p = t.file("big5.txt", &original_bytes);
    let r = repo_in(t.path());
    let l = r.load(&p, None).unwrap();

    let out = r.save(&p, "第一章😀", "Big5", &l.content_hash, true);
    let SaveOutcome::Unencodable { charset_name, chars } = out else {
        panic!("expected Unencodable, got {out:?}");
    };
    assert_eq!(charset_name, "Big5");
    assert_eq!(chars, vec!['😀']);
    assert_eq!(std::fs::read(&p).unwrap(), original_bytes, "不得寫入");
    assert!(r.list_backups(p.to_str().unwrap()).is_empty(), "不得產生備份");

    // 強制覆蓋也一樣拒絕。
    assert!(matches!(
        r.save_overwrite(&p, "😀", "Big5", true),
        SaveOutcome::Unencodable { .. }
    ));
    assert_eq!(std::fs::read(&p).unwrap(), original_bytes);

    // 改用 UTF-8 即可存。
    assert!(matches!(r.save(&p, "第一章😀", "UTF-8", &l.content_hash, true), SaveOutcome::Success { .. }));
    assert_eq!(r.load(&p, None).unwrap().text, "第一章😀");
}

#[test]
fn charset_override_on_bom_file_uses_requested_charset() {
    // 舊版 decode 會 BOM sniffing:指定 windows-1252 卻偷偷用 UTF-8 解。
    let t = TempDir::new("override_bom");
    let p = t.file("bom.txt", &[&encoding::UTF8_BOM[..], "é".as_bytes()].concat());
    let r = repo_in(t.path());
    let l = r.load(&p, Some("windows-1252")).unwrap();
    assert_eq!(l.charset_name, "windows-1252");
    assert_eq!(l.text, "ï»¿Ã©");
    assert_eq!(r.load(&p, Some("UTF-8")).unwrap().text, "é");
}

#[test]
fn restore_keeps_backup_bom() {
    let t = TempDir::new("restore_bom");
    let with_bom = [&encoding::UTF8_BOM[..], b"v1"].concat();
    let p = t.file("book.txt", &with_bom);
    let r = repo_in(t.path());
    let h1 = r.load(&p, None).unwrap().content_hash;
    assert!(matches!(r.save(&p, "v2", "UTF-8", &h1, true), SaveOutcome::Success { .. }));
    // 磁碟改成無 BOM 版本,備份仍有 BOM → 還原後應回到「完全相同」的位元組。
    std::fs::write(&p, b"v3").unwrap();
    let h3 = r.load(&p, None).unwrap().content_hash;
    let backup = r
        .list_backups(p.to_str().unwrap())
        .into_iter()
        .find(|b| std::fs::read(&b.file).unwrap() == with_bom)
        .expect("v1 backup");
    assert!(matches!(r.restore_backup(&p, &backup.file, &h3), SaveOutcome::Success { .. }));
    assert_eq!(std::fs::read(&p).unwrap(), with_bom);
}

// ─── 原子寫入與損毀書庫 ──────────────────────────────────────────────────────

#[test]
fn atomic_write_replaces_file_and_leaves_no_temp() {
    let t = TempDir::new("atomic");
    let p = t.file("book.txt", b"old content that is longer");
    moonreader_rs::fileio::write_bytes(&p, b"new").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"new");
    let names: Vec<String> = std::fs::read_dir(t.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["book.txt".to_string()], "暫存檔應已 rename 掉");
}

#[test]
fn atomic_write_failure_keeps_original() {
    // 目標是目錄 → rename 失敗;原目錄與內容不受影響,也不留暫存檔。
    let t = TempDir::new("atomic_fail");
    let dir = t.path().join("iam_a_dir");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("keep.txt"), b"x").unwrap();
    assert!(moonreader_rs::fileio::write_bytes(&dir, b"data").is_err());
    assert_eq!(std::fs::read(dir.join("keep.txt")).unwrap(), b"x");
    assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 1);
}

#[test]
fn corrupt_library_is_set_aside_not_overwritten() {
    let t = TempDir::new("library_corrupt_keep");
    let paths = AppPaths::at(t.path());
    let corrupt = r#"[{"path":"/books/a.txt","title":"A"}, BROKEN"#;
    std::fs::write(t.path().join("library.json"), corrupt).unwrap();

    let mut store = LibraryStore::load(&paths);
    assert!(store.books().is_empty());
    store.upsert(Book::new("/books/new.txt", "New"));
    store.save();

    // 原損毀內容另存一份,沒被新的 save() 蓋掉。
    let kept: Vec<PathBuf> = std::fs::read_dir(t.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("library.json.corrupt-"))
        .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(std::fs::read_to_string(&kept[0]).unwrap(), corrupt);
    assert_eq!(LibraryStore::load(&paths).books().len(), 1);
}
