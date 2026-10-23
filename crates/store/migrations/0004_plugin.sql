-- 插件系统 P1：插件账本 + 能力授权。
--
-- 对应 `docs/14` §5.4（安装账本）与 §7.1（能力授权），P1 只写「装了什么」，
-- P2 再往同一张表补「什么时候跑起来过、跑了多久、崩了几次」。
--
-- 两张表的分工刻意拆开：`plugin` 一行一个插件（列表页一次查完），
-- `plugin_capability` 一行一个能力（授权是逐项点的，不能塞进一个 JSON 列——
-- 那样「撤销 fs.write」得读改写整串，并发下会互相覆盖）。
--
-- 关键词一：**status 字段现在只在 `installed` 这一档**。
-- P1 不加载插件，所以不可能出现 `ready` / `crashed`；留着这列是因为 P2 的
-- 状态机要落库（重启后不能把 crashed 的插件当成好的加载），建表时就定好枚举值，
-- 免得下一片再来一次 ALTER TABLE。
--
-- 关键词二：**install_dir 存绝对路径**。
-- 安装目录由数据目录推出，但数据目录可能被用户搬走（`AI_IDE_DATA_DIR` 改名）。
-- 存相对路径就得在每次读取时重新拼，拼错了会静默指向别处；存绝对路径至少
-- 在启动自检里能立刻发现「账本指向的目录不存在」并报出来。
CREATE TABLE IF NOT EXISTS plugin (
    -- 清单里的 id（`^[a-z0-9][a-z0-9-]{1,63}$`），同时是安装目录名。
    id                TEXT    PRIMARY KEY,
    -- 展示名与版本，列表页直接读，不解析 manifest_json。
    name              TEXT    NOT NULL,
    version           TEXT    NOT NULL,
    description       TEXT,
    -- 清单声明的宿主 API 版本（major 已校验等于宿主）。
    plugin_api        TEXT    NOT NULL,
    -- 归一化后的包内相对入口（`/` 分隔）。
    main              TEXT    NOT NULL,
    -- 安装后的绝对目录：`<data_dir>/plugins/<id>`。
    install_dir       TEXT    NOT NULL,
    -- 归一化清单全量，重装/审计/回滚时不必回头读磁盘。
    manifest_json     TEXT    NOT NULL,
    -- 能力 / 命令 / 订阅的投影，列表页免解析大 JSON。
    capabilities_json TEXT    NOT NULL,
    commands_json     TEXT    NOT NULL,
    subscriptions_json TEXT   NOT NULL DEFAULT '[]',
    -- 工具数量：列表页展示「N 个工具」，不必解析 manifest。
    tool_count        INTEGER NOT NULL DEFAULT 0,
    -- 生命周期状态：installed / needs_grant / ready / crashed（P1 只产生前两档）。
    status            TEXT    NOT NULL DEFAULT 'installed',
    -- 上次失败原因（P2 起进程失败时写入；P1 只置空）。
    last_error        TEXT,
    -- 崩溃重启计数（P2 用；P1 恒为 0）。
    restarts          INTEGER NOT NULL DEFAULT 0,
    -- Unix 毫秒，与 session / message / audit_event 同一口径。
    installed_at      INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);

-- 列表按安装时间倒序：刚装的排前面，出问题时最好找。
CREATE INDEX IF NOT EXISTS idx_plugin_updated_at
    ON plugin (updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_plugin_status
    ON plugin (status, updated_at DESC);

CREATE TABLE IF NOT EXISTS plugin_capability (
    -- 级联删除：卸载插件后授权残留会让「重新安装后自动获得旧授权」成为可能。
    plugin_id  TEXT    NOT NULL REFERENCES plugin (id) ON DELETE CASCADE,
    -- §7.1 白名单内的能力名（`fs.read` / `fs.write` / …）。
    capability TEXT    NOT NULL,
    -- 1 = 已授权，0 = 已申请未授权（needs_grant 的依据）。
    granted    INTEGER NOT NULL DEFAULT 0,
    -- 授权时间（Unix 毫秒）；未授权为 NULL，避免用 0 冒充「1970 年授权过」。
    granted_at INTEGER,
    PRIMARY KEY (plugin_id, capability)
);

CREATE INDEX IF NOT EXISTS idx_plugin_capability_granted
    ON plugin_capability (capability, granted);
