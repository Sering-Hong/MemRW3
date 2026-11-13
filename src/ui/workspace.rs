use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct Appearance {
    background_strength: f32, // 背景强度，0–1
    panel_opacity: f32,       // 面板不透明度，0.5–1
}

impl Appearance {
    fn from_workspace(workspace: &WorkspaceSettings) -> Self {
        Self {
            background_strength: workspace.background_strength,
            panel_opacity: workspace.panel_opacity,
        }
    }

    fn apply(self, workspace: &mut WorkspaceSettings) {
        workspace.background_strength = self.background_strength;
        workspace.panel_opacity = self.panel_opacity;
        workspace.normalize();
    }
}

pub struct AppearanceStore {
    path: Option<PathBuf>,         // 本机外观设置文件
    preferred: Option<Appearance>, // 最近选定的外观
    saved: Option<Appearance>,     // 已落盘的外观
    baseline: Appearance,          // 首次启动的默认值
    changed_at: Option<Instant>,   // 最后调整时间
    pub error: Option<String>,     // 可见的保存失败原因
}

impl AppearanceStore {
    pub fn new() -> Self {
        #[cfg(target_os = "windows")]
        let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        #[cfg(not(target_os = "windows"))]
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")));
        Self::at(base.map(|base| base.join("MemRW3").join("appearance.json")))
    }

    fn at(path: Option<PathBuf>) -> Self {
        let loaded = path.as_ref().map(|path| std::fs::read(path)).transpose();
        let (preferred, error) = match loaded {
            Ok(Some(bytes)) => match serde_json::from_slice::<Appearance>(&bytes) {
                Ok(value) => {
                    let mut workspace = WorkspaceSettings::default();
                    value.apply(&mut workspace);
                    (Some(Appearance::from_workspace(&workspace)), None)
                }
                Err(error) => (None, Some(format!("外观设置读取失败：{error}"))),
            },
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                (None, Some(format!("外观设置读取失败：{error}")))
            }
            _ => (None, None),
        };
        Self {
            path,
            preferred,
            saved: preferred,
            baseline: Appearance::from_workspace(&WorkspaceSettings::default()),
            changed_at: None,
            error,
        }
    }

    pub fn restore(&self, workspace: &mut WorkspaceSettings) {
        if let Some(value) = self.preferred {
            value.apply(workspace);
        }
    }

    pub fn poll(&mut self, workspace: &WorkspaceSettings, ctx: &egui::Context) {
        let current = Appearance::from_workspace(workspace);
        if current != self.preferred.unwrap_or(self.baseline) {
            self.preferred = Some(current);
            self.changed_at = Some(Instant::now());
        }
        if let Some(changed) = self.changed_at {
            let delay = Duration::from_millis(400);
            if changed.elapsed() >= delay {
                self.changed_at = None;
                let _ = self.flush(workspace);
            } else {
                ctx.request_repaint_after(delay.saturating_sub(changed.elapsed()));
            }
        }
    }

    pub fn flush(&mut self, workspace: &WorkspaceSettings) -> Result<(), String> {
        let value = Appearance::from_workspace(workspace);
        if self.preferred.is_none() && value == self.baseline {
            return Ok(());
        }
        self.preferred = Some(value);
        if self.saved == Some(value) {
            return Ok(());
        }
        let result = (|| {
            use std::io::Write;
            let path = self.path.as_ref().ok_or("无法确定本机外观设置目录")?;
            let parent = path.parent().ok_or("外观设置路径无效")?;
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            let bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
            let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
            let write_result = (|| {
                let mut file = std::fs::File::create(&temporary)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                drop(file);
                std::fs::rename(&temporary, path)
            })();
            if write_result.is_err() {
                let _ = std::fs::remove_file(&temporary);
            }
            write_result.map_err(|e| e.to_string())
        })();
        match result {
            Ok(()) => {
                self.saved = Some(value);
                self.error = None;
                Ok(())
            }
            Err(error) => {
                self.error = Some(format!("外观自动保存失败：{error}"));
                Err(error)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Arrangement {
    MainTop,  // 上方主区，下方左右并排
    MainLeft, // 左侧主区，右侧上下堆叠
    Columns,  // 全部左右并排
    Rows,     // 全部上下堆叠
}

pub const PANEL_IDS: [&str; 3] = ["chart", "table", "debug"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceSettings {
    pub chart_on_top: bool,               // 旧版排列开关，保留兼容
    pub primary_ratio: f32,               // 主分隔比例，0.15–0.85
    pub secondary_ratio: f32,             // 次分隔比例，0.15–0.85
    pub order: [String; 3],               // 三个面板的显示顺序
    pub background_strength: f32,         // 背景强度，0–1
    pub panel_opacity: f32,               // 面板不透明度，0.5–1
    pub hidden: Vec<String>,              // 隐藏的面板 ID
    pub arrangement: Option<Arrangement>, // 排列方式；空则按旧开关
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            chart_on_top: true,
            primary_ratio: 0.55,
            secondary_ratio: 0.60,
            order: ["chart".into(), "table".into(), "debug".into()],
            background_strength: 0.30,
            panel_opacity: 0.90,
            hidden: vec!["debug".into()],
            arrangement: None,
        }
    }
}

impl WorkspaceSettings {
    pub fn normalize(&mut self) {
        let defaults = Self::default();
        fn bounded(value: f32, default: f32, min: f32, max: f32) -> f32 {
            if value.is_finite() {
                value.clamp(min, max)
            } else {
                default
            }
        }
        self.primary_ratio = bounded(self.primary_ratio, defaults.primary_ratio, 0.15, 0.85);
        self.secondary_ratio = bounded(self.secondary_ratio, defaults.secondary_ratio, 0.15, 0.85);
        self.background_strength = bounded(self.background_strength, 0.30, 0.0, 1.0);
        self.panel_opacity = bounded(self.panel_opacity, 0.90, 0.50, 1.0);
        if !PANEL_IDS
            .iter()
            .all(|id| self.order.iter().filter(|s| s.as_str() == *id).count() == 1)
        {
            self.order = defaults.order;
        }
        let mut hidden = Vec::new();
        for id in PANEL_IDS {
            if self.hidden.iter().any(|h| h == id) {
                hidden.push(id.to_owned());
            }
        }
        // 至少保留一个面板可见。
        if hidden.len() == PANEL_IDS.len() {
            hidden = defaults.hidden;
        }
        self.hidden = hidden;
    }

    pub fn arrangement(&self) -> Arrangement {
        self.arrangement.unwrap_or(if self.chart_on_top {
            Arrangement::MainTop
        } else {
            Arrangement::MainLeft
        })
    }

    pub fn set_arrangement(&mut self, arrangement: Arrangement) {
        self.arrangement = Some(arrangement);
        // 旧版程序只认识这个开关。
        match arrangement {
            Arrangement::MainTop | Arrangement::Rows => self.chart_on_top = true,
            Arrangement::MainLeft | Arrangement::Columns => self.chart_on_top = false,
        }
    }

    pub fn is_hidden(&self, id: &str) -> bool {
        self.hidden.iter().any(|h| h == id)
    }

    /// 隐藏或显示面板；拒绝隐藏最后一个可见面板。
    pub fn set_hidden(&mut self, id: &str, hidden: bool) -> bool {
        if !PANEL_IDS.contains(&id) || self.is_hidden(id) == hidden {
            return false;
        }
        if hidden {
            if self.hidden.len() + 1 >= PANEL_IDS.len() {
                return false;
            }
            self.hidden.push(id.to_owned());
        } else {
            self.hidden.retain(|h| h != id);
        }
        true
    }

    /// 交换两个面板的位置。
    pub fn swap_panels(&mut self, a: &str, b: &str) -> bool {
        let (Some(i), Some(j)) = (
            self.order.iter().position(|id| id == a),
            self.order.iter().position(|id| id == b),
        ) else {
            return false;
        };
        if i == j {
            return false;
        }
        self.order.swap(i, j);
        true
    }

    /// 按排列方式给出各槽位的面板 ID。
    pub fn slots(&self) -> [String; 3] {
        if self.arrangement() == Arrangement::MainLeft {
            // 兼容旧版：左侧主区放第二个面板。
            [
                self.order[1].clone(),
                self.order[0].clone(),
                self.order[2].clone(),
            ]
        } else {
            self.order.clone()
        }
    }

    pub fn controls(&mut self, ui: &mut egui::Ui) -> bool {
        let mut reset = false;
        ui.horizontal_wrapped(|ui| {
            ui.menu_button("工作区布局", |ui| {
                ui.label("排列方式");
                let current = self.arrangement();
                for (arrangement, label) in [
                    (Arrangement::MainTop, "上方主区，下方左右并排"),
                    (Arrangement::MainLeft, "左侧主区，右侧上下堆叠"),
                    (Arrangement::Columns, "全部左右并排"),
                    (Arrangement::Rows, "全部上下堆叠"),
                ] {
                    if ui.selectable_label(current == arrangement, label).clicked() {
                        self.set_arrangement(arrangement);
                    }
                }
                ui.separator();
                ui.label("显示的面板");
                for (id, label) in [("chart", "图表"), ("table", "变量"), ("debug", "调试")] {
                    let mut shown = !self.is_hidden(id);
                    if ui.checkbox(&mut shown, label).changed() {
                        self.set_hidden(id, !shown);
                    }
                }
                ui.separator();
                ui.label("拖动面板标题到另一个面板上可交换位置");
                if ui.button("恢复默认布局").clicked() {
                    reset = true;
                    let background = (self.background_strength, self.panel_opacity);
                    *self = Self::default();
                    (self.background_strength, self.panel_opacity) = background;
                }
            });
            ui.menu_button("背景设置", |ui| {
                ui.label("调整后自动保存，下次启动沿用；加载调参配置不覆盖本机外观。");
                ui.add(
                    egui::Slider::new(&mut self.background_strength, 0.0..=1.0)
                        .text("背景显示强度")
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                );
                ui.add(
                    egui::Slider::new(&mut self.panel_opacity, 0.50..=1.0)
                        .text("面板不透明度")
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                );
            });
        });
        reset
    }
}

pub fn surface(ui: &egui::Ui, color: Color32) -> Color32 {
    let opacity = ui
        .ctx()
        .data_mut(|d| d.get_temp::<f32>(egui::Id::new("workspace_opacity")))
        .unwrap_or(0.90);
    color.gamma_multiply(opacity)
}

#[derive(Default)]
pub struct Background {
    texture: Option<egui::TextureHandle>,
    attempted: bool,
}

impl Background {
    pub fn paint(&mut self, ui: &egui::Ui, strength: f32) {
        if !self.attempted {
            self.attempted = true;
            if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!(
                "../../assets/workspace-background.png"
            )) {
                self.texture = Some(ui.ctx().load_texture(
                    "workspace_background",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [icon.width as usize, icon.height as usize],
                        &icon.rgba,
                    ),
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
        let Some(texture) = &self.texture else {
            return;
        };
        let rect = ui.max_rect();
        let uv = cover_uv(texture.size_vec2(), rect.size());
        ui.painter().image(
            texture.id(),
            rect,
            uv,
            Color32::WHITE.gamma_multiply(strength),
        );
    }
}

fn cover_uv(image: egui::Vec2, area: egui::Vec2) -> egui::Rect {
    let factor = (area.x / image.x).max(area.y / image.y);
    let visible = area / (image * factor);
    egui::Rect::from_center_size(egui::pos2(0.5, 0.5), visible)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("memrw3-appearance-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn appearance_survives_restart_and_replaces_existing_file() {
        let directory = temporary_directory("restart");
        let path = directory.join("appearance.json");
        let mut settings = WorkspaceSettings::default();
        let mut store = AppearanceStore::at(Some(path.clone()));
        settings.background_strength = 0.72;
        settings.panel_opacity = 0.63;
        store.flush(&settings).unwrap();
        let restarted = AppearanceStore::at(Some(path.clone()));
        let mut loaded_config = WorkspaceSettings::default();
        loaded_config.primary_ratio = 0.70;
        restarted.restore(&mut loaded_config);
        assert_eq!(loaded_config.background_strength, 0.72);
        assert_eq!(loaded_config.panel_opacity, 0.63);
        assert_eq!(loaded_config.primary_ratio, 0.70);
        settings.background_strength = 0.0;
        store.flush(&settings).unwrap();
        let restarted = AppearanceStore::at(Some(path.clone()));
        restarted.restore(&mut loaded_config);
        assert_eq!(loaded_config.background_strength, 0.0);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn slider_changes_are_debounced_but_exit_flush_saves_immediately() {
        let directory = temporary_directory("debounce");
        let path = directory.join("appearance.json");
        let mut store = AppearanceStore::at(Some(path.clone()));
        let mut settings = WorkspaceSettings::default();
        let ctx = egui::Context::default();
        settings.panel_opacity = 0.77;
        store.poll(&settings, &ctx);
        assert!(!path.exists());
        store.changed_at = Some(Instant::now() - Duration::from_secs(1));
        store.poll(&settings, &ctx);
        assert!(path.exists());
        settings.panel_opacity = 0.81;
        store.poll(&settings, &ctx);
        store.flush(&settings).unwrap();
        assert_eq!(
            AppearanceStore::at(Some(path.clone()))
                .preferred
                .unwrap()
                .panel_opacity,
            0.81
        );
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn invalid_file_and_write_errors_are_reported_without_false_success() {
        let directory = temporary_directory("failure");
        let path = directory.join("appearance.json");
        std::fs::write(&path, b"invalid json").unwrap();
        let mut store = AppearanceStore::at(Some(path.clone()));
        assert!(store.error.is_some());
        let mut settings = WorkspaceSettings::default();
        settings.background_strength = 0.45;
        store.flush(&settings).unwrap();
        assert!(store.error.is_none());
        let mut blocked = AppearanceStore::at(Some(path.join("impossible.json")));
        assert!(blocked.flush(&settings).is_err());
        assert!(blocked.saved.is_none());
        assert!(blocked.error.is_some());
        assert_eq!(
            AppearanceStore::at(Some(path.clone()))
                .preferred
                .unwrap()
                .background_strength,
            0.45
        );
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn old_workspace_uses_defaults_and_roundtrips() {
        let settings: WorkspaceSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.primary_ratio, 0.55);
        let restored: WorkspaceSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored.order, settings.order);
    }
    #[test]
    fn old_layout_without_new_fields_keeps_its_arrangement_and_gets_default_visibility() {
        let settings: WorkspaceSettings = serde_json::from_str(
            r#"{"chart_on_top":false,"primary_ratio":0.4,"secondary_ratio":0.65,"order":["chart","table","debug"]}"#,
        )
        .unwrap();
        assert_eq!(settings.arrangement(), Arrangement::MainLeft);
        assert_eq!(
            settings.slots(),
            ["table", "chart", "debug"].map(String::from)
        );
        assert_eq!(settings.hidden, vec!["debug".to_owned()]);
    }
    #[test]
    fn panels_swap_hide_and_never_all_disappear() {
        let mut settings = WorkspaceSettings::default();
        assert!(settings.swap_panels("chart", "table"));
        assert_eq!(
            settings.order,
            ["table", "chart", "debug"].map(String::from)
        );
        assert!(!settings.swap_panels("chart", "chart"));
        assert!(!settings.swap_panels("chart", "unknown"));
        assert!(settings.set_hidden("chart", true));
        assert!(!settings.set_hidden("table", true), "最后一个面板不能隐藏");
        assert!(settings.set_hidden("debug", false));
        settings.hidden = vec!["chart".into(), "table".into(), "debug".into(), "x".into()];
        settings.normalize();
        assert_eq!(settings.hidden, vec!["debug".to_owned()]);
        settings.set_arrangement(Arrangement::Columns);
        let restored: WorkspaceSettings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(restored, settings);
    }
    #[test]
    fn invalid_layout_is_repaired() {
        let mut settings = WorkspaceSettings::default();
        settings.primary_ratio = f32::NAN;
        settings.order = ["chart".into(), "chart".into(), "debug".into()];
        settings.normalize();
        assert_eq!(settings.order, WorkspaceSettings::default().order);
        assert_eq!(settings.primary_ratio, 0.55);
    }
    #[test]
    fn embedded_background_decodes_without_external_file() {
        let icon = eframe::icon_data::from_png_bytes(include_bytes!(
            "../../assets/workspace-background.png"
        ))
        .unwrap();
        assert_eq!((icon.width, icon.height), (3840, 2158));
        assert_eq!(icon.rgba.len(), 3840 * 2158 * 4);
    }

    #[test]
    fn cover_crop_preserves_image_aspect() {
        let uv = cover_uv(egui::vec2(3840.0, 2160.0), egui::vec2(800.0, 800.0));
        assert!((uv.height() - 1.0).abs() < 1e-6);
        assert!((uv.width() - 0.5625).abs() < 1e-6);
    }
}
