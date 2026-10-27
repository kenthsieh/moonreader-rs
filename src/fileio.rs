// 對應 Kotlin `io/SafFileIO.kt` 的桌面等價物:一般檔案路徑讀寫 + 指紋。
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn read_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    fs::read(path)
}

/// 全檔寫回(原子):先寫同目錄暫存檔並 fsync,再 rename 取代原檔。
/// 寫到一半當機/斷電時,原檔保持完整(直接 `fs::write` 會先截斷,中途失敗就只剩半份)。
pub fn write_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = temp_sibling(path);
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        // Windows 上 std::fs::rename 以 MOVEFILE_REPLACE_EXISTING 取代既有檔。
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn temp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    path.with_file_name(format!(".{name}.{}.moonreader-tmp", std::process::id()))
}

/// 磁碟上最後修改時間(毫秒),取不到回 0。
pub fn last_modified_ms(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

pub fn size_bytes(path: &Path) -> i64 {
    fs::metadata(path).map(|m| m.len() as i64).unwrap_or(-1)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 顯示用檔名(不含副檔名以外的路徑)。
pub fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
