// 對應 Kotlin `io/Encoding.kt`:編碼偵測與解碼/回寫。
// 偵測順序:BOM → 嚴格 UTF-8 → Big5 → GBK → 後備 windows-1252。
use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, UTF_8, WINDOWS_1252};

/// UI 可手動改選的候選集(偵測錯誤的後援)。
pub const SUPPORTED_CHARSETS: [&str; 6] =
    ["UTF-8", "UTF-16LE", "UTF-16BE", "Big5", "GBK", "windows-1252"];

#[derive(Clone, Debug)]
pub struct Detected {
    pub charset_name: String,
    pub text: String,
}

/// 解析字元集,不支援時退回 UTF-8(對應 Kotlin charsetOf)。
fn encoding_of(name: &str) -> &'static Encoding {
    Encoding::for_label(name.as_bytes()).unwrap_or(UTF_8)
}

fn starts_with(bytes: &[u8], prefix: &[u8]) -> bool {
    bytes.len() >= prefix.len() && &bytes[..prefix.len()] == prefix
}

/// 嚴格解碼(不可有錯誤),對應 Kotlin decodesStrictly。
/// encoding_rs 的 decode 為 best-effort:had_errors=true 代表有 illegal/malformed 位元組。
fn decodes_strictly(bytes: &[u8], enc: &'static Encoding) -> bool {
    let (_, _, had_errors) = enc.decode(bytes);
    !had_errors
}

pub fn detect(bytes: &[u8]) -> Detected {
    // 1) BOM 優先(最明確)。encoding_rs 的 decode 依 WHATWG 會移除對應 BOM。
    if starts_with(bytes, &[0xEF, 0xBB, 0xBF]) {
        let (cow, _, _) = UTF_8.decode(bytes);
        return Detected {
            charset_name: "UTF-8".into(),
            text: cow.into_owned(),
        };
    }
    if starts_with(bytes, &[0xFF, 0xFE]) {
        let (cow, _, _) = UTF_16LE.decode(bytes);
        return Detected {
            charset_name: "UTF-16LE".into(),
            text: cow.into_owned(),
        };
    }
    if starts_with(bytes, &[0xFE, 0xFF]) {
        let (cow, _, _) = UTF_16BE.decode(bytes);
        return Detected {
            charset_name: "UTF-16BE".into(),
            text: cow.into_owned(),
        };
    }
    // 2) 嚴格 UTF-8:全檔可無錯解碼才算 UTF-8/ASCII。
    if decodes_strictly(bytes, UTF_8) {
        let (cow, _, _) = UTF_8.decode(bytes);
        return Detected {
            charset_name: "UTF-8".into(),
            text: cow.into_owned(),
        };
    }
    // 3) Big5,再 GBK(encoding_rs 以 gb18030 處理 GBK 標籤,為 GBK 超集)。
    for label in ["Big5", "GBK"] {
        if let Some(enc) = Encoding::for_label(label.as_bytes()) {
            if decodes_strictly(bytes, enc) {
                let (cow, _, _) = enc.decode(bytes);
                return Detected {
                    charset_name: label.into(),
                    text: cow.into_owned(),
                };
            }
        }
    }
    // 4) 後備 windows-1252(幾乎任何位元組都能解)。
    let (cow, _, _) = WINDOWS_1252.decode(bytes);
    Detected {
        charset_name: "windows-1252".into(),
        text: cow.into_owned(),
    }
}

/// 以指定編碼解碼(使用者「重新以 X 編碼開啟」)。
/// 不可用 `Encoding::decode`:它會 BOM sniffing,檔案帶 UTF-8 BOM 時即使指定 Big5
/// 也會偷偷改用 UTF-8 解,但 UI/存檔仍以為是 Big5。這裡只移除「與指定編碼相符」的 BOM。
pub fn decode(bytes: &[u8], charset_name: &str) -> String {
    let (cow, _) = encoding_of(charset_name).decode_with_bom_removal(bytes);
    cow.into_owned()
}

pub const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

pub fn has_utf8_bom(bytes: &[u8]) -> bool {
    starts_with(bytes, &UTF8_BOM)
}

fn is_utf16le(charset_name: &str) -> bool {
    encoding_of(charset_name) == UTF_16LE
}
fn is_utf16be(charset_name: &str) -> bool {
    encoding_of(charset_name) == UTF_16BE
}

/// 存檔用編碼結果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileEncoding {
    Ok(Vec<u8>),
    /// 目標編碼無法表示的字元(最多列出數個,依出現順序、不重複)。
    /// encoding_rs 遇到這類字元會默默換成 `&#NNNN;`,寫進檔案就是資料損壞,故不可寫。
    Unencodable(Vec<char>),
}

/// 存檔用:一律可被 `detect` 正確讀回。
/// - UTF-16LE/BE:一律帶 BOM(沒有 BOM 時 `detect` 無從判斷,會誤判成 UTF-8/windows-1252)。
/// - UTF-8:`utf8_bom` 為真時帶 BOM(保留原檔格式)。
/// - 其他:有無法表示的字元 → `Unencodable`。
pub fn encode_for_file(text: &str, charset_name: &str, utf8_bom: bool) -> FileEncoding {
    if is_utf16le(charset_name) {
        let mut out = vec![0xFF, 0xFE];
        out.extend(encode(text, charset_name));
        return FileEncoding::Ok(out);
    }
    if is_utf16be(charset_name) {
        let mut out = vec![0xFE, 0xFF];
        out.extend(encode(text, charset_name));
        return FileEncoding::Ok(out);
    }
    let enc = encoding_of(charset_name);
    if enc == UTF_8 {
        let mut out = Vec::with_capacity(text.len() + 3);
        if utf8_bom {
            out.extend_from_slice(&UTF8_BOM);
        }
        out.extend_from_slice(text.as_bytes());
        return FileEncoding::Ok(out);
    }
    let (cow, _, had_errors) = enc.encode(text);
    if had_errors {
        return FileEncoding::Unencodable(unencodable_chars(text, enc, 5));
    }
    FileEncoding::Ok(cow.into_owned())
}

/// 找出 enc 無法表示的字元(只在 encode 已回報錯誤時呼叫,逐字檢查成本可接受)。
fn unencodable_chars(text: &str, enc: &'static Encoding, limit: usize) -> Vec<char> {
    let mut out: Vec<char> = Vec::new();
    let mut buf = [0u8; 4];
    for c in text.chars() {
        if c.is_ascii() || out.contains(&c) {
            continue;
        }
        let (_, _, bad) = enc.encode(c.encode_utf8(&mut buf));
        if bad {
            out.push(c);
            if out.len() >= limit {
                break;
            }
        }
    }
    out
}

/// 原始編碼(不加 BOM、無法表示的字元以 `&#NNNN;` 代替)。存檔請用 `encode_for_file`。
pub fn encode(text: &str, charset_name: &str) -> Vec<u8> {
    // UTF-16 在 WHATWG 不提供 encode 路徑,手動輸出(與 Java UTF-16LE/BE 不帶 BOM 一致)。
    if is_utf16le(charset_name) {
        return text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    }
    if is_utf16be(charset_name) {
        return text.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
    }
    let enc = encoding_of(charset_name);
    if enc == UTF_8 {
        return text.as_bytes().to_vec();
    }
    let (cow, _, _) = enc.encode(text);
    cow.into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_with_bom_detected_and_stripped() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("hello".as_bytes());
        let d = detect(&bytes);
        assert_eq!(d.charset_name, "UTF-8");
        assert_eq!(d.text, "hello");
    }

    #[test]
    fn plain_ascii_is_utf8() {
        let d = detect(b"plain ascii text");
        assert_eq!(d.charset_name, "UTF-8");
    }

    #[test]
    fn utf16le_bom_detected() {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(encode("ab", "UTF-16LE"));
        let d = detect(&bytes);
        assert_eq!(d.charset_name, "UTF-16LE");
        assert_eq!(d.text, "ab");
    }

    #[test]
    fn utf16be_bom_detected() {
        let mut bytes = vec![0xFE, 0xFF];
        bytes.extend(encode("ab", "UTF-16BE"));
        let d = detect(&bytes);
        assert_eq!(d.charset_name, "UTF-16BE");
        assert_eq!(d.text, "ab");
    }

    #[test]
    fn utf16_roundtrip_without_bom() {
        for cs in ["UTF-16LE", "UTF-16BE"] {
            let text = "中文測試內容";
            assert_eq!(decode(&encode(text, cs), cs), text);
        }
    }

    #[test]
    fn encode_decode_roundtrip_custom_charset() {
        let text = "中文測試內容";
        let bytes = encode(text, "Big5");
        assert_eq!(decode(&bytes, "Big5"), text);
    }

    #[test]
    fn chinese_utf8_content_decodes() {
        let text = "靜讀天下 編輯器";
        let d = detect(text.as_bytes());
        assert_eq!(d.charset_name, "UTF-8");
        assert_eq!(d.text, text);
    }

    #[test]
    fn big5_bytes_are_detected_as_big5() {
        // Big5 編碼的中文字串不是合法 UTF-8,偵測順序應落在 Big5。
        let text = "第一章　劫後夢醒";
        let bytes = encode(text, "Big5");
        let d = detect(&bytes);
        assert_eq!(d.charset_name, "Big5");
        assert_eq!(d.text, text);
    }

    #[test]
    fn gbk_bytes_fall_to_a_cjk_charset() {
        // 偵測順序為 Big5 → GBK(對應 Kotlin 版),故 GBK 位元組可能被判為 Big5;
        // 這裡只要求「落在 CJK 候選之一」,不要求文字內容相同。
        let text = "第一章　劫后梦醒";
        let bytes = encode(text, "GBK");
        let d = detect(&bytes);
        assert!(matches!(d.charset_name.as_str(), "Big5" | "GBK"));
        assert!(!d.text.is_empty());
    }

    #[test]
    fn unsupported_charset_falls_back_to_utf8_without_crash() {
        // 對應 Kotlin charsetOf("NOT-A-REAL-CHARSET") == UTF_8。
        let text = "中文";
        let bytes = encode(text, "NOT-A-REAL-CHARSET");
        assert_eq!(bytes, text.as_bytes());
        assert_eq!(decode(&bytes, "NOT-A-REAL-CHARSET"), text);
    }

    #[test]
    fn supported_list_contains_big5_and_gbk() {
        assert!(SUPPORTED_CHARSETS.contains(&"Big5"));
        assert!(SUPPORTED_CHARSETS.contains(&"GBK"));
    }

    #[test]
    fn windows_1252_fallback_never_fails() {
        // 任意位元組都能解出(後備編碼永不拋錯)。
        let bytes = [0x80u8, 0xFF, 0x41, 0xFE];
        let d = detect(&bytes);
        assert_eq!(d.charset_name, "windows-1252");
        assert!(!d.text.is_empty());
    }

    #[test]
    fn utf16_file_encoding_roundtrips_through_detect() {
        // 舊版存 UTF-16 不帶 BOM,重開會被判成 UTF-8(夾 NUL)或 windows-1252 亂碼。
        for cs in ["UTF-16LE", "UTF-16BE"] {
            for text in ["hello", "中文測試內容", ""] {
                let FileEncoding::Ok(bytes) = encode_for_file(text, cs, false) else {
                    panic!("UTF-16 can encode anything");
                };
                let d = detect(&bytes);
                assert_eq!(d.charset_name, cs, "{text:?}");
                assert_eq!(d.text, text);
            }
        }
    }

    #[test]
    fn utf8_bom_preserved_only_when_requested() {
        let FileEncoding::Ok(with) = encode_for_file("abc", "UTF-8", true) else { panic!() };
        assert_eq!(with, [&UTF8_BOM[..], b"abc"].concat());
        assert!(has_utf8_bom(&with));
        assert_eq!(detect(&with).text, "abc");
        let FileEncoding::Ok(without) = encode_for_file("abc", "UTF-8", false) else { panic!() };
        assert_eq!(without, b"abc");
    }

    #[test]
    fn unencodable_chars_reported_instead_of_ncr() {
        // Big5 無 emoji:舊版會默默寫成 "&#128512;"。
        let r = encode_for_file("你好😀世界😀🎉", "Big5", false);
        assert_eq!(r, FileEncoding::Unencodable(vec!['😀', '🎉']));
        assert_eq!(
            encode_for_file("café", "windows-1252", false),
            FileEncoding::Ok(encode("café", "windows-1252"))
        );
        assert!(matches!(encode_for_file("中文", "windows-1252", false), FileEncoding::Unencodable(_)));
    }

    #[test]
    fn override_decode_does_not_sniff_foreign_bom() {
        // 帶 UTF-8 BOM 的檔案,使用者指定 windows-1252:必須真的用 windows-1252 解。
        let mut bytes = UTF8_BOM.to_vec();
        bytes.extend_from_slice("é".as_bytes());
        assert_eq!(decode(&bytes, "windows-1252"), "ï»¿Ã©");
        // 相符的 BOM 仍會移除。
        assert_eq!(decode(&bytes, "UTF-8"), "é");
        let mut le = vec![0xFF, 0xFE];
        le.extend(encode("ab", "UTF-16LE"));
        assert_eq!(decode(&le, "UTF-16LE"), "ab");
    }

    #[test]
    fn empty_input_is_utf8() {
        let d = detect(&[]);
        assert_eq!(d.charset_name, "UTF-8");
        assert_eq!(d.text, "");
    }
}
