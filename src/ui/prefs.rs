//! 本机界面偏好：布局、列显示和侧栏开关，独立于调参 JSON 自动记忆。

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use eframe::egui;
use serde::{Deserialize, Serialize};

use super::workspace::WorkspaceSettings;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPrefs {
    pub workspace: Option<WorkspaceSettings>, // 最近使用的工作区布局
    pub table_hidden_columns: Option<Vec<String>>, // 变量表隐藏的列键
    pub table_wrap_text: Option<bool>,        // 变量表长文本换行
    pub side_panel_open: Option<bool>,        // 调试助手栏是否展开
    pub side_panel_width: Option<f32>,        // 调试助手栏宽度，像素
}

struct Store {
    path: Option<PathBuf>,       // 偏好文件；None 时不落盘
    prefs: UiPrefs,              // 当前偏好
    saved: UiPrefs,              // 已落盘的偏好
    changed_at: Option<Instant>, // 最后修改时间
    error: Option<String>,       // 可见的读写失败原因
}

const SAVE_DELAY: Duration = Duration::from_millis(400);

fn store() -> &'static Mutex<Store> {
    static STORE: OnceLock<Mutex<Store>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(Store::load(default_path())))
}

fn default_path() -> Option<PathBuf> {
    // NOTE: 单元测试不读写使用者的本机偏好文件。
    if cfg!(test) {
        return None;
    }
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")));
    base.map(|base| base.join("MemRW3").join("ui-prefs.json"))
}

impl Store {
    fn load(path: Option<PathBuf>) -> Self {
        let (prefs, error) = match path.as_ref().map(std::fs::read) {
            Some(Ok(bytes)) => match serde_json::from_slice::<UiPrefs>(&bytes) {
                Ok(mut prefs) => {
                    if let Some(workspace) = &mut prefs.workspace {
                        workspace.normalize();
                    }
                    (prefs, None)
                }
                Err(error) => (
                    UiPrefs::default(),
                    Some(format!("界面偏好读取失败：{error}")),
                ),
            },
            Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => (
                UiPrefs::default(),
                Some(format!("界面偏好读取失败：{error}")),
            ),
            _ => (UiPrefs::default(), None),
        };
        Self {
            path,
            saved: prefs.clone(),
            prefs,
            changed_at: None,
            error,
        }
    }

    fn flush(&mut self) {
        self.changed_at = None;
        if self.prefs == self.saved {
            return;
        }
        let Some(path) = self.path.clone() else {
            self.saved = self.prefs.clone();
            return;
        };
        match write_atomic(&path, &self.prefs) {
            Ok(()) => {
                self.saved = self.prefs.clone();
                self.error = None;
            }
            Err(error) => self.error = Some(format!("界面偏好自动保存失败：{error}")),
        }
    }
}

fn write_atomic(path: &std::path::Path, prefs: &UiPrefs) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("界面偏好路径无效")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(prefs).map_err(|e| e.to_string())?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|e| e.to_string())
}

/// 读取当前偏好的副本。
pub fn get() -> UiPrefs {
    store().lock().map(|s| s.prefs.clone()).unwrap_or_default()
}

/// 修改偏好；内容确有变化时进入防抖保存。
pub fn update(change: impl FnOnce(&mut UiPrefs)) {
    if let Ok(mut store) = store().lock() {
        let before = store.prefs.clone();
        change(&mut store.prefs);
        if store.prefs != before {
            store.changed_at = Some(Instant::now());
        }
    }
}

/// 每帧调用：停止调整约 400 ms 后落盘。
pub fn poll(ctx: &egui::Context) {
    if let Ok(mut store) = store().lock() {
        if let Some(changed) = store.changed_at {
            if changed.elapsed() >= SAVE_DELAY {
                store.flush();
            } else {
                ctx.request_repaint_after(SAVE_DELAY.saturating_sub(changed.elapsed()));
            }
        }
    }
}

/// 正常退出时补存尚未落盘的修改。
pub fn flush() {
    if let Ok(mut store) = store().lock() {
        store.flush();
    }
}

pub fn error() -> Option<String> {
    store().lock().ok().and_then(|s| s.error.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_roundtrip_through_file_and_ignore_unknown_or_missing_fields() {
        let directory =
            std::env::temp_dir().join(format!("memrw3-ui-prefs-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("ui-prefs.json");
        let mut store = Store::load(Some(path.clone()));
        assert_eq!(store.prefs, UiPrefs::default());
        store.prefs.table_hidden_columns = Some(vec!["address".into()]);
        store.prefs.side_panel_width = Some(333.0);
        store.flush();
        assert!(store.error.is_none());
        let restarted = Store::load(Some(path.clone()));
        assert_eq!(
            restarted.prefs.table_hidden_columns.as_deref(),
            Some(&["address".to_owned()][..])
        );
        assert_eq!(restarted.prefs.side_panel_width, Some(333.0));
        std::fs::write(&path, br#"{"future_field":1}"#).unwrap();
        assert_eq!(Store::load(Some(path.clone())).prefs, UiPrefs::default());
        std::fs::write(&path, b"not json").unwrap();
        assert!(Store::load(Some(path.clone())).error.is_some());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
