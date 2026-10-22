-- M5 切片六：权限规则表。
--
-- 对应 `docs/02` §6.2 的 `permission_rule`（字段逐个对齐，不另造 schema）。
-- 这张表只存**用户显式点过「总是允许」**的那些范围：默认策略（工作区内读写放行、
-- 越界拒、高危命令拒）永远先于本表判定，表里收窄不了也放大不了策略，只负责
-- 「这一次别再问我」。
--
-- 关键词：**id 就是免问范围的键**。
-- `crates/agent` 侧的范围键形如 `command:cargo` / `path:edit:src/lib` / `tool:foo`，
-- 直接拿来当主键，重启后原样还原成进程内的免问集合——粒度不会因为落盘而变粗，
-- `run_command` 也不会变成「一次同意、永久放行所有命令」。
CREATE TABLE IF NOT EXISTS permission_rule (
    -- 免问范围的键（见上）。
    id          TEXT    PRIMARY KEY,
    -- 工具名，兼作展示与按工具撤销的过滤条件。
    tool_pattern TEXT   NOT NULL,
    -- 生效范围：工作区绝对路径，或 `global`（跨工作区）。
    scope       TEXT    NOT NULL,
    -- 裁决结果，当前只有 `allow`（deny 不必记：默认就是拒）。
    policy      TEXT    NOT NULL,
    -- Unix 毫秒，与 session / message / audit_event 同一口径。
    created_at  INTEGER NOT NULL
);

-- 列表按时间倒序（最近同意的排前面，撤销时最好找）。
CREATE INDEX IF NOT EXISTS idx_permission_rule_created_at
    ON permission_rule (created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_permission_rule_scope
    ON permission_rule (scope, created_at DESC, id DESC);
