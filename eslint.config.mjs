// @ts-check
/**
 * 前端 / 脚本 ESLint 配置（flat config）。
 *
 * 覆盖范围：apps/desktop 的 TS/TSX 源码、packages/* 的 TS 源码、scripts/*.mjs。
 * 忽略：构建产物、依赖目录、Rust 产物、ts-rs 生成物（由 pnpm types:gen 产出，禁手改）。
 *
 * 注：文件名使用 .mjs，避免在未设置 `"type": "module"` 的根包下触发
 * MODULE_TYPELESS_PACKAGE_JSON 警告。
 */
import js from '@eslint/js';
import globals from 'globals';
import tseslint from 'typescript-eslint';
import reactHooks from 'eslint-plugin-react-hooks';
import reactRefresh from 'eslint-plugin-react-refresh';

export default tseslint.config(
  {
    ignores: [
      '**/node_modules/**',
      '**/out/**',
      '**/dist/**',
      '**/build/**',
      'target/**',
      'coverage/**',
      // ts-rs 生成物：由 scripts/gen-types.mjs 产出，禁手改，故不参与 lint
      'packages/shared-types/src/generated/**',
    ],
  },

  js.configs.recommended,

  ...tseslint.configs.recommended,

  {
    files: ['**/*.{ts,tsx,mjs,js}'],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: 'module',
      globals: { ...globals.node },
    },
    rules: {
      // 以 `_` 前缀显式声明「有意未使用」
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_', caughtErrorsIgnorePattern: '^_' },
      ],
    },
  },

  {
    files: ['apps/desktop/src/renderer/**/*.{ts,tsx}'],
    languageOptions: {
      globals: { ...globals.browser },
    },
    plugins: {
      'react-hooks': reactHooks,
      'react-refresh': reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      'react-refresh/only-export-components': ['warn', { allowConstantExport: true }],
    },
  },

  {
    // E2E 脚本内含 page.evaluate 的浏览器侧回调，故同时开放浏览器全局
    files: ['scripts/e2e-*.mjs'],
    languageOptions: {
      globals: { ...globals.node, ...globals.browser },
    },
  }
);
