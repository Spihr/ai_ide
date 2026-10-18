//! 文件读写、创建、重命名、删除。
//!
//! ## 两个容易被忽略的工程细节
//!
//! 1. **换行与编码要「原样进、原样出」**。编辑器内部统一用 `\n`，所以读取时记录
//!    原始 EOL 并在写回时还原；编码同理（GBK 文件写回 UTF-8 会让别的工具变乱码）。
//! 2. **乐观锁是内容级而非时间级的**。用 SHA-256 比对磁盘与前端持有的版本，
//!    避免「mtime 精度不足导致的假阴性」。

use crate::path::PathGuard;
use crate::WorkspaceError;
use ai_ide_protocol::{
    CreateKind, Eol, WorkspaceDeleteResult, WorkspaceReadFileResult, WorkspaceWriteFileResult,
};
use encoding_rs::{Encoding, GBK, UTF_16BE, UTF_16LE, UTF_8};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::Path;

/// 默认读取上限：8 MiB。超出即截断，避免一个大日志把 UI 拖死。
pub const DEFAULT_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// 计算内容的 SHA-256（十六进制小写）。
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 读取文件。
pub fn read_file(
    guard: &PathGuard,
    input: &str,
    encoding: Option<&str>,
    max_bytes: Option<u64>,
) -> Result<WorkspaceReadFileResult, WorkspaceError> {
    let abs = guard.resolve(input)?;
    if !abs.is_file() {
        return Err(WorkspaceError::NotFound(abs.display().to_string()));
    }

    let bytes = fs::read(&abs).map_err(|e| WorkspaceError::io(&abs, e))?;
    let size = bytes.len() as u64;
    let limit = max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    let truncated = size > limit;
    let slice = if truncated {
        &bytes[..limit as usize]
    } else {
        &bytes[..]
    };

    let (decoded, encoding_name) = decode(slice, encoding);
    let eol = detect_eol(&decoded);

    Ok(WorkspaceReadFileResult {
        content: normalize_eol(&decoded),
        encoding: encoding_name,
        eol,
        size,
        truncated,
        // 哈希基于**完整**磁盘内容，截断不影响乐观锁的正确性。
        hash: hash_bytes(&bytes),
    })
}

/// 写入文件（原子写 + 乐观锁）。
pub fn write_file(
    guard: &PathGuard,
    input: &str,
    content: &str,
    expected_hash: Option<&str>,
    eol_override: Option<Eol>,
) -> Result<WorkspaceWriteFileResult, WorkspaceError> {
    let abs = guard.resolve_for_write(input)?;

    if let Some(parent) = abs.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| WorkspaceError::io(parent, e))?;
        }
    }

    let existing = if abs.is_file() {
        Some(fs::read(&abs).map_err(|e| WorkspaceError::io(&abs, e))?)
    } else {
        None
    };

    if let Some(expected) = expected_hash {
        let actual = existing
            .as_ref()
            .map(|b| hash_bytes(b))
            .unwrap_or_else(|| hash_bytes(b""));
        if actual != expected {
            return Err(WorkspaceError::HashMismatch {
                expected: expected.to_string(),
                actual,
            });
        }
    }

    // 编码与 EOL 都沿用原文件；新建文件用 UTF-8 + LF。
    let (encoding_name, eol) = match existing.as_ref() {
        Some(bytes) => {
            let (decoded, name) = decode(bytes, None);
            (name, eol_override.unwrap_or_else(|| detect_eol(&decoded)))
        }
        None => ("utf-8".to_string(), eol_override.unwrap_or(Eol::Lf)),
    };

    let text = restore_eol(&normalize_eol(content), eol);
    let bytes = encode(&text, &encoding_name);
    atomic_write(&abs, &bytes)?;

    Ok(WorkspaceWriteFileResult {
        ok: true,
        hash: hash_bytes(&bytes),
    })
}

/// 新建文件或目录（父目录会自动补齐）。
pub fn create(guard: &PathGuard, input: &str, kind: CreateKind) -> Result<(), WorkspaceError> {
    let abs = guard.resolve_for_write(input)?;
    if abs.exists() {
        return Err(WorkspaceError::Other(format!(
            "目标已存在：{}",
            abs.display()
        )));
    }

    match kind {
        CreateKind::Dir => {
            fs::create_dir_all(&abs).map_err(|e| WorkspaceError::io(&abs, e))?;
        }
        CreateKind::File => {
            if let Some(parent) = abs.parent() {
                fs::create_dir_all(parent).map_err(|e| WorkspaceError::io(parent, e))?;
            }
            fs::File::create(&abs).map_err(|e| WorkspaceError::io(&abs, e))?;
        }
    }
    Ok(())
}

/// 重命名 / 移动。两端都必须留在工作区内。
pub fn rename(
    guard: &PathGuard,
    from: &str,
    to: &str,
    overwrite: bool,
) -> Result<(), WorkspaceError> {
    let src = guard.resolve_for_write(from)?;
    let dst = guard.resolve_for_write(to)?;

    if !src.exists() {
        return Err(WorkspaceError::NotFound(src.display().to_string()));
    }
    if dst.exists() && !overwrite {
        return Err(WorkspaceError::Other(format!(
            "目标已存在：{}",
            dst.display()
        )));
    }
    if let Some(parent) = dst.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| WorkspaceError::io(parent, e))?;
        }
    }

    fs::rename(&src, &dst).map_err(|e| WorkspaceError::io(&dst, e))
}

/// 删除：只允许移入回收站，逐个执行并汇报失败项。
pub fn delete(
    guard: &PathGuard,
    paths: &[String],
    to_trash: bool,
) -> Result<WorkspaceDeleteResult, WorkspaceError> {
    if !to_trash {
        return Err(WorkspaceError::InvalidArgs(
            "协议只支持移入回收站（to_trash 必须为 true）".into(),
        ));
    }
    if paths.is_empty() {
        return Err(WorkspaceError::InvalidArgs("paths 不能为空".into()));
    }

    let mut failed: Vec<String> = Vec::new();
    for raw in paths {
        match guard.resolve_for_write(raw) {
            Ok(abs) => {
                if !abs.exists() {
                    failed.push(raw.clone());
                    continue;
                }
                if let Err(err) = trash::delete_all([&abs]) {
                    tracing::warn!(path = %abs.display(), error = %err, "移入回收站失败");
                    failed.push(raw.clone());
                }
            }
            // 越界或受保护路径：直接记为失败，不泄露具体原因给上层调用方之外的通道。
            Err(err) => {
                tracing::warn!(path = %raw, error = %err, "拒绝删除");
                failed.push(raw.clone());
            }
        }
    }

    Ok(WorkspaceDeleteResult {
        ok: failed.is_empty(),
        failed,
    })
}

// ---------------------------------------------------------------------------
// 编码 / 换行
// ---------------------------------------------------------------------------

/// BOM 常量。
const BOM_UTF8: &[u8] = &[0xEF, 0xBB, 0xBF];
const BOM_UTF16LE: &[u8] = &[0xFF, 0xFE];
const BOM_UTF16BE: &[u8] = &[0xFE, 0xFF];

/// 解码字节流，返回 `(文本, 编码名)`。
pub fn decode(bytes: &[u8], forced: Option<&str>) -> (String, String) {
    if let Some(label) = forced {
        let enc = Encoding::for_label(label.as_bytes()).unwrap_or(UTF_8);
        let (cow, _, _) = enc.decode(bytes);
        return (cow.into_owned(), enc.name().to_string());
    }

    if bytes.starts_with(BOM_UTF8) {
        let (cow, _, _) = UTF_8.decode(&bytes[BOM_UTF8.len()..]);
        return (cow.into_owned(), "utf-8".into());
    }
    if bytes.starts_with(BOM_UTF16LE) {
        let (cow, _, _) = UTF_16LE.decode(&bytes[BOM_UTF16LE.len()..]);
        return (cow.into_owned(), "utf-16le".into());
    }
    if bytes.starts_with(BOM_UTF16BE) {
        let (cow, _, _) = UTF_16BE.decode(&bytes[BOM_UTF16BE.len()..]);
        return (cow.into_owned(), "utf-16be".into());
    }

    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_string(), "utf-8".into()),
        // 无 BOM 且不是合法 UTF-8：中文环境最常见的答案是 GBK。
        Err(_) => {
            let (cow, _, had_errors) = GBK.decode(bytes);
            if had_errors {
                tracing::debug!("GBK 解码存在替换字符，可能不是纯文本");
            }
            (cow.into_owned(), "gbk".into())
        }
    }
}

/// 编码文本；未知编码名按 UTF-8 处理。
pub fn encode(text: &str, encoding_name: &str) -> Vec<u8> {
    match encoding_name.to_lowercase().as_str() {
        "utf-8" | "utf8" => text.as_bytes().to_vec(),
        other => match Encoding::for_label(other.as_bytes()) {
            Some(enc) if enc != UTF_8 => enc.encode(text).0.into_owned(),
            _ => text.as_bytes().to_vec(),
        },
    }
}

/// 探测换行风格；无换行时按 LF。
pub fn detect_eol(text: &str) -> Eol {
    let crlf = text.matches("\r\n").count();
    let total_lf = text.matches('\n').count();
    let lf_only = total_lf.saturating_sub(crlf);
    let cr_only = text.matches('\r').count().saturating_sub(crlf);

    if crlf > 0 && crlf >= lf_only && crlf >= cr_only {
        Eol::Crlf
    } else if cr_only > lf_only {
        Eol::Cr
    } else {
        Eol::Lf
    }
}

/// 统一成 `\n`。
pub fn normalize_eol(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// 按目标风格还原换行。
pub fn restore_eol(text: &str, eol: Eol) -> String {
    let normalized = normalize_eol(text);
    match eol {
        Eol::Lf => normalized,
        Eol::Crlf => normalized.replace('\n', "\r\n"),
        Eol::Cr => normalized.replace('\n', "\r"),
    }
}

/// 原子写：先落临时文件再改名，避免半截文件。
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), WorkspaceError> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let tmp = parent.join(format!(
        ".{}.ai-ide-tmp-{}",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into()),
        uuid::Uuid::new_v4()
    ));

    {
        let mut file = fs::File::create(&tmp).map_err(|e| WorkspaceError::io(&tmp, e))?;
        file.write_all(bytes)
            .map_err(|e| WorkspaceError::io(&tmp, e))?;
        file.flush().map_err(|e| WorkspaceError::io(&tmp, e))?;
    }

    if let Err(err) = fs::rename(&tmp, path) {
        // 改名失败要把临时文件清掉，别在用户目录里留垃圾。
        let _ = fs::remove_file(&tmp);
        return Err(WorkspaceError::io(path, err));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ai-ide-fs-{}-{}-{}",
            tag,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    #[test]
    fn round_trip_keeps_content_and_hash() {
        let root = temp_workspace("roundtrip");
        let guard = PathGuard::new(root.clone()).unwrap();
        let file = root.join("a.txt");
        fs::write(&file, "hello\nworld\n").unwrap();

        let read = read_file(&guard, "a.txt", None, None).unwrap();
        assert_eq!(read.content, "hello\nworld\n");
        assert_eq!(read.encoding, "utf-8");
        assert_eq!(read.eol, Eol::Lf);
        assert!(!read.truncated);

        let written = write_file(&guard, "a.txt", "hello\nrust\n", Some(&read.hash), None).unwrap();
        assert!(written.ok);
        assert_eq!(fs::read_to_string(&file).unwrap(), "hello\nrust\n");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn crlf_is_preserved_on_save() {
        let root = temp_workspace("crlf");
        let guard = PathGuard::new(root.clone()).unwrap();
        let file = root.join("win.txt");
        fs::write(&file, "a\r\nb\r\n").unwrap();

        let read = read_file(&guard, "win.txt", None, None).unwrap();
        assert_eq!(read.eol, Eol::Crlf);
        assert_eq!(read.content, "a\nb\n", "内部统一为 LF");

        write_file(&guard, "win.txt", "a\nc\n", Some(&read.hash), None).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "a\r\nc\r\n");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn optimistic_lock_rejects_stale_write() {
        let root = temp_workspace("lock");
        let guard = PathGuard::new(root.clone()).unwrap();
        let file = root.join("a.txt");
        fs::write(&file, "v1").unwrap();

        let read = read_file(&guard, "a.txt", None, None).unwrap();
        fs::write(&file, "v2-external").unwrap();

        let err = write_file(&guard, "a.txt", "v3", Some(&read.hash), None).unwrap_err();
        assert!(matches!(err, WorkspaceError::HashMismatch { .. }));
        assert_eq!(fs::read_to_string(&file).unwrap(), "v2-external");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_refuses_permanent_mode() {
        let root = temp_workspace("delmode");
        let guard = PathGuard::new(root.clone()).unwrap();
        let err = delete(&guard, &["a.txt".into()], false).unwrap_err();
        assert!(matches!(err, WorkspaceError::InvalidArgs(_)));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_reports_missing_paths_without_touching_disk() {
        let root = temp_workspace("delmissing");
        let guard = PathGuard::new(root.clone()).unwrap();
        let result = delete(&guard, &["nope.txt".into()], true).unwrap();
        assert!(!result.ok);
        assert_eq!(result.failed, vec!["nope.txt".to_string()]);
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn write_refuses_sensitive_path() {
        let root = temp_workspace("sensitive");
        let guard = PathGuard::new(root.clone()).unwrap();
        let err = write_file(&guard, ".env", "SECRET=1", None, None).unwrap_err();
        assert!(matches!(err, WorkspaceError::Path(_)));
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn create_file_makes_parent_dirs() {
        let root = temp_workspace("create");
        let guard = PathGuard::new(root.clone()).unwrap();
        create(&guard, "src/deep/new.rs", CreateKind::File).unwrap();
        assert!(root.join("src").join("deep").join("new.rs").is_file());

        let err = create(&guard, "src/deep/new.rs", CreateKind::File).unwrap_err();
        assert!(matches!(err, WorkspaceError::Other(_)));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rename_within_workspace() {
        let root = temp_workspace("rename");
        let guard = PathGuard::new(root.clone()).unwrap();
        create(&guard, "old.txt", CreateKind::File).unwrap();
        rename(&guard, "old.txt", "new.txt", false).unwrap();
        assert!(root.join("new.txt").is_file());
        assert!(!root.join("old.txt").exists());

        let err = rename(&guard, "..\\..\\escape.txt", "x.txt", false).unwrap_err();
        assert!(matches!(err, WorkspaceError::Path(_)));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn utf8_bom_is_stripped_and_reported() {
        let root = temp_workspace("bom");
        let guard = PathGuard::new(root.clone()).unwrap();
        let mut bytes = BOM_UTF8.to_vec();
        bytes.extend_from_slice("hi".as_bytes());
        fs::write(root.join("bom.txt"), &bytes).unwrap();

        let read = read_file(&guard, "bom.txt", None, None).unwrap();
        assert_eq!(read.encoding, "utf-8");
        assert_eq!(read.content, "hi", "BOM 不应出现在内容里");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn gbk_round_trip_keeps_encoding() {
        let root = temp_workspace("gbk");
        let guard = PathGuard::new(root.clone()).unwrap();
        let (gbk_bytes, _, _) = GBK.encode("中文测试");
        fs::write(root.join("gbk.txt"), gbk_bytes.as_ref()).unwrap();

        let read = read_file(&guard, "gbk.txt", None, None).unwrap();
        assert_eq!(read.encoding, "gbk");
        assert_eq!(read.content, "中文测试");

        write_file(&guard, "gbk.txt", "中文改写", Some(&read.hash), None).unwrap();
        let raw = fs::read(root.join("gbk.txt")).unwrap();
        let (decoded, _, _) = GBK.decode(&raw);
        assert_eq!(decoded, "中文改写", "写回应保持 GBK 编码");

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn truncation_keeps_full_hash() {
        let root = temp_workspace("truncate");
        let guard = PathGuard::new(root.clone()).unwrap();
        fs::write(root.join("big.txt"), "0123456789").unwrap();

        let read = read_file(&guard, "big.txt", None, Some(4)).unwrap();
        assert!(read.truncated);
        assert_eq!(read.content, "0123");
        assert_eq!(read.size, 10);
        assert_eq!(read.hash, hash_bytes(b"0123456789"));

        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn eol_detection_prefers_dominant_style() {
        assert_eq!(detect_eol("a\nb\n"), Eol::Lf);
        assert_eq!(detect_eol("a\r\nb\r\n"), Eol::Crlf);
        assert_eq!(detect_eol("a\rb\r"), Eol::Cr);
        assert_eq!(detect_eol("no newline"), Eol::Lf);
    }
}
