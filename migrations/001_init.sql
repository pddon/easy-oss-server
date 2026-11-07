-- OSS Server 元数据 DDL（MySQL / InnoDB），schema v2
--
-- 该文件由 `src/store/mysql_store.rs` 在启动/测试时通过 include_str! 执行，
-- 不是无人执行的参考文件。所有建表语句幂等（IF NOT EXISTS）。
--
-- v2 变化：
--   * objects 增加 app_id、deleted_at（回收站）、original_bucket_id、original_key；
--   * 用生成列 active_key 实现“仅 active 对象参与 (bucket_id, key) 唯一约束”；
--   * objects.bucket_id 不再有外键，删除桶后回收站记录仍保留（原桶信息在
--     original_bucket_id 中），磁盘文件不会成为无主文件；
--   * 新增 maintenance_runs 表，记录清理任务与启动自检的运行结果。
--
-- 已有 v1 库的升级语句见 002_upgrade_v2.sql（由 MysqlStore::apply_upgrade_v2 执行）。
--
-- 注意：`key` / `active_key` / `original_key` 使用 ascii 字符集，保证唯一索引
-- 不超过 InnoDB 的 3072 字节限制，同时与 `validate_object_key` 只允许 ASCII
-- 字符的规则一致。

CREATE TABLE IF NOT EXISTS apps (
    app_id              VARCHAR(64)  NOT NULL,
    app_secret_encrypted TEXT        NOT NULL,
    name                VARCHAR(255) NOT NULL,
    created_at          BIGINT       NOT NULL,
    PRIMARY KEY (app_id),
    KEY idx_apps_created (created_at, app_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS buckets (
    bucket_id  VARCHAR(64)  NOT NULL,
    app_id     VARCHAR(64)  NOT NULL,
    name       VARCHAR(255) NOT NULL,
    is_public  TINYINT(1)   NOT NULL DEFAULT 0,
    created_at BIGINT       NOT NULL,
    PRIMARY KEY (bucket_id),
    UNIQUE KEY uk_buckets_name (name),
    KEY idx_buckets_app_created (app_id, created_at, bucket_id),
    CONSTRAINT fk_buckets_app FOREIGN KEY (app_id) REFERENCES apps (app_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS objects (
    object_id          VARCHAR(64)   NOT NULL,
    app_id             VARCHAR(64)   NOT NULL,
    bucket_id          VARCHAR(64)   NOT NULL,
    `key`              VARCHAR(1024) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
    size               BIGINT        NOT NULL,
    content_type       VARCHAR(255)  NOT NULL,
    etag               VARCHAR(255)  NOT NULL,
    created_at         BIGINT        NOT NULL,
    deleted_at         BIGINT        NULL,
    original_bucket_id VARCHAR(64)   NULL,
    original_key       VARCHAR(1024) CHARACTER SET ascii COLLATE ascii_bin NULL,
    active_key         VARCHAR(1024) CHARACTER SET ascii COLLATE ascii_bin
                       GENERATED ALWAYS AS (IF(deleted_at IS NULL, `key`, NULL)) STORED,
    PRIMARY KEY (object_id),
    UNIQUE KEY uk_objects_bucket_active (bucket_id, active_key),
    KEY idx_objects_trash (deleted_at, object_id),
    KEY idx_objects_app (app_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS nonces (
    app_id     VARCHAR(64)  NOT NULL,
    nonce      VARCHAR(128) NOT NULL,
    created_at BIGINT       NOT NULL,
    PRIMARY KEY (app_id, nonce),
    KEY idx_nonces_created (created_at),
    CONSTRAINT fk_nonces_app FOREIGN KEY (app_id) REFERENCES apps (app_id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS maintenance_runs (
    run_id         VARCHAR(64) NOT NULL,
    kind           VARCHAR(32) NOT NULL,
    `trigger`      VARCHAR(32) NOT NULL,
    started_at     BIGINT      NOT NULL,
    finished_at    BIGINT      NOT NULL,
    duration_ms    BIGINT      NOT NULL,
    status         VARCHAR(16) NOT NULL,
    scanned        BIGINT      NOT NULL DEFAULT 0,
    purged_objects BIGINT      NOT NULL DEFAULT 0,
    freed_bytes    BIGINT      NOT NULL DEFAULT 0,
    invalid_records BIGINT     NOT NULL DEFAULT 0,
    orphan_files   BIGINT      NOT NULL DEFAULT 0,
    orphan_bytes   BIGINT      NOT NULL DEFAULT 0,
    purged_nonces  BIGINT      NOT NULL DEFAULT 0,
    fixed          BIGINT      NOT NULL DEFAULT 0,
    error          TEXT        NULL,
    detail         TEXT        NULL,
    PRIMARY KEY (run_id),
    KEY idx_runs_kind_started (kind, started_at, run_id)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS _migrations (
    name       VARCHAR(128) NOT NULL,
    applied_at BIGINT       NOT NULL,
    PRIMARY KEY (name)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
