//! 维护任务（定时清理 / 启动自检）的运行记录模型。
//!
//! 三种后端共用同一套记录结构，便于管理后台统一展示。

/// 运行类型：周期性 / 手动清理。
pub const KIND_CLEANUP: &str = "cleanup";
/// 运行类型：启动一致性自检。
pub const KIND_STARTUP_CHECK: &str = "startup_check";

/// 一次维护任务（清理或自检）的运行结果。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MaintenanceRun {
    pub run_id: String,
    /// `cleanup` 或 `startup_check`。
    pub kind: String,
    /// `startup` / `scheduled` / `manual`。
    pub trigger: String,
    pub started_at: i64,
    pub finished_at: i64,
    pub duration_ms: i64,
    /// `success` / `partial` / `failed`。
    pub status: String,
    /// 扫描到的对象元数据条数。
    pub scanned: i64,
    /// 彻底删除的对象数（含过期回收站对象）。
    pub purged_objects: i64,
    /// 彻底删除释放的字节数。
    pub freed_bytes: i64,
    /// 「有记录无文件」的失效记录数。
    pub invalid_records: i64,
    /// 「有文件无记录」的孤儿文件数。
    pub orphan_files: i64,
    /// 孤儿文件占用的字节数。
    pub orphan_bytes: i64,
    /// 清理掉的过期 nonce 数。
    pub purged_nonces: i64,
    /// 自动修复的条目数（失效记录 + 孤儿文件 + 其他）。
    pub fixed: i64,
    /// 失败原因（status 为 partial / failed 时非空）。
    pub error: Option<String>,
    /// 结构化详情（JSON：示例路径、配置等）。
    pub detail: Option<String>,
}

impl MaintenanceRun {
    pub fn is_success(&self) -> bool {
        self.status == "success"
    }
}

/// 自检 / 清理任务的状态展示值。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MaintenanceConfigView {
    pub trash_retention_days: i64,
    pub cleanup_interval_minutes: u64,
    pub cleanup_max_objects: i64,
    pub cleanup_max_seconds: u64,
    pub startup_check_mode: String,
    pub nonce_retention_seconds: i64,
}
