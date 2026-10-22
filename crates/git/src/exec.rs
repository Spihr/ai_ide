//! 进程层：拼命令、跑命令、校验路径。
//!
//! 这一层只做三件事，且每件都有明确理由：
//!
//! 1. **固定环境**：`LC_ALL=C` / `LANG=C` 让 git 的错误信息稳定为英文（要识别
//!    「nothing to commit」得靠它）；`GIT_TERMINAL_PROMPT=0` 保证后台永不弹凭据
//!    输入；`GIT_OPTIONAL_LOCKS=0` 让状态轮询不去抢 `index.lock`（否则编辑器每
//!    两秒一次 `status` 会与用户手敲的 `git add` 互相卡住）；`core.quotepath=false`
//!    让中文路径原样输出。
//! 2. **参数直传**：全程 `Command::arg`，不经 shell。Windows 上也不拼引号，
//!    带空格 / 中文 / 引号的路径不会被二次解释。
//! 3. **路径守卫**：协议来的路径先归一化成「相对工作区根、正斜杠」的形式，
//!    绝对路径与 `..` 一律拒绝。git 的 `-C` 能把工作目录挪走，所以这条守卫是
//!    安全边界而不是洁癖。

use crate::error::GitError;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

/// 指定 git 可执行文件的环境变量（缺省用 `PATH` 里的 `git`）。
pub(crate) const GIT_ENV: &str = "AI_IDE_GIT";

/// 一次命令的原始结果。非零退出也照常返回，由调用方决定是否可以接受。
pub(crate) struct CommandOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandOutput {
    /// stdout 按 UTF-8 宽松解码并去掉尾部空白。
    pub(crate) fn stdout_trimmed(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim().to_string()
    }

    /// stderr 按 UTF-8 宽松解码并去掉尾部空白。
    pub(crate) fn stderr_trimmed(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

/// 绑定到某个工作区根的命令执行器。
pub(crate) struct Runner {
    root: PathBuf,
}

impl Runner {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// 跑一条 git 命令，**不**校验退出码。
    ///
    /// `configs` 会以 `-c key=value` 形式插在子命令之前（用于临时身份等）；
    /// `stdin` 非空时写入子进程标准输入并关闭（提交信息走这条路，避免超长参数）。
    pub(crate) fn execute(
        &self,
        configs: &[(&str, &str)],
        args: &[&str],
        stdin: Option<&str>,
    ) -> Result<CommandOutput, GitError> {
        let mut command = Command::new(git_program());
        command
            .arg("-C")
            .arg(&self.root)
            .arg("-c")
            .arg("core.quotepath=false")
            .arg("-c")
            .arg("color.ui=false");
        for (key, value) in configs {
            command.arg("-c").arg(format!("{key}={value}"));
        }
        command
            .args(args)
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_PAGER", "cat")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                GitError::NotAvailable(format!(
                    "{}（可用环境变量 {GIT_ENV} 指定可执行文件）",
                    git_program().to_string_lossy()
                ))
            } else {
                GitError::Io(format!("启动 git 失败：{err}"))
            }
        })?;

        if let Some(text) = stdin {
            let mut pipe = child
                .stdin
                .take()
                .ok_or_else(|| GitError::Io("git 标准输入不可用".to_string()))?;
            pipe.write_all(text.as_bytes())
                .map_err(|err| GitError::Io(format!("写入 git 标准输入失败：{err}")))?;
            // 必须显式关闭：`commit -F -` 读到 EOF 才动手。
            drop(pipe);
        }

        let output = child
            .wait_with_output()
            .map_err(|err| GitError::Io(format!("等待 git 退出失败：{err}")))?;
        Ok(CommandOutput {
            success: output.status.success(),
            code: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    /// 跑一条命令并要求成功，失败转 [`GitError::Command`]。
    pub(crate) fn execute_checked(
        &self,
        configs: &[(&str, &str)],
        args: &[&str],
        stdin: Option<&str>,
    ) -> Result<CommandOutput, GitError> {
        let output = self.execute(configs, args, stdin)?;
        if !output.success {
            return Err(GitError::Command {
                args: args.join(" "),
                stderr: output.stderr_trimmed(),
                code: output.code,
            });
        }
        Ok(output)
    }

    /// 跑一条命令并返回 trim 后的 stdout。
    pub(crate) fn text(&self, args: &[&str]) -> Result<String, GitError> {
        Ok(self.execute_checked(&[], args, None)?.stdout_trimmed())
    }

    /// 跑一条命令并返回原始 stdout 字节（`-z` 输出不能按行切）。
    pub(crate) fn bytes(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        Ok(self.execute_checked(&[], args, None)?.stdout)
    }

    /// 跑一条命令，忽略退出码只取 stdout（`symbolic-ref` 在分离头指针下返回 1）。
    pub(crate) fn text_allow_failure(&self, args: &[&str]) -> Result<Option<String>, GitError> {
        let output = self.execute(&[], args, None)?;
        if output.success {
            Ok(Some(output.stdout_trimmed()))
        } else {
            Ok(None)
        }
    }
}

/// git 可执行文件：`AI_IDE_GIT` 优先，否则 `PATH` 里的 `git`。
fn git_program() -> OsString {
    std::env::var_os(GIT_ENV).unwrap_or_else(|| OsString::from("git"))
}

/// 把协议来的路径归一化成「相对工作区根、正斜杠」。
///
/// 拒绝：空串、绝对路径（含 `C:\` / `/etc`）、任何 `..` 段。只接受 `.` 段并丢弃，
/// 其余原样保留——git 自己会对不存在的路径报错，这里不替它猜。
pub(crate) fn normalize_path(raw: &str) -> Result<String, GitError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(GitError::Invalid("路径不能为空".to_string()));
    }
    let normalized = trimmed.replace('\\', "/");
    // Windows 盘符（`C:/x`）在非 Windows 上不算绝对路径，单独拦一道。
    let looks_like_drive = normalized.len() >= 2
        && normalized.as_bytes()[1] == b':'
        && normalized.as_bytes()[0].is_ascii_alphabetic();
    if looks_like_drive || Path::new(&normalized).is_absolute() {
        return Err(GitError::Invalid(format!(
            "路径必须是相对工作区根的路径：{raw}"
        )));
    }
    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(GitError::Invalid(format!("路径不得跳出工作区：{raw}")))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(GitError::Invalid(format!(
                    "路径必须是相对工作区根的路径：{raw}"
                )))
            }
        }
    }
    Ok(normalized.trim_start_matches("./").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_pass_through_with_forward_slashes() {
        assert_eq!(normalize_path("src/main.rs").unwrap(), "src/main.rs");
        assert_eq!(normalize_path("src\\main.rs").unwrap(), "src/main.rs");
        assert_eq!(normalize_path("./src/a.rs").unwrap(), "src/a.rs");
        assert_eq!(normalize_path("  文档/说明.md ").unwrap(), "文档/说明.md");
    }

    #[test]
    fn absolute_paths_are_rejected() {
        for raw in ["C:\\Windows\\win.ini", "C:/Users/me/a.txt", "/etc/passwd"] {
            let err = normalize_path(raw).expect_err("绝对路径必须被拒绝");
            assert!(matches!(err, GitError::Invalid(_)), "{raw} → {err:?}");
        }
    }

    #[test]
    fn parent_dir_segments_are_rejected() {
        for raw in ["../secret.txt", "src/../../etc/passwd", "a/../b"] {
            let err = normalize_path(raw).expect_err("越界路径必须被拒绝");
            assert!(matches!(err, GitError::Invalid(_)), "{raw} → {err:?}");
        }
    }

    #[test]
    fn empty_path_is_rejected() {
        assert!(normalize_path("   ").is_err());
        assert!(normalize_path("").is_err());
    }
}
