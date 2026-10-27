// 對應 Kotlin `data/local/LibraryStore.kt` + `data/prefs/ReaderPrefsStore.kt`。
// DataStore → 以 JSON 檔案持久化;路徑放在 app_data_dir/MoonReader。
use crate::fileio;
use crate::model::{Book, ReaderPrefs};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct AppPaths {
    root: PathBuf,
}

impl AppPaths {
    pub fn open() -> Self {
        let root = directories::ProjectDirs::from("com", "example", "moonreader")
            .map(|d| d.data_dir().join("MoonReader"))
            .unwrap_or_else(|| PathBuf::from("."));
        Self::at(root)
    }

    /// 指定根目錄(測試用:避免寫入使用者真正的 app-data)。
    pub fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let _ = fs::create_dir_all(&root);
        AppPaths { root }
    }

    pub fn backups_dir(&self) -> PathBuf {
        let p = self.root.join("backups");
        let _ = fs::create_dir_all(&p);
        p
    }

    fn library_file(&self) -> PathBuf {
        self.root.join("library.json")
    }
    fn prefs_file(&self) -> PathBuf {
        self.root.join("reader_prefs.json")
    }
}

/// 書庫:整包 Vec<Book> 存單一 JSON 檔(v1 書量小,全量重寫足夠)。
pub struct LibraryStore {
    paths: AppPaths,
    books: Vec<Book>,
}

impl LibraryStore {
    pub fn load(paths: &AppPaths) -> Self {
        let file = paths.library_file();
        let books = match fs::read_to_string(&file) {
            Ok(raw) if raw.trim().is_empty() => Vec::new(),
            Ok(raw) => serde_json::from_str::<Vec<Book>>(&raw).unwrap_or_else(|_| {
                // 損毀:先把原檔改名保留,否則下一次 save() 會以空書庫覆蓋、資料永久遺失。
                set_aside_corrupt(&file);
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        LibraryStore {
            paths: paths.clone(),
            books,
        }
    }

    pub fn books(&self) -> &[Book] {
        &self.books
    }

    pub fn find(&self, path: &str) -> Option<&Book> {
        self.books.iter().find(|b| b.path == path)
    }

    /// 新增或依 path 取代。回傳是否為新檔。
    pub fn upsert(&mut self, book: Book) -> bool {
        let idx = self.books.iter().position(|b| b.path == book.path);
        match idx {
            Some(i) => {
                self.books[i] = book;
                false
            }
            None => {
                self.books.push(book);
                true
            }
        }
    }

    pub fn update<F: FnOnce(&Book) -> Book>(&mut self, path: &str, transform: F) {
        if let Some(i) = self.books.iter().position(|b| b.path == path) {
            let b = self.books[i].clone();
            self.books[i] = transform(&b);
        }
    }

    pub fn remove(&mut self, path: &str) {
        self.books.retain(|b| b.path != path);
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string(&self.books) {
            let _ = fileio::write_bytes(&self.paths.library_file(), json.as_bytes());
        }
    }
}

/// 閱讀偏好:整包 ReaderPrefs 序列化,變更即寫。
pub struct ReaderPrefsStore {
    paths: AppPaths,
    prefs: ReaderPrefs,
}

impl ReaderPrefsStore {
    pub fn load(paths: &AppPaths) -> Self {
        let prefs = fs::read_to_string(paths.prefs_file())
            .ok()
            .and_then(|raw| serde_json::from_str::<ReaderPrefs>(&raw).ok())
            .unwrap_or_default();
        ReaderPrefsStore {
            paths: paths.clone(),
            prefs,
        }
    }

    pub fn prefs(&self) -> &ReaderPrefs {
        &self.prefs
    }

    pub fn update<F: FnOnce(&mut ReaderPrefs)>(&mut self, transform: F) {
        let mut p = self.prefs.clone();
        transform(&mut p);
        self.prefs = p;
        self.save();
    }

    fn save(&self) {
        if let Ok(json) = serde_json::to_string(&self.prefs) {
            let _ = fileio::write_bytes(&self.paths.prefs_file(), json.as_bytes());
        }
    }
}

/// 把無法解析的設定檔改名為 `<name>.corrupt-<時間戳>`(不覆蓋既有的同名檔)。
fn set_aside_corrupt(file: &Path) {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let dest = (0..)
        .map(|n| {
            let suffix = if n == 0 { String::new() } else { format!("-{n}") };
            file.with_file_name(format!("{name}.corrupt-{stamp}{suffix}"))
        })
        .find(|p| !p.exists());
    if let Some(dest) = dest {
        let _ = fs::rename(file, dest);
    }
}
