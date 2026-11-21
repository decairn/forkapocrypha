use anyhow::{anyhow, Result};
use rusqlite::Connection;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

const CLOSE_TO_TRAY_KEY: &str = "close_to_tray";
const AUTO_UPDATE_INTERVAL_KEY: &str = "auto_update_interval_minutes";

use crate::api::MmouiClient;
use crate::db;
use crate::models::CatalogMeta;
use crate::scanner;

pub struct AppState {
    pub db: Mutex<Connection>,
    pub addons_dir: Mutex<PathBuf>,
    pub addon_fs_mutations: tokio::sync::Mutex<()>,
    pub backups_dir: PathBuf,
    pub mmoui: MmouiClient,
    pub syncing: AtomicBool,
    pub close_to_tray: AtomicBool,
    pub auto_update_interval_minutes: AtomicU64,
}

const ADDONS_DIR_KEY: &str = "addons_dir_override";

impl AppState {
    pub fn init() -> Result<Self> {
        let data_dir = dirs::data_dir()
            .ok_or_else(|| anyhow!("could not resolve user data directory"))?
            .join("Apocrypha");
        std::fs::create_dir_all(&data_dir)?;
        let db_path = data_dir.join("catalog.db");
        let conn = db::open(&db_path)?;
        // Prefer a previously persisted override; fall back to the OS-default
        // (Documents/Elder Scrolls Online/live/AddOns, OneDrive-aware on Windows).
        let addons_dir = db::get_metadata(&conn, ADDONS_DIR_KEY)
            .map(PathBuf::from)
            .or_else(scanner::default_addons_dir)
            .unwrap_or_else(|| data_dir.join("addons_placeholder"));
        let backups_dir = data_dir.join("backups");
        std::fs::create_dir_all(&backups_dir)?;
        let close_to_tray = db::get_metadata(&conn, CLOSE_TO_TRAY_KEY)
            .map(|v| v == "true")
            .unwrap_or(false);
        let auto_update_interval = db::get_metadata(&conn, AUTO_UPDATE_INTERVAL_KEY)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        Ok(Self {
            db: Mutex::new(conn),
            addons_dir: Mutex::new(addons_dir),
            addon_fs_mutations: tokio::sync::Mutex::new(()),
            backups_dir,
            mmoui: MmouiClient::new(),
            syncing: AtomicBool::new(false),
            close_to_tray: AtomicBool::new(close_to_tray),
            auto_update_interval_minutes: AtomicU64::new(auto_update_interval),
        })
    }

    pub fn persist_addons_dir(&self, path: &str) -> Result<()> {
        let conn = self.db.lock().unwrap();
        db::set_metadata(&conn, ADDONS_DIR_KEY, path)
    }

    pub fn set_close_to_tray(&self, value: bool) -> Result<()> {
        self.close_to_tray.store(value, Ordering::SeqCst);
        let conn = self.db.lock().unwrap();
        db::set_metadata(&conn, CLOSE_TO_TRAY_KEY, if value { "true" } else { "false" })
    }

    pub fn close_to_tray_enabled(&self) -> bool {
        self.close_to_tray.load(Ordering::SeqCst)
    }

    pub fn set_auto_update_interval(&self, minutes: u64) -> Result<()> {
        self.auto_update_interval_minutes
            .store(minutes, Ordering::SeqCst);
        let conn = self.db.lock().unwrap();
        db::set_metadata(&conn, AUTO_UPDATE_INTERVAL_KEY, &minutes.to_string())
    }

    pub fn auto_update_interval(&self) -> u64 {
        self.auto_update_interval_minutes.load(Ordering::SeqCst)
    }

    pub async fn run_sync(&self, app: &AppHandle) -> Result<CatalogMeta> {
        run_guarded_sync(
            &self.syncing,
            async {
                let addons = self.mmoui.fetch_addons().await?;
                let categories = self.mmoui.fetch_categories().await?;

                let mut conn = self.db.lock().unwrap();
                db::upsert_categories(&mut conn, &categories)?;
                db::upsert_addons(&mut conn, &addons)?;
                db::catalog_meta(&conn)
            },
            |event| match event {
                SyncEvent::Start => {
                    let _ = app.emit("catalog:sync:start", ());
                }
                SyncEvent::Done(meta) => {
                    let _ = app.emit("catalog:sync:done", meta);
                }
                SyncEvent::Error(error) => {
                    let _ = app.emit("catalog:sync:error", error.to_string());
                }
            },
        )
        .await
    }
}

#[derive(Debug, thiserror::Error)]
#[error("sync already in progress")]
struct SyncInProgress;

enum SyncEvent<'a, T> {
    Start,
    Done(&'a T),
    Error(&'a anyhow::Error),
}

async fn run_guarded_sync<T>(
    syncing: &AtomicBool,
    work: impl Future<Output = Result<T>>,
    mut emit: impl FnMut(SyncEvent<'_, T>),
) -> Result<T> {
    if syncing.swap(true, Ordering::SeqCst) {
        return Err(SyncInProgress.into());
    }
    // Keep ownership through the terminal event so another sync cannot emit
    // its start before this sync has reported its result.
    let _guard = SyncGuard(syncing);
    emit(SyncEvent::Start);
    let result = work.await;
    match &result {
        Ok(value) => emit(SyncEvent::Done(value)),
        Err(error) => emit(SyncEvent::Error(error)),
    }
    result
}

struct SyncGuard<'a>(&'a AtomicBool);

impl<'a> Drop for SyncGuard<'a> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn concurrent_sync_emits_no_events_or_work_and_next_sync_can_start() {
        let syncing = AtomicBool::new(false);
        let events = Mutex::new(Vec::new());
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let owner = run_guarded_sync(
            &syncing,
            async {
                started_tx.send(()).unwrap();
                release_rx.await.unwrap();
                Ok(42)
            },
            |event| {
                assert!(syncing.load(Ordering::SeqCst));
                events.lock().unwrap().push(match event {
                    SyncEvent::Start => "start",
                    SyncEvent::Done(value) => {
                        assert_eq!(*value, 42);
                        "done"
                    }
                    SyncEvent::Error(_) => "error",
                });
            },
        );
        let contender = async {
            started_rx.await.unwrap();
            let result = run_guarded_sync(
                &syncing,
                async { panic!("concurrent sync work must not run") },
                |_: SyncEvent<'_, ()>| panic!("concurrent sync must not emit events"),
            )
            .await;
            assert!(result.unwrap_err().downcast_ref::<SyncInProgress>().is_some());
            release_tx.send(()).unwrap();
        };
        let (result, ()) = tokio::join!(owner, contender);
        assert_eq!(result.unwrap(), 42);
        assert!(!syncing.load(Ordering::SeqCst));

        run_guarded_sync(&syncing, async { Ok(()) }, |event| {
            events.lock().unwrap().push(match event {
                SyncEvent::Start => "start",
                SyncEvent::Done(_) => "done",
                SyncEvent::Error(_) => "error",
            });
        })
        .await
        .unwrap();
        assert_eq!(*events.lock().unwrap(), ["start", "done", "start", "done"]);
    }

    #[tokio::test]
    async fn failed_sync_emits_error_and_releases_guard() {
        let syncing = AtomicBool::new(false);
        let events = Mutex::new(Vec::new());
        let result = run_guarded_sync(
            &syncing,
            async { Err::<(), _>(anyhow!("refresh failed")) },
            |event| {
                assert!(syncing.load(Ordering::SeqCst));
                events.lock().unwrap().push(match event {
                    SyncEvent::Start => "start",
                    SyncEvent::Done(_) => "done",
                    SyncEvent::Error(error) => {
                        assert_eq!(error.to_string(), "refresh failed");
                        "error"
                    }
                });
            },
        )
        .await;
        assert_eq!(result.unwrap_err().to_string(), "refresh failed");
        assert_eq!(*events.lock().unwrap(), ["start", "error"]);
        assert!(!syncing.load(Ordering::SeqCst));
        assert_eq!(
            run_guarded_sync(&syncing, async { Ok(7) }, |_| {}).await.unwrap(),
            7
        );
    }
}
