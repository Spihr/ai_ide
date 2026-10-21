-- M5 切片四：审计事件表。
--
-- 与 `message` 的取舍相反：这里**不挂外键、不做级联删除**。审计的意义是「发生过
-- 什么」，删会话这件事本身就要留痕，会话没了记录还得在；外键还会让审计写入与
-- 会话删除互相排队，把一条旁路日志变成关键路径上的锁。
CREATE TABLE IF NOT EXISTS audit_event (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    -- 事件种类，形如 `session.created`（见 `model::audit_kind`）。
    kind        TEXT    NOT NULL,
    -- 关联会话；跨会话事件（将来的全局开关）允许为空。
    session_id  TEXT,
    -- 发起方：`core` / `user` / `agent`（见 `model::audit_actor`）。
    actor       TEXT    NOT NULL,
    -- 人可读的一句话，写库前已脱敏。
    summary     TEXT    NOT NULL,
    -- 结构化补充信息（JSON 对象），写库前已按敏感键脱敏。
    detail_json TEXT    NOT NULL DEFAULT '{}',
    -- Unix 毫秒，与 session / message 同一口径。
    created_at  INTEGER NOT NULL
);

-- 列表恒定按时间倒序翻页；三个索引都把 created_at / id 带上，
-- 这样「按会话筛完直接排序」不用回表排序。
CREATE INDEX IF NOT EXISTS idx_audit_event_created_at
    ON audit_event (created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_audit_event_session
    ON audit_event (session_id, created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_audit_event_kind
    ON audit_event (kind, created_at DESC, id DESC);
