-- OSS Server schema v1 -> v2 升级脚本（MySQL / InnoDB）
--
-- 由 `src/store/mysql_store.rs::apply_upgrade_v2` 在检测到 `_migrations` 中
-- 没有 `schema_v2_mysql` 时逐条执行（按 ';' 拆分，忽略“列/索引已存在”等
-- 重复定义错误），因此对全新 v2 库也是安全的空操作。
--
-- 手工升级时按顺序执行即可；重复执行会因重复定义报 1060/1061/1091，可忽略。
--
-- 升级内容：
--   1. objects 增加 app_id（从 buckets 回填），用于 App 删除后回收站仍可归属；
--   2. 增加回收站字段 deleted_at / original_bucket_id / original_key；
--   3. 解除 objects.bucket_id 外键，删除桶后回收站记录保留；
--   4. 用生成列 active_key 替代 (bucket_id, key) 唯一索引，使回收站中的同 key
--      记录不阻塞新写入；
--   5. 增加回收站与 App 查询索引；
--   6. 新增 maintenance_runs 表（由 001_init.sql 的 CREATE TABLE IF NOT EXISTS 完成）。

ALTER TABLE objects ADD COLUMN app_id VARCHAR(64) NOT NULL DEFAULT '' AFTER object_id;

UPDATE objects o JOIN buckets b ON o.bucket_id = b.bucket_id
SET o.app_id = b.app_id
WHERE o.app_id = '';

ALTER TABLE objects ADD COLUMN deleted_at BIGINT NULL AFTER created_at;

ALTER TABLE objects ADD COLUMN original_bucket_id VARCHAR(64) NULL AFTER deleted_at;

ALTER TABLE objects ADD COLUMN original_key VARCHAR(1024) CHARACTER SET ascii COLLATE ascii_bin NULL AFTER original_bucket_id;

ALTER TABLE objects DROP FOREIGN KEY fk_objects_bucket;

ALTER TABLE objects DROP INDEX uk_objects_bucket_key;

ALTER TABLE objects ADD COLUMN active_key VARCHAR(1024) CHARACTER SET ascii COLLATE ascii_bin
    GENERATED ALWAYS AS (IF(deleted_at IS NULL, `key`, NULL)) STORED AFTER original_key;

ALTER TABLE objects ADD UNIQUE KEY uk_objects_bucket_active (bucket_id, active_key);

ALTER TABLE objects ADD KEY idx_objects_trash (deleted_at, object_id);

ALTER TABLE objects ADD KEY idx_objects_app (app_id);
