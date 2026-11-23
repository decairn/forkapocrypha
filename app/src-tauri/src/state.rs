use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex as AsyncMutex;

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
    pub syncing: AsyncMutex<()>,
    pub close_to_tray: AtomicBool,
    pub auto_update_interval_minutes: AtomicU64,
}

const ADDONS_DIR_KEY: &str = "addons_dir_override";
const RESCAN_CATALOG_MAX_AGE: chrono::TimeDelta = chrono::TimeDelta::minutes(5);
const RESCAN_REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

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
            syncing: AsyncMutex::new(()),
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
        run_guarded_sync(&self.syncing, self.sync_catalog(None), |event| {
            emit_sync_event(app, event)
        })
        .await
    }

    pub async fn refresh_catalog_for_rescan(&self, app: &AppHandle) -> Result<()> {
        refresh_if_stale(
            &self.syncing,
            || {
                let conn = self.db.lock().unwrap();
                let last_synced = db::get_metadata(&conn, "last_synced");
                catalog_is_fresh(last_synced.as_deref(), Utc::now())
            },
            async {
                report_sync(self.sync_catalog(Some(RESCAN_REFRESH_TIMEOUT)), |event| {
                    emit_sync_event(app, event);
                })
                .await
                .map(drop)
            },
            RESCAN_REFRESH_TIMEOUT,
        )
        .await
    }

    async fn sync_catalog(&self, timeout: Option<Duration>) -> Result<CatalogMeta> {
        let fetch = async {
            let addons = self.mmoui.fetch_addons().await?;
            let categories = self.mmoui.fetch_categories().await?;
            Ok::<_, anyhow::Error>((addons, categories))
        };
        let (addons, categories) = match timeout {
            Some(duration) => tokio::time::timeout(duration, fetch)
                .await
                .context("ESOUI catalog refresh timed out")??,
            None => fetch.await?,
        };

        let mut conn = self.db.lock().unwrap();
        db::upsert_categories(&mut conn, &categories)?;
        db::upsert_addons(&mut conn, &addons)?;
        db::catalog_meta(&conn)
    }
}

fn catalog_is_fresh(last_synced: Option<&str>, now: DateTime<Utc>) -> bool {
    last_synced
        .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
        .is_some_and(|timestamp| {
            let age = now.signed_duration_since(timestamp);
            (chrono::TimeDelta::zero()..=RESCAN_CATALOG_MAX_AGE).contains(&age)
        })
}

async fn refresh_if_stale(
    syncing: &AsyncMutex<()>,
    is_fresh: impl Fn() -> bool,
    refresh: impl Future<Output = Result<()>>,
    wait_limit: Duration,
) -> Result<()> {
    if is_fresh() {
        return Ok(());
    }

    let Ok(_guard) = syncing.try_lock() else {
        let _guard = tokio::time::timeout(wait_limit, syncing.lock())
            .await
            .context("waiting for the ESOUI catalog refresh timed out")?;
        // The running sync either made the cache fresh or failed. Avoid an
        // immediate second request if it failed.
        return if is_fresh() {
            Ok(())
        } else {
            Err(anyhow!(
                "the in-progress ESOUI catalog refresh did not complete successfully"
            ))
        };
    };

    if !is_fresh() {
        refresh.await?;
    }
    Ok(())
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
    syncing: &AsyncMutex<()>,
    work: impl Future<Output = Result<T>>,
    emit: impl FnMut(SyncEvent<'_, T>),
) -> Result<T> {
    let _guard = syncing.try_lock().map_err(|_| SyncInProgress)?;
    report_sync(work, emit).await
}

async fn report_sync<T>(
    work: impl Future<Output = Result<T>>,
    mut emit: impl FnMut(SyncEvent<'_, T>),
) -> Result<T> {
    emit(SyncEvent::Start);
    let result = work.await;
    match &result {
        Ok(value) => emit(SyncEvent::Done(value)),
        Err(error) => emit(SyncEvent::Error(error)),
    }
    result
}

fn emit_sync_event(app: &AppHandle, event: SyncEvent<'_, CatalogMeta>) {
    match event {
        SyncEvent::Start => {
            let _ = app.emit("catalog:sync:start", ());
        }
        SyncEvent::Done(meta) => {
            let _ = app.emit("catalog:sync:done", meta);
        }
        SyncEvent::Error(error) => {
            let _ = app.emit("catalog:sync:error", error.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::sync::oneshot;

    #[test]
    fn rescan_cache_uses_the_last_successful_sync() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-23T12:05:00Z")
            .unwrap()
            .to_utc();
        assert!(catalog_is_fresh(Some("2026-11-23T12:00:00Z"), now));
        assert!(!catalog_is_fresh(Some("2026-11-23T11:59:59Z"), now));
        assert!(!catalog_is_fresh(Some("2026-11-23T12:05:01Z"), now));
        assert!(!catalog_is_fresh(Some("invalid"), now));
        assert!(!catalog_is_fresh(None, now));
    }

    #[tokio::test]
    async fn rescan_waits_for_active_sync_without_downloading_again() {
        for succeeded in [true, false] {
            let syncing = AsyncMutex::new(());
            let guard = syncing.lock().await;
            let fresh = AtomicBool::new(false);
            let rescan = refresh_if_stale(
                &syncing,
                || fresh.load(Ordering::SeqCst),
                async { panic!("an in-flight refresh must not start another download") },
                Duration::from_secs(1),
            );
            tokio::pin!(rescan);
            assert!(tokio::time::timeout(Duration::ZERO, &mut rescan)
                .await
                .is_err());
            fresh.store(succeeded, Ordering::SeqCst);
            drop(guard);
            assert_eq!(rescan.await.is_ok(), succeeded);
        }
    }

    #[tokio::test]
    async fn rescan_can_time_out_waiting_for_active_sync() {
        let syncing = AsyncMutex::new(());
        let _guard = syncing.lock().await;
        let result = refresh_if_stale(
            &syncing,
            || false,
            async { panic!("a wait timeout must not start another download") },
            Duration::ZERO,
        )
        .await;
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn rescan_reuses_a_fresh_catalog_and_surfaces_failed_downloads() {
        let syncing = AsyncMutex::new(());
        let wait_limit = Duration::from_secs(1);
        refresh_if_stale(
            &syncing,
            || true,
            async { panic!("a fresh catalog must not trigger a download") },
            wait_limit,
        )
        .await
        .unwrap();

        let result = refresh_if_stale(
            &syncing,
            || false,
            async {
                assert!(syncing.try_lock().is_err());
                Err(anyhow!("ESOUI unavailable"))
            },
            wait_limit,
        )
        .await;
        assert_eq!(result.unwrap_err().to_string(), "ESOUI unavailable");
        assert!(syncing.try_lock().is_ok());
    }

    #[tokio::test]
    async fn concurrent_sync_emits_no_events_or_work_and_next_sync_can_start() {
        let syncing = AsyncMutex::new(());
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
                assert!(syncing.try_lock().is_err());
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
        assert!(syncing.try_lock().is_ok());

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
        let syncing = AsyncMutex::new(());
        let events = Mutex::new(Vec::new());
        let result = run_guarded_sync(
            &syncing,
            async { Err::<(), _>(anyhow!("refresh failed")) },
            |event| {
                assert!(syncing.try_lock().is_err());
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
        assert!(syncing.try_lock().is_ok());
        assert_eq!(
            run_guarded_sync(&syncing, async { Ok(7) }, |_| {}).await.unwrap(),
            7
        );
    }
}
