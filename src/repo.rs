// 對應 Kotlin `data/BookRepository.kt`:讀(編碼偵測)、存(衝突偵測→備份→回寫)、備份/還原。
// 備份清單/還原與 matches_disk_state 在原 Android app 的 UI 也尚未接線,但屬 repository
// 對外介面,一併移植(含單元測試)以免行為遺失。
#![allow(dead_code)]
use crate::encoding;
use crate::fileio;
use crate::model::{LoadResult, SaveOutcome};
use std::path::{Path, PathBuf};

pub struct BookRepository {
    pub backups_dir: PathBuf,
}

impl BookRepository {
    pub fn new(backups_dir: PathBuf) -> Self {
        BookRepository { backups_dir }
    }

    /// charset_override 非空時跳過自動偵測(UI「重新以 X 編碼開啟」)。
    pub fn load(&self, path: &Path, charset_override: Option<&str>) -> std::io::Result<LoadResult> {
        let bytes = fileio::read_bytes(path)?;
        let detected = match charset_override {
            Some(cs) => encoding::Detected {
                charset_name: cs.to_string(),
                text: encoding::decode(&bytes, cs),
            },
            None => encoding::detect(&bytes),
        };
        Ok(LoadResult {
            size_bytes: bytes.len() as i64,
            content_hash: fileio::sha256_hex(&bytes),
            last_modified_at: fileio::last_modified_ms(path),
            charset_name: detected.charset_name,
            text: detected.text,
        })
    }

    /// 磁碟現況是否仍等於 expected_hash。
    pub fn matches_disk_state(&self, path: &Path, expected_hash: &str) -> bool {
        match fileio::read_bytes(path) {
            Ok(b) => fileio::sha256_hex(&b) == expected_hash,
            Err(_) => false,
        }
    }

    /// 存檔:
    /// 1) 重讀磁碟算 hash,與 expected_old_hash 不同 → Conflict(不寫入)。
    /// 2) 編碼全文;有目標編碼無法表示的字元 → Unencodable(不寫入、不備份)。
    /// 3) 先把磁碟現況備份到備份目錄(時間戳命名)。
    /// 4) 原子寫回;原檔有 UTF-8 BOM 時保留。
    pub fn save(
        &self,
        path: &Path,
        text: &str,
        charset_name: &str,
        expected_old_hash: &str,
        backup: bool,
    ) -> SaveOutcome {
        self.save_checked(path, text, charset_name, expected_old_hash, backup, None)
    }

    /// utf8_bom_override:None = 沿用磁碟現況的 BOM。
    fn save_checked(
        &self,
        path: &Path,
        text: &str,
        charset_name: &str,
        expected_old_hash: &str,
        backup: bool,
        utf8_bom_override: Option<bool>,
    ) -> SaveOutcome {
        let disk_bytes = match fileio::read_bytes(path) {
            Err(e) => return SaveOutcome::Failure(e.to_string()),
            Ok(b) => b,
        };
        if is_conflict(&fileio::sha256_hex(&disk_bytes), expected_old_hash) {
            return SaveOutcome::Conflict;
        }
        let bom = utf8_bom_override.unwrap_or_else(|| encoding::has_utf8_bom(&disk_bytes));
        self.write_document(path, text, charset_name, Some(&disk_bytes), backup, bom)
    }

    /// 衝突時強制覆蓋:仍先備份磁碟現況。
    pub fn save_overwrite(
        &self,
        path: &Path,
        text: &str,
        charset_name: &str,
        backup: bool,
    ) -> SaveOutcome {
        let disk_bytes = fileio::read_bytes(path).ok();
        let bom = disk_bytes.as_deref().is_some_and(encoding::has_utf8_bom);
        self.write_document(path, text, charset_name, disk_bytes.as_deref(), backup, bom)
    }

    fn write_document(
        &self,
        path: &Path,
        text: &str,
        charset_name: &str,
        disk_bytes: Option<&[u8]>,
        backup: bool,
        utf8_bom: bool,
    ) -> SaveOutcome {
        let out_bytes = match encoding::encode_for_file(text, charset_name, utf8_bom) {
            encoding::FileEncoding::Ok(b) => b,
            encoding::FileEncoding::Unencodable(chars) => {
                return SaveOutcome::Unencodable {
                    charset_name: charset_name.to_string(),
                    chars,
                };
            }
        };
        let made_backup = match disk_bytes {
            Some(b) if backup && !b.is_empty() => self.write_backup(path, b).is_some(),
            _ => false,
        };
        if let Err(e) = fileio::write_bytes(path, &out_bytes) {
            return SaveOutcome::Failure(e.to_string());
        }
        SaveOutcome::Success {
            backup_created: made_backup,
            new_hash: fileio::sha256_hex(&out_bytes),
            new_last_modified: fileio::last_modified_ms(path),
            new_size_bytes: out_bytes.len() as i64,
        }
    }

    fn write_backup(&self, path: &Path, current_bytes: &[u8]) -> Option<PathBuf> {
        let _ = std::fs::create_dir_all(&self.backups_dir);
        let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let base = sanitize_for_path(&fileio::display_name(path));
        // 時間戳只有秒級:同一秒內重複存檔時加序號,避免蓋掉前一份備份。
        let dest = (0..)
            .map(|n| {
                let name = if n == 0 {
                    format!("{base}.{stamp}.bak")
                } else {
                    format!("{base}-{n}.{stamp}.bak")
                };
                self.backups_dir.join(name)
            })
            .find(|p| !p.exists())?;
        std::fs::write(&dest, current_bytes).ok()?;
        Some(dest)
    }

    /// 列出某書的備份(新→舊)。
    pub fn list_backups(&self, book_path: &str) -> Vec<BackupInfo> {
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(&self.backups_dir) {
            Ok(r) => r,
            Err(_) => return out,
        };
        let stem_prefix = sanitize_for_path(
            &Path::new(book_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".bak") || !entry.path().is_file() {
                continue;
            }
            let stem = name.trim_end_matches(".bak");
            let idx = match stem.rfind('.') {
                Some(i) if i > 0 => i,
                _ => continue,
            };
            // 名稱形如 <base>.<timestamp>.bak;base 可能含 '.',故取「timestamp 段」判斷:
            // timestamp = yyyyMMdd_HHmmss(15 字,含底線),最後一個 '.' 前應剛好是它。
            let maybe_ts = &stem[idx + 1..];
            if maybe_ts.len() != 15 || !maybe_ts.contains('_') {
                continue;
            }
            // 只收屬於這本書的備份
            if !stem[..idx].starts_with(stem_prefix.as_str()) {
                continue;
            }
            let created = entry
                .metadata()
                .and_then(|m| m.modified())
                .map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            out.push(BackupInfo {
                file: entry.path(),
                created_at: created,
                original_name: stem[..idx].to_string(),
            });
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out
    }

    /// 還原備份:一律走衝突感知 save()(覆寫前 save 會再自動備份一次)。
    /// 備份檔自身的編碼由內容重新偵測。
    pub fn restore_backup(
        &self,
        path: &Path,
        backup_file: &Path,
        current_hash: &str,
    ) -> SaveOutcome {
        let bytes = match fileio::read_bytes(backup_file) {
            Ok(b) => b,
            Err(e) => return SaveOutcome::Failure(e.to_string()),
        };
        let detected = encoding::detect(&bytes);
        let bom = encoding::has_utf8_bom(&bytes);
        self.save_checked(path, &detected.text, &detected.charset_name, current_hash, true, Some(bom))
    }
}

pub struct BackupInfo {
    pub file: PathBuf,
    pub created_at: i64,
    pub original_name: String,
}

/// 純衝突判定:磁碟 hash 與基準不同 → 衝突;空基準(首次)不衝。
pub fn is_conflict(disk_hash: &str, expected_hash: &str) -> bool {
    !expected_hash.is_empty() && disk_hash != expected_hash
}

fn sanitize_for_path(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric()
                || c == '.'
                || c == ' '
                || c == '-'
                || ('\u{4e00}'..='\u{9fff}').contains(&c)
            {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_rule() {
        assert!(!is_conflict("abc123", "abc123"));
        assert!(is_conflict("aaa", "bbb"));
        assert!(!is_conflict("anything", ""));
    }

    #[test]
    fn sanitize_keeps_cjk_drops_separators() {
        assert_eq!(sanitize_for_path("a b/c:d"), "a b_c_d");
        assert_eq!(sanitize_for_path("第一章.txt"), "第一章.txt");
    }
}
