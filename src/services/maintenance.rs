//! 维护服务：启动一致性自检 + 定时/手动清理任务。
//!
//! 设计原则：
//! - 任何删除磁盘文件的操作都**只**发生在这里（彻底删除 / 孤儿文件），
//!   普通删除与覆盖写都只改元数据。
//! - 清理任务用 `try_lock` 保证同一进程内不会并发运行；单个条目的失败不会
//!   终止整轮，任务结束记录为 `partial` 并写明原因。
//! - 单轮有处理条数与耗时上限，避免一次清理把服务拖死。
//! - 清理与上传/下载/还原的并发安全依赖存储层契约：
//!   - 上传写入的是全新 `object_id` 文件，路径不与清理对象重叠；
//!   - 彻底删除先删文件再删元数据，失败时保留元数据，下一轮重试；
//!   - 下载中文件在 Windows 上可能短暂无法删除，按可重试失败处理。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde_json::json;
use tokio::sync::Mutex as AsyncMutex;

use crate::config::{Config, StartupCheckMode};
use crate::models::maintenance::{MaintenanceRun, KIND_CLEANUP, KIND_STARTUP_CHECK};
use crate::models::object::Object;
use crate::services::storage::StorageService;
use crate::store::{Backend, StoreHandle};

/// 扫描磁盘得到的单个文件。
#[derive(Debug, Clone)]
struct DiskFile {
    path: PathBuf,
    size: u64,
}

/// 维护任务的配置快照。
#[derive(Debug, Clone)]
pub struct MaintenanceOptions {
    pub trash_retention_days: i64,
    pub cleanup_max_objects: i64,
    pub cleanup_max_seconds: u64,
    pub nonce_retention_seconds: i64,
}

/// 维护服务。
pub struct MaintenanceService {
    pool: StoreHandle,
    storage: StorageService,
    options: MaintenanceOptions,
    /// 定时清理周期（分钟），0 表示关闭。
    cleanup_interval_minutes: u64,
    /// 保证清理任务串行执行（手动 + 定时共享同一把锁）。
    cleanup_guard: AsyncMutex<()>,
    /// 位于数据目录内、但不属于对象存储的文件（redb 数据库文件）。
    /// 这些文件必须从孤儿文件扫描中排除，否则 `fix` 模式会把数据库本身删掉。
    excluded_paths: Vec<PathBuf>,
}

impl MaintenanceService {
    pub fn new(pool: StoreHandle, storage: StorageService, config: &Config) -> Arc<Self> {
        Arc::new(Self {
            pool,
            storage,
            options: MaintenanceOptions {
                trash_retention_days: config.trash_retention_days,
                cleanup_max_objects: config.cleanup_max_objects,
                cleanup_max_seconds: config.cleanup_max_seconds,
                nonce_retention_seconds: config.nonce_retention_seconds,
            },
            cleanup_interval_minutes: config.cleanup_interval_minutes,
            cleanup_guard: AsyncMutex::new(()),
            excluded_paths: metadata_files(config),
        })
    }

    pub fn options(&self) -> &MaintenanceOptions {
        &self.options
    }

    pub fn interval_minutes(&self) -> u64 {
        self.cleanup_interval_minutes
    }

    /// 最近一次指定类型的运行记录。
    pub async fn latest_run(&self, kind: &str) -> Result<Option<MaintenanceRun>> {
        self.pool.latest_maintenance_run(kind).await
    }

    /// 启动自检：比对元数据与磁盘文件。
    ///
    /// - `report`：只统计并记录，不修改任何数据；
    /// - `fix`：删除失效记录的元数据、删除孤儿文件。
    pub async fn startup_check(
        &self,
        mode: StartupCheckMode,
        trigger: &str,
    ) -> Result<MaintenanceRun> {
        let started_at = chrono::Utc::now().timestamp();
        let timer = Instant::now();

        let mut all_records = self.collect_all_records().await?;
        let disk_files = scan_disk(self.storage.base_dir(), &self.excluded_paths).await?;
        let disk_paths: HashSet<PathBuf> = disk_files.iter().map(|f| f.path.clone()).collect();
        let known_paths: HashSet<PathBuf> = all_records
            .iter()
            .map(|o| self.record_path(o))
            .collect();

        let missing: Vec<Object> = all_records
            .drain(..)
            .filter(|o| !disk_paths.contains(&self.record_path(o)))
            .collect();
        let missing_paths: Vec<String> = missing
            .iter()
            .take(10)
            .map(|o| format!("{}/{}", o.bucket_id, o.key))
            .collect();
        let missing_bytes: i64 = missing.iter().map(|o| o.size).sum();

        let orphans: Vec<DiskFile> = disk_files
            .into_iter()
            .filter(|f| !known_paths.contains(&f.path))
            .collect();
        let orphan_bytes: i64 = orphans.iter().map(|f| f.size as i64).sum();
        let orphan_samples: Vec<String> = orphans
            .iter()
            .take(10)
            .map(|f| f.path.display().to_string())
            .collect();

        let mut fixed = 0i64;
        let mut freed_bytes = 0i64;
        let mut error: Option<String> = None;

        if mode == StartupCheckMode::Fix {
            if !missing.is_empty() {
                let ids: Vec<String> = missing.iter().map(|o| o.object_id.clone()).collect();
                match self.pool.purge_objects(&ids).await {
                    Ok(removed) => fixed += removed as i64,
                    Err(e) => error = Some(format!("failed to purge invalid records: {}", e)),
                }
            }
            for file in &orphans {
                match delete_with_retry(&file.path, 3).await {
                    Ok(()) => {
                        fixed += 1;
                        freed_bytes += file.size as i64;
                    }
                    Err(e) => {
                        error = Some(format!("failed to delete orphan {}: {}", file.path.display(), e))
                    }
                }
            }
        }

        let duration_ms = timer.elapsed().as_millis() as i64;
        let finished_at = chrono::Utc::now().timestamp();
        let status = if error.is_some() {
            if fixed > 0 || mode == StartupCheckMode::Report {
                "partial"
            } else {
                "failed"
            }
        } else {
            "success"
        };

        tracing::info!(
            mode = mode.as_str(),
            missing_records = missing.len(),
            orphan_files = orphans.len(),
            orphan_bytes,
            fixed,
            duration_ms,
            "startup consistency check finished"
        );
        if !missing.is_empty() {
            tracing::warn!(
                missing_records = missing.len(),
                missing_bytes,
                samples = ?missing_paths,
                "objects have metadata but no file on disk"
            );
        }
        if !orphans.is_empty() {
            tracing::warn!(
                orphan_files = orphans.len(),
                orphan_bytes,
                samples = ?orphan_samples,
                "files on disk have no metadata record"
            );
        }

        let run = MaintenanceRun {
            run_id: uuid::Uuid::new_v4().to_string(),
            kind: KIND_STARTUP_CHECK.to_string(),
            trigger: trigger.to_string(),
            started_at,
            finished_at,
            duration_ms,
            status: status.to_string(),
            scanned: (missing.len() + known_paths.len()) as i64,
            purged_objects: if mode == StartupCheckMode::Fix {
                missing.len() as i64
            } else {
                0
            },
            freed_bytes,
            invalid_records: missing.len() as i64,
            orphan_files: orphans.len() as i64,
            orphan_bytes,
            purged_nonces: 0,
            fixed,
            error,
            detail: Some(
                json!({
                    "mode": mode.as_str(),
                    "missing_records": missing.len(),
                    "missing_bytes": missing_bytes,
                    "missing_samples": missing_paths,
                    "orphan_files": orphans.len(),
                    "orphan_bytes": orphan_bytes,
                    "orphan_samples": orphan_samples,
                })
                .to_string(),
            ),
        };
        self.pool.save_maintenance_run(&run).await?;
        Ok(run)
    }

    /// 执行一轮清理。返回 `None` 表示已有清理在运行（并发保护）。
    pub async fn run_cleanup(&self, trigger: &str) -> Result<Option<MaintenanceRun>> {
        let Ok(_guard) = self.cleanup_guard.try_lock() else {
            tracing::info!("cleanup skipped: another cleanup run is in progress");
            return Ok(None);
        };

        let started_at = chrono::Utc::now().timestamp();
        let timer = Instant::now();
        let deadline = timer + Duration::from_secs(self.options.cleanup_max_seconds);
        let mut budget = self.options.cleanup_max_objects.max(0);
        let mut purged_objects = 0i64;
        let mut freed_bytes = 0i64;
        let mut invalid_records = 0i64;
        let mut orphan_files = 0i64;
        let mut orphan_bytes = 0i64;
        let mut fixed = 0i64;
        let mut failures: Vec<String> = Vec::new();

        // 1. 过期回收站对象：彻底删除（先删文件，成功后再删元数据）。
        let retention_days = self.options.trash_retention_days;
        if retention_days > 0 && budget > 0 {
            let before = started_at - retention_days * 86400;
            let batch = budget.min(500);
            let expired = self.pool.list_expired_trash(before, batch).await?;
            for obj in expired {
                if budget <= 0 || Instant::now() >= deadline {
                    break;
                }
                budget -= 1;
                let path = self.record_path(&obj);
                match delete_with_retry(&path, 3).await {
                    Ok(()) => {
                        if self.pool.purge_object(&obj.object_id).await?.is_some() {
                            purged_objects += 1;
                            freed_bytes += obj.size;
                        }
                    }
                    Err(e) => {
                        // 文件删不掉时保留元数据，下一轮重试；不制造孤儿文件。
                        failures.push(format!("delete {}: {}", path.display(), e));
                    }
                }
            }
        }

        // 2. 一致性扫描：失效记录与孤儿文件。
        if budget > 0 && Instant::now() < deadline {
            let mut all_records = self.collect_all_records().await?;
            let disk_files = scan_disk(self.storage.base_dir(), &self.excluded_paths).await?;
            let disk_paths: HashSet<PathBuf> = disk_files.iter().map(|f| f.path.clone()).collect();
            let known_paths: HashSet<PathBuf> =
                all_records.iter().map(|o| self.record_path(o)).collect();

            let missing: Vec<Object> = all_records
                .drain(..)
                .filter(|o| !disk_paths.contains(&self.record_path(o)))
                .take(budget as usize)
                .collect();
            if !missing.is_empty() {
                let ids: Vec<String> = missing.iter().map(|o| o.object_id.clone()).collect();
                invalid_records = ids.len() as i64;
                let removed = self.pool.purge_objects(&ids).await?;
                fixed += removed as i64;
                budget -= ids.len() as i64;
            }

            for file in disk_files
                .into_iter()
                .filter(|f| !known_paths.contains(&f.path))
            {
                if budget <= 0 || Instant::now() >= deadline {
                    break;
                }
                budget -= 1;
                orphan_files += 1;
                let size = file.size as i64;
                orphan_bytes += size;
                match delete_with_retry(&file.path, 2).await {
                    Ok(()) => {
                        fixed += 1;
                        freed_bytes += size;
                    }
                    Err(e) => failures.push(format!(
                        "delete orphan {}: {}",
                        file.path.display(),
                        e
                    )),
                }
            }
        }

        // 3. 过期 nonce。
        let nonce_before = started_at - self.options.nonce_retention_seconds;
        let purged_nonces = self.pool.purge_nonces_before(nonce_before).await? as i64;

        let duration_ms = timer.elapsed().as_millis() as i64;
        let finished_at = chrono::Utc::now().timestamp();
        let error = if failures.is_empty() {
            None
        } else {
            Some(format!(
                "{} item(s) failed: {}",
                failures.len(),
                failures.join("; ")
            ))
        };
        let status = match (&error, fixed + purged_objects) {
            (None, _) => "success",
            (Some(_), 0) => "failed",
            (Some(_), _) => "partial",
        };

        let run = MaintenanceRun {
            run_id: uuid::Uuid::new_v4().to_string(),
            kind: KIND_CLEANUP.to_string(),
            trigger: trigger.to_string(),
            started_at,
            finished_at,
            duration_ms,
            status: status.to_string(),
            scanned: 0,
            purged_objects,
            freed_bytes,
            invalid_records,
            orphan_files,
            orphan_bytes,
            purged_nonces,
            fixed,
            error,
            detail: Some(
                json!({
                    "trash_retention_days": retention_days,
                    "max_objects": self.options.cleanup_max_objects,
                    "max_seconds": self.options.cleanup_max_seconds,
                    "nonce_retention_seconds": self.options.nonce_retention_seconds,
                })
                .to_string(),
            ),
        };
        self.pool.save_maintenance_run(&run).await?;

        tracing::info!(
            trigger,
            purged_objects,
            freed_bytes,
            invalid_records,
            orphan_files,
            purged_nonces,
            duration_ms,
            status,
            "cleanup finished"
        );
        Ok(Some(run))
    }

    fn record_path(&self, obj: &Object) -> PathBuf {
        self.storage
            .object_path(&obj.app_id, &obj.bucket_id, &obj.key, &obj.object_id)
    }

    /// 分页读取全部对象元数据（含回收站）。
    async fn collect_all_records(&self) -> Result<Vec<Object>> {
        let mut records = Vec::new();
        let total = self.pool.count_all_objects().await?;
        let page = 500i64;
        let mut offset = 0i64;
        while offset < total {
            let batch = self.pool.list_all_objects(offset, page).await?;
            if batch.is_empty() {
                break;
            }
            offset += batch.len() as i64;
            records.extend(batch);
        }
        Ok(records)
    }
}

/// 元数据文件（redb 数据库）默认就落在数据目录内，必须排除在孤儿文件扫描之外。
/// 分片上传的临时目录同样属于"非对象数据"，不能当作孤儿文件清理。
fn metadata_files(config: &Config) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from(&config.data_dir).join("multipart")];
    if config.storage_backend != Backend::Redb {
        return paths;
    }
    let raw = config
        .db_url
        .strip_prefix("redb://")
        .unwrap_or(&config.db_url);
    if raw.is_empty() || raw.starts_with("mysql://") {
        paths
    } else {
        paths.push(PathBuf::from(raw));
        paths
    }
}

/// 递归扫描数据目录下的所有普通文件（排除元数据文件）。
async fn scan_disk(base: &Path, excluded: &[PathBuf]) -> Result<Vec<DiskFile>> {
    let base = base.to_path_buf();
    let excluded = excluded.to_vec();
    tokio::task::spawn_blocking(move || -> Result<Vec<DiskFile>> {
        let mut files = Vec::new();
        if base.exists() {
            // 把排除路径转换成为相对 base 的路径，扫描时即可直接比较。
            let canonical_base = std::fs::canonicalize(&base).unwrap_or_else(|_| base.clone());
            let skip: HashSet<PathBuf> = excluded
                .iter()
                .filter_map(|p| std::fs::canonicalize(p).ok())
                .filter_map(|p| p.strip_prefix(&canonical_base).ok().map(Path::to_path_buf))
                .collect();
            walk_dir(&base, Path::new(""), &mut files, &skip)?;
        }
        Ok(files)
    })
    .await
    .map_err(|e| anyhow!("Disk scan task panicked: {}", e))?
}

fn walk_dir(
    dir: &Path,
    rel: &Path,
    out: &mut Vec<DiskFile>,
    skip: &HashSet<PathBuf>,
) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("Failed to read directory {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let child_rel = rel.join(entry.file_name());
        if skip.contains(&child_rel) {
            continue;
        }
        let meta = entry
            .metadata()
            .with_context(|| format!("Failed to stat {}", path.display()))?;
        if meta.is_dir() {
            walk_dir(&path, &child_rel, out, skip)?;
        } else if meta.is_file() {
            // S3 自定义元数据 sidecar：对象文件还在就跳过；对象文件已不存在，
            // 说明它成了无主文件，交给孤儿清理。
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(object_id) = name.strip_suffix(".meta.json") {
                    if dir.join(object_id).exists() {
                        continue;
                    }
                }
            }
            out.push(DiskFile {
                path,
                size: meta.len(),
            });
        }
    }
    Ok(())
}

/// 删除文件并重试（Windows 上文件被下载占用时可能短暂失败）。
async fn delete_with_retry(path: &Path, attempts: u32) -> Result<()> {
    let mut last_error = None;
    for attempt in 0..attempts.max(1) {
        match tokio::fs::remove_file(path).await {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                last_error = Some(e);
                if attempt + 1 < attempts.max(1) {
                    tokio::time::sleep(Duration::from_millis(50 * (attempt as u64 + 1))).await;
                }
            }
        }
    }
    Err(anyhow!(
        "{}",
        last_error.map(|e| e.to_string()).unwrap_or_default()
    ))
}
