// ===== File: ml_studio/mod.rs — native ML Studio core module =====
//
// ML Studio is a native core module (not a WASM addon). It owns a SEPARATE
// SQLite database (`ml_studio.db`) with its own pool and migrations; identity
// (`owner_user_id`/`org_id`) is referenced at the application level from the
// request `HandlerContext`, never via SQL foreign keys into `tentaflow.db`.

pub mod autolabel_recog_dataset;
pub mod build_recog_dataset;
pub mod coco_annotate;
pub mod db;
pub mod export_llm;
pub mod infer;
pub mod mesh_artifact;
pub mod models;
pub mod profile;
pub mod repository;
pub mod train_autogluon;
pub mod train_llm;
pub mod train_recognition;
pub mod train_tabular;

use std::path::Path;

use anyhow::Result;

use crate::db::DbPool;

/// Initialises ML Studio: opens `<home>/data/ml_studio.db`, runs its migrations
/// and publishes the dedicated pool. Call once at startup, next to `db::init`.
pub fn init(home: &Path) -> Result<DbPool> {
    let db_path = home.join("data").join("ml_studio.db");
    db::init(&db_path)
}
