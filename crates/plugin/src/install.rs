//! 安装目录与包复制。
//!
//! P1 的边界很清楚：**只搬文件，不起进程**。`install` 的返回值里没有任何
//! 进程句柄、没有 ready 状态、也没有握手结果——插件要能跑起来，得等 P2 的
//! `plugin.load` 授权齐备后再说。这条边界让 P1 的失败模式收敛成「磁盘读写失败」，
//! 排查时不需要考虑「是不是插件代码把自己写崩了」。
//!
//! 复制策略：先复制到 `plugins/.staging-*`，成功后再替换目标目录。
//! 直接往目标目录里覆盖会在中途失败时留下半个包——那种包能被 `list` 读出来，
//! 但入口文件可能缺一半，属于最难排查的中间态。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::manifest::{self, ValidatedManifest, Violation, MANIFEST_FILE};

/// 数据目录下存放全部插件的子目录名。
pub const PLUGINS_DIR: &str = "plugins";

/// 临时复制目录前缀（与插件 id 拼接，形如 `.staging-demo-hello-...`）。
const STAGING_PREFIX: &str = ".staging-";

/// 进程内序号：同一毫秒内装两个插件时，staging 目录不能撞名。
static STAGING_SEQ: AtomicU64 = AtomicU64::new(0);

/// 安装/卸载失败的两类原因。
#[derive(Debug)]
pub enum PackageError {
    /// 包本身不合法（不存在、缺清单、清单校验不过）。`details` 会原样进 6107。
    Invalid(Vec<Violation>),
    /// 文件系统操作失败（复制 / 改名 / 删除）。与「清单写错」区分开，
    /// 因为用户的处置动作完全不同：一个是改清单，一个是看磁盘权限。
    Io(String),
}

impl PackageError {
    pub fn io(context: impl AsRef<str>, error: &io::Error) -> Self {
        Self::Io(format!("{}：{error}", context.as_ref()))
    }
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(errors) => {
                write!(formatter, "插件包不合法（{} 条问题）", errors.len())
            }
            Self::Io(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for PackageError {}

/// 一次成功安装的结果。
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub manifest: ValidatedManifest,
    /// 安装后的绝对路径：`<data_dir>/plugins/<id>`。
    pub dir: PathBuf,
    /// 是否覆盖了同 id 的旧版本（旧目录或旧账本存在都算）。
    pub replaced: bool,
    /// 入口文件的绝对路径（P2 起进程时用；P1 只做存在性复核）。
    pub entry_path: PathBuf,
}

/// `<data_dir>/plugins`。
pub fn plugins_root(data_dir: &Path) -> PathBuf {
    data_dir.join(PLUGINS_DIR)
}

/// `<data_dir>/plugins/<id>`。
pub fn installed_dir(data_dir: &Path, id: &str) -> PathBuf {
    plugins_root(data_dir).join(id)
}

/// 包是否看起来像个插件包（有 `plugin.json`）。
pub fn looks_like_package(dir: &Path) -> bool {
    dir.join(MANIFEST_FILE).is_file()
}

/// 读取并校验一个包目录，**不写盘**。
///
/// 安装流程先用它把错误挡在磁盘操作之前：清单非法时不应该留下任何目录残留。
pub fn inspect(package_dir: &Path) -> Result<ValidatedManifest, PackageError> {
    if !package_dir.exists() {
        return Err(PackageError::Invalid(vec![Violation::manifest(
            "$",
            format!("插件包不存在：{}", package_dir.display()),
        )]));
    }
    if !package_dir.is_dir() {
        // P1 只支持目录包。压缩包要引入解压与 zip-slip 防护，属于另一片的工作。
        return Err(PackageError::Invalid(vec![Violation::manifest(
            "$",
            "P1 仅支持目录形式的插件包（请先解压再安装）",
        )]));
    }

    let manifest_path = package_dir.join(MANIFEST_FILE);
    let text = match fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(PackageError::Invalid(vec![Violation::manifest(
                "$",
                format!("包内缺少 {MANIFEST_FILE}"),
            )]));
        }
        Err(error) => return Err(PackageError::io("读取 plugin.json 失败", &error)),
    };

    let raw: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            return Err(PackageError::Invalid(vec![Violation::manifest(
                "$",
                format!("plugin.json 不是合法 JSON：{error}"),
            )]));
        }
    };

    let validated = manifest::validate(package_dir, &raw).map_err(PackageError::Invalid)?;

    // 入口也要过一遍 canonicalize：`main` 的字符串层面已经拒绝了 `..`，
    // 但包内一个软链接仍能把 `index.mjs` 指到包外。P2 起进程要用这个路径，
    // 所以在这里就把「入口必须落在包内」钉死，而不是等加载时再判。
    let entry_path = package_dir.join(&validated.entry);
    let canonical_root = fs::canonicalize(package_dir)
        .map_err(|error| PackageError::io("解析插件包路径失败", &error))?;
    let canonical_entry = fs::canonicalize(&entry_path).map_err(|error| {
        PackageError::Invalid(vec![Violation::manifest(
            "$.main",
            format!("main 指向的入口无法解析（{}）：{error}", validated.entry),
        )])
    })?;
    if !canonical_entry.starts_with(&canonical_root) {
        return Err(PackageError::Invalid(vec![Violation::manifest(
            "$.main",
            format!("main 解析后越出插件包根：{}", canonical_entry.display()),
        )]));
    }

    Ok(validated)
}

/// 校验并安装一个目录包，返回落盘位置。
///
/// 校验失败时磁盘上不会出现任何新目录；复制失败时会清掉 staging 目录。
/// 整个过程不读取、不执行包内脚本，也不 spawn 任何进程。
pub fn install(package_dir: &Path, data_dir: &Path) -> Result<InstalledPackage, PackageError> {
    let manifest = inspect(package_dir)?;
    let id = manifest.manifest.id.clone();
    let root = plugins_root(data_dir);
    fs::create_dir_all(&root).map_err(|error| PackageError::io("创建 plugins 目录失败", &error))?;

    let target = root.join(&id);
    let replaced = target.exists();
    let entry_path = target.join(&manifest.entry);

    // 包已经在目标位置（用户直接把数据目录里的包再装一次）：没有可复制的，
    // 也不能删了再拷——那等于把唯一的副本删掉。
    let same_dir = match (fs::canonicalize(package_dir), fs::canonicalize(&target)) {
        (Ok(source), Ok(dest)) => source == dest,
        _ => false,
    };
    if same_dir {
        return Ok(InstalledPackage {
            manifest,
            dir: target,
            replaced: true,
            entry_path,
        });
    }

    let staging = root.join(format!(
        "{STAGING_PREFIX}{id}-{}-{}",
        std::process::id(),
        STAGING_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .map_err(|error| PackageError::io("清理残留 staging 目录失败", &error))?;
    }

    if let Err(error) = copy_tree(package_dir, &staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    // 复核 staging 里的入口：复制过程不跟随软链接，所以这里能确认最终落盘的
    // 结构里入口仍然存在（防止「源包入口是软链接、复制时被跳过」这种漏项）。
    if !staging.join(&manifest.entry).is_file() {
        let _ = fs::remove_dir_all(&staging);
        return Err(PackageError::Invalid(vec![Violation::manifest(
            "$.main",
            format!(
                "复制后入口文件缺失：{}（包内符号链接不会被打包）",
                manifest.entry
            ),
        )]));
    }

    if target.exists() {
        fs::remove_dir_all(&target)
            .map_err(|error| PackageError::io("移除旧版本目录失败", &error))?;
    }
    if let Err(error) = fs::rename(&staging, &target) {
        let _ = fs::remove_dir_all(&staging);
        return Err(PackageError::io("落盘插件目录失败", &error));
    }

    tracing::info!(
        plugin_id = %id,
        version = %manifest.manifest.version,
        replaced,
        "插件包已安装（未启动进程）"
    );

    Ok(InstalledPackage {
        manifest,
        dir: target,
        replaced,
        entry_path,
    })
}

/// 删除已安装目录。返回是否真的删掉了东西。
///
/// 目录被占用（Windows 上插件文件被编辑器打开）不算安装失败：
/// 账本该删还是删，残目录如实报给用户——这比「卸载失败」导致账本与磁盘双份不一致要好。
pub fn remove_installed(data_dir: &Path, id: &str) -> bool {
    let target = installed_dir(data_dir, id);
    if !target.exists() {
        return false;
    }
    match fs::remove_dir_all(&target) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(plugin_id = %id, %error, "插件目录删除失败，账本仍会移除");
            false
        }
    }
}

/// 递归复制目录。
///
/// 不跟随符号链接：跟随会让 `plugins/<id>` 里出现指向用户工作区的目录项，
/// 卸载时 `remove_dir_all` 的语义就变得不可预测（有些平台不删链接目标，
/// 有些会——差异不该由插件作者决定）。
fn copy_tree(source: &Path, target: &Path) -> Result<(), PackageError> {
    fs::create_dir_all(target).map_err(|error| PackageError::io("创建目录失败", &error))?;
    let entries =
        fs::read_dir(source).map_err(|error| PackageError::io("读取源目录失败", &error))?;
    for entry in entries {
        let entry = entry.map_err(|error| PackageError::io("读取目录项失败", &error))?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        let metadata = fs::symlink_metadata(&from)
            .map_err(|error| PackageError::io("读取文件元数据失败", &error))?;
        if metadata.file_type().is_symlink() {
            return Err(PackageError::Invalid(vec![Violation::manifest(
                "$",
                format!("包内不允许符号链接：{}", from.display()),
            )]));
        }
        if metadata.is_dir() {
            copy_tree(&from, &to)?;
        } else if metadata.is_file() {
            fs::copy(&from, &to).map_err(|error| {
                PackageError::io(format!("复制文件失败：{}", from.display()), &error)
            })?;
        }
        // 其余类型（设备、管道、socket）在插件包里没有意义，静默跳过。
    }
    Ok(())
}

/// 复制阶段的 staging 目录是否还需要清理（诊断用；正常流程会自行清掉）。
pub fn staging_dirs(data_dir: &Path) -> Vec<PathBuf> {
    let root = plugins_root(data_dir);
    let Ok(entries) = fs::read_dir(&root) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(STAGING_PREFIX)
        })
        .map(|entry| entry.path())
        .collect()
}

/// 当前时间戳（毫秒），供账本 `installed_at` / `updated_at` 使用。
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    static DIR_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_root(tag: &str) -> PathBuf {
        let seq = DIR_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ai-ide-plugin-install-{tag}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时根目录");
        dir
    }

    /// 在 `root` 下写一个可用的插件包。
    fn write_package(root: &Path, tag: &str, id: &str, capabilities: Vec<&str>) -> PathBuf {
        let dir = root.join(format!("pkg-{tag}"));
        std::fs::create_dir_all(dir.join("lib")).expect("建包目录");
        std::fs::write(dir.join("index.mjs"), "export const x = 1\n").expect("写入口");
        std::fs::write(dir.join("lib").join("util.mjs"), "export const y = 2\n").expect("写子文件");
        let manifest = json!({
            "id": id,
            "name": format!("包 {id}"),
            "version": "1.0.0",
            "pluginApi": "1.0",
            "main": "index.mjs",
            "capabilities": capabilities,
            "tools": [{
                "name": "repo_stats",
                "description": "统计工作区",
                "risk": "read",
                "parameters": { "type": "object", "properties": {} }
            }]
        });
        std::fs::write(
            dir.join(MANIFEST_FILE),
            serde_json::to_string_pretty(&manifest).expect("序列化清单"),
        )
        .expect("写清单");
        dir
    }

    /// 安装 = 复制目录 + 不碰进程；同 id 再装一次要标记 replaced。
    #[test]
    fn install_copies_package_and_marks_replacement() {
        let root = temp_root("copy");
        let data_dir = root.join("data");
        let package = write_package(&root, "copy", "demo-copy", vec!["fs.read"]);

        let first = install(&package, &data_dir).expect("首次安装应成功");
        assert!(!first.replaced, "首次安装不应标记覆盖");
        assert_eq!(first.dir, installed_dir(&data_dir, "demo-copy"));
        assert!(first.dir.join("index.mjs").is_file(), "入口应被复制");
        assert!(
            first.dir.join("lib").join("util.mjs").is_file(),
            "子目录应递归复制"
        );
        assert!(first.entry_path.is_file(), "entry_path 应指向真实文件");
        assert!(
            staging_dirs(&data_dir).is_empty(),
            "复制成功后不应残留 staging 目录"
        );

        // 改一版内容再装：目录被替换，replaced=true。
        std::fs::write(package.join("index.mjs"), "export const x = 2\n").expect("改入口");
        let second = install(&package, &data_dir).expect("覆盖安装应成功");
        assert!(second.replaced, "同 id 再装必须标记覆盖");
        let text = std::fs::read_to_string(second.dir.join("index.mjs")).expect("读入口");
        assert_eq!(text, "export const x = 2\n", "覆盖安装应写入新内容");

        assert!(remove_installed(&data_dir, "demo-copy"));
        assert!(
            !installed_dir(&data_dir, "demo-copy").exists(),
            "卸载应删目录"
        );
        assert!(
            !remove_installed(&data_dir, "demo-copy"),
            "重复卸载返回 false 而不是报错"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 清单非法时一个字节都不该落盘。
    #[test]
    fn invalid_manifest_leaves_no_directory() {
        let root = temp_root("invalid");
        let data_dir = root.join("data");
        let package = root.join("pkg-broken");
        std::fs::create_dir_all(&package).expect("建包目录");
        std::fs::write(package.join("index.mjs"), "// x\n").expect("写入口");
        std::fs::write(
            package.join(MANIFEST_FILE),
            json!({
                "id": "Broken_Pkg",
                "name": "坏包",
                "version": "1.0",
                "pluginApi": "1.0",
                "main": "../escape.mjs",
                "capabilities": ["fs.read"]
            })
            .to_string(),
        )
        .expect("写清单");

        match install(&package, &data_dir) {
            Err(PackageError::Invalid(errors)) => {
                assert!(errors.len() >= 3, "多类问题应一次报全：{errors:?}");
            }
            other => panic!("期望 Invalid，实际：{other:?}"),
        }
        assert!(
            !installed_dir(&data_dir, "Broken_Pkg").exists(),
            "校验失败不能留下安装目录"
        );
        assert!(!package.join("escape.mjs").exists(), "越界路径不应被创建");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 压缩包与不存在的路径都按「包不合法」处理，而不是 IO 错误。
    #[test]
    fn non_directory_package_is_rejected() {
        let root = temp_root("nondir");
        let file = root.join("plugin.zip");
        std::fs::write(&file, b"PK\x03\x04").expect("写假压缩包");
        assert!(matches!(inspect(&file), Err(PackageError::Invalid(_))));
        assert!(matches!(
            inspect(&root.join("missing")),
            Err(PackageError::Invalid(_))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 包内符号链接必须拒载：否则复制后的入口可能指向包外。
    #[test]
    fn symlink_inside_package_is_rejected() {
        let root = temp_root("symlink");
        let data_dir = root.join("data");
        let package = write_package(&root, "symlink", "demo-symlink", vec!["fs.read"]);

        #[cfg(unix)]
        let created = std::os::unix::fs::symlink("/etc/hosts", package.join("link")).is_ok();
        #[cfg(windows)]
        let created =
            std::os::windows::fs::symlink_file(root.join("outside.txt"), package.join("link"))
                .is_ok();

        if !created {
            // Windows 上创建符号链接需要开发者模式或管理员权限，CI 可能拿不到。
            // 那不是被测逻辑的问题，跳过而不是让门禁随机红。
            let _ = std::fs::remove_dir_all(&root);
            return;
        }

        match install(&package, &data_dir) {
            Err(PackageError::Invalid(errors)) => {
                assert!(errors
                    .iter()
                    .any(|error| error.message.contains("符号链接")));
            }
            other => panic!("期望拒绝符号链接，实际：{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `install` 只读清单与文件，绝不执行包内脚本：入口里写一个「被执行就会
    /// 创建标记文件」的脚本，装完标记文件必须不存在。
    #[test]
    fn install_never_executes_entry_script() {
        let root = temp_root("noexec");
        let data_dir = root.join("data");
        let package = write_package(&root, "noexec", "demo-noexec", vec!["fs.read"]);
        let marker = root.join("executed.marker");
        std::fs::write(
            package.join("index.mjs"),
            format!(
                "import {{ writeFileSync }} from 'node:fs'\nwriteFileSync({:?}, 'ran')\n",
                marker.to_string_lossy()
            ),
        )
        .expect("写会留下痕迹的入口");

        install(&package, &data_dir).expect("安装应成功");
        assert!(
            !marker.exists(),
            "安装阶段不得执行插件入口脚本（P1 不启动插件进程）"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 把已安装目录当包再装一次，不能删掉源目录（否则唯一副本没了）。
    #[test]
    fn reinstalling_installed_dir_is_a_no_op() {
        let root = temp_root("inplace");
        let data_dir = root.join("data");
        let package = write_package(&root, "inplace", "demo-inplace", vec!["fs.read"]);
        let first = install(&package, &data_dir).expect("首次安装应成功");

        let again = install(&first.dir, &data_dir).expect("原地重装应成功");
        assert!(again.replaced, "原地重装应标记覆盖");
        assert!(
            again.dir.join("index.mjs").is_file(),
            "原地重装不能删掉原目录"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
