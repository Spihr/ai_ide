#!/usr/bin/env node
/**
 * 离线 / 受限网络下的 electron-builder 打包入口。
 *
 * 背景：electron-builder 的二进制下载器**不读** .npmrc 的 electron_mirror，
 * 默认仍去 GitHub 拉 electron 压缩包与 NSIS 资源；网络受限时 `pnpm dist` 必挂。
 * 本脚本把两条绕行固化成命令：
 *   1. --config.electronDist=<本地 electron dist>：直接用 node_modules 里已解包的 electron，
 *      完全不下载 electron 本体；
 *   2. ELECTRON_BUILDER_BINARIES_MIRROR：NSIS / winCodeSign 等附加资源走国内镜像
 *      （已显式设置过则不覆盖）。
 *
 * 用法：
 *   node scripts/offline-pack.mjs             # 出 NSIS 安装器（等价 pnpm dist:offline）
 *   node scripts/offline-pack.mjs --dir       # 只出免安装目录（等价 pnpm package:dir:offline）
 *   node scripts/offline-pack.mjs --dry-run   # 只打印探测结果与将执行的命令，不打包
 *
 * 注意：本脚本只负责调用 electron-builder，不负责 cargo 构建与外壳 vite 构建，
 * 前置步骤由 package.json 里的 package:dir:offline / dist:offline 串联。
 */
import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const desktopDir = join(repoRoot, 'apps', 'desktop');
const args = process.argv.slice(2);
const dryRun = args.includes('--dry-run');
const builderArgs = args.filter((a) => !a.startsWith('--dry-run'));

const MIRROR = 'https://npmmirror.com/mirrors/electron-builder-binaries/';

function fail(msg, hint) {
  console.error(`[offline-pack] 失败：${msg}`);
  if (hint) console.error(`[offline-pack] 处理建议：${hint}`);
  process.exit(1);
}

function readElectronRange() {
  const pkgPath = join(desktopDir, 'package.json');
  if (!existsSync(pkgPath)) fail(`找不到 ${pkgPath}`);
  const pkg = JSON.parse(readFileSync(pkgPath, 'utf8'));
  const range =
    pkg.devDependencies?.electron ?? pkg.dependencies?.electron ?? null;
  if (!range) fail('apps/desktop/package.json 未声明 electron 依赖');
  return { range, version: range.replace(/^[^\d]*/, '') };
}

/** 从 .pnpm 目录按 semver 前缀挑出与声明版本一致的 electron 解包目录 */
function findPnpmElectronDist(version) {
  const pnpmDir = join(repoRoot, 'node_modules', '.pnpm');
  if (!existsSync(pnpmDir)) return null;
  const majors = readdirSync(pnpmDir, { withFileTypes: true })
    .filter((d) => d.isDirectory() && d.name.startsWith('electron@'))
    .map((d) => d.name);
  // 精确版本优先；退化为同主版本；避免落到 electron@x_supports-color 这类别名目录
  const exact = majors.find((n) => n === `electron@${version}`);
  const sameMajor = majors.find((n) =>
    n.startsWith(`electron@${version.split('.')[0]}.`),
  );
  const picked = exact ?? sameMajor;
  if (!picked) return null;
  return join(pnpmDir, picked, 'node_modules', 'electron', 'dist');
}

function findElectronDist(version) {
  const candidates = [
    join(desktopDir, 'node_modules', 'electron', 'dist'),
    join(repoRoot, 'node_modules', 'electron', 'dist'),
    findPnpmElectronDist(version),
  ].filter(Boolean);

  for (const dir of candidates) {
    if (!existsSync(dir)) continue;
    // Windows 产物判据：dist 内需有 electron.exe
    if (process.platform === 'win32') {
      if (existsSync(join(dir, 'electron.exe'))) return dir;
    } else if (existsSync(join(dir, 'electron'))) {
      return dir;
    }
  }
  return null;
}

function findBuilderCli() {
  const candidates = [
    join(desktopDir, 'node_modules', 'electron-builder', 'out', 'cli', 'cli.js'),
    join(repoRoot, 'node_modules', 'electron-builder', 'out', 'cli', 'cli.js'),
  ];
  for (const p of candidates) if (existsSync(p)) return p;

  const pnpmDir = join(repoRoot, 'node_modules', '.pnpm');
  if (existsSync(pnpmDir)) {
    const dir = readdirSync(pnpmDir, { withFileTypes: true })
      .filter((d) => d.isDirectory() && d.name.startsWith('electron-builder@'))
      .map((d) => d.name)
      .sort()
      .at(-1);
    if (dir) {
      const p = join(pnpmDir, dir, 'node_modules', 'electron-builder', 'out', 'cli', 'cli.js');
      if (existsSync(p)) return p;
    }
  }
  return null;
}

const { range, version } = readElectronRange();
const electronDist = findElectronDist(version);
if (!electronDist) {
  fail(
    `未找到本地 electron dist（声明版本 ${range}）`,
    '先执行 pnpm install（.npmrc 已配 electron_mirror，安装阶段会走国内镜像），' +
      '或手动指定 --config.electronDist=<electron 解包目录>',
  );
}

const builderCli = findBuilderCli();
if (!builderCli) {
  fail(
    '未找到 electron-builder CLI（node_modules/electron-builder/out/cli/cli.js）',
    '执行 pnpm install 后重试',
  );
}

const mirrorConfigured = Boolean(process.env.ELECTRON_BUILDER_BINARIES_MIRROR);
const env = {
  ...process.env,
  ELECTRON_BUILDER_BINARIES_MIRROR:
    process.env.ELECTRON_BUILDER_BINARIES_MIRROR ?? MIRROR,
};
const finalArgs = ['--config.electronDist=' + electronDist, ...builderArgs];

console.log('[offline-pack] electron 声明版本:', range);
console.log('[offline-pack] 本地 electron dist:', electronDist);
console.log('[offline-pack] electron-builder CLI:', builderCli);
console.log(
  '[offline-pack] builder 二进制镜像:',
  `${env.ELECTRON_BUILDER_BINARIES_MIRROR}${mirrorConfigured ? '（沿用已有环境变量）' : '（本次注入）'}`,
);
console.log('[offline-pack] 目标产物:', builderArgs.includes('--dir') ? 'win-unpacked（--dir）' : 'NSIS 安装器');
console.log('[offline-pack] 执行命令: electron-builder ' + finalArgs.join(' '));

if (dryRun) {
  console.log('[offline-pack] --dry-run：未实际执行打包');
  process.exit(0);
}

const res = spawnSync(process.execPath, [builderCli, ...finalArgs], {
  cwd: desktopDir,
  env,
  stdio: 'inherit',
});
process.exit(res.status ?? 1);
