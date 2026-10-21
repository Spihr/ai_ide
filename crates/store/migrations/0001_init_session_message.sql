-- M5 切片一：会话与消息持久化（对齐 docs/02 §6.2「SQLite Schema（核心表）」）。
--
-- 约定：
--   * 时间列一律为 Unix 毫秒（INTEGER），模型侧用 i64，不引 chrono；
--   * *_json 列为原样 JSON 文本，编解码在 crates/store 内完成，SQL 不做 JSON 解析；
--   * message 表相对 docs/02 增加 tool_calls_json / tool_call_id 两列，用于对齐
--     docs/04 §4.3 的 Message 定义（字段为超集，不改变原设计语义）；
--   * 主键为 TEXT（UUID v4），表保留隐式 rowid，列表查询用 (created_at, rowid)
--     保证同毫秒消息的插入顺序稳定。

CREATE TABLE IF NOT EXISTS session (
    id             TEXT    PRIMARY KEY,
    title          TEXT    NOT NULL,
    workspace_path TEXT,
    model_ref_json TEXT,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS message (
    id              TEXT    PRIMARY KEY,
    session_id      TEXT    NOT NULL REFERENCES session (id),
    parent_id       TEXT,
    role            TEXT    NOT NULL,
    content_json    TEXT    NOT NULL,
    tool_calls_json TEXT    NOT NULL DEFAULT '[]',
    tool_call_id    TEXT,
    model_ref_json  TEXT,
    usage_json      TEXT,
    created_at      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_message_session ON message (session_id, created_at);
CREATE INDEX IF NOT EXISTS idx_session_updated ON session (updated_at DESC);
