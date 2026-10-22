# AI IDE 演示工作区

这是 `demo-plugin-system` 的示例工作区，用于演示插件通过 `host/fs.*` 读取内容。

- `README.md`：本文件
- `src/main.js` / `src/util/format.js`：两个 JS 源文件
- `docs/note.md`：一篇 Markdown 笔记
- `.git/config`：敏感路径样本，用来验证路径闸门（插件读它必须被拒）
