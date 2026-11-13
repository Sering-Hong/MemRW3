use super::fft::{FftResult, FftWindowType};
use super::legend::ChartLegend;
use crate::dwarf::types::ExtendType;
use crate::model::VariablePool;
use crate::model::VariableReadClass;
use crate::ui::plugin::{
    MemRWPlugin, PluginAction, PluginRenderContext, PluginUpdateContext, ToastLevel,
    VariableCandidate, temp_text_value,
};
use crate::ui::theme;
use eframe::egui::{self, Color32, RichText, Ui};
use egui_plot::{Line, Plot, PlotBounds, PlotPoint, PlotPoints, Points};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufWriter, Write};
use std::time::Instant;

const ZOOM_MODE_BUTTON_SIZE: [f32; 2] = [42.0, 22.0];
const FFT_TOGGLE_BUTTON_SIZE: [f32; 2] = [80.0, 22.0];
const SCROLL_ZOOM_STEP: f64 = 1.02;
const MIN_POINT_MARKER_THRESHOLD: usize = 2;
const DEFAULT_POINT_MARKER_THRESHOLD: usize = 32;
const MAX_POINT_MARKER_THRESHOLD: usize = 1000;
type CursorValue = (String, f64, f64, Color32);
type CursorOverlay = (f32, f32, Vec<CursorValue>);
type FftSeries<'a> = (&'a str, Color32, std::sync::Arc<FftResult>, f64);

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum YAxisMode {
    Auto,
    Fixed { min: f64, max: f64 },
    None,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub enum XAxisMode {
    Auto,
    Fixed(f64),
}

impl XAxisMode {
    fn window(&self, xr: f64) -> f64 {
        match self {
            XAxisMode::Auto => xr.max(6.0),
            XAxisMode::Fixed(w) => *w,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum FftScrollMode {
    Both,
    X,
    Y,
}

impl FftScrollMode {
    fn label(&self) -> &'static str {
        match self {
            FftScrollMode::Both => "双轴",
            FftScrollMode::X => "X",
            FftScrollMode::Y => "Y",
        }
    }
}

pub struct ChartPluginState {
    pub legends: Vec<ChartLegend>,
    pub editing_legend: Option<usize>,
    pub show_line_dialog: bool,
    pub auto_scroll: bool,
    pub x_mode: XAxisMode,
    pub y_mode: YAxisMode,
    pub acq_hz: f64,
    pub removed_var_ids: Vec<usize>,
    pub reset_timer: bool,
    pub edit_curve_name: String,
    pub edit_color: Color32,
    pub edit_buffer_size: usize,
    pub edit_visible: bool,
    pub log_file: Option<std::path::PathBuf>,
    log_writer: Option<BufWriter<std::fs::File>>,
    log_positions: Vec<usize>,
    logging_active: bool,
    pub log_started: bool,
    pub log_stopped: bool,
    pub cursor_txt: String,
    pub show_fft: bool,
    pub show_sparse_points: bool,
    pub sparse_point_threshold: usize,
    pub fft_sample_count: usize,
    pub fft_window_type: FftWindowType,
    pub fft_scroll_mode: FftScrollMode,
    fft_plot_bounds: Option<(f64, f64, f64, f64)>,
    pub td_scroll_mode: FftScrollMode,
    td_plot_bounds: Option<(f64, f64, f64, f64)>,
    acq_frame_count: u64,
    acq_last_reset: Instant,
    was_running: bool,
    legend_open: bool,
    highlighted: Option<(usize, Instant)>,
    highlight_scrolled: bool,
    interval_enabled: bool,
    cursor_a: f64,
    cursor_b: f64,
    stats_cache: Vec<(String, crate::ui::debug_tools::IntervalStats)>,
    stats_updated: Instant,
    export_pending: Option<std::sync::mpsc::Receiver<Result<usize, String>>>,
    export_status: String,
    x_edit: String,
    y_min_edit: String,
    y_max_edit: String,
}

impl Default for ChartPluginState {
    fn default() -> Self {
        Self {
            legends: Vec::new(),
            editing_legend: None,
            show_line_dialog: false,
            auto_scroll: true,
            x_mode: XAxisMode::Auto,
            y_mode: YAxisMode::Auto,
            acq_hz: 0.0,
            removed_var_ids: Vec::new(),
            reset_timer: false,
            edit_curve_name: String::new(),
            edit_color: Color32::WHITE,
            edit_buffer_size: 0,
            edit_visible: true,
            log_file: None,
            log_writer: None,
            log_positions: Vec::new(),
            logging_active: false,
            log_started: false,
            log_stopped: false,
            cursor_txt: String::new(),
            show_fft: false,
            show_sparse_points: false,
            sparse_point_threshold: DEFAULT_POINT_MARKER_THRESHOLD,
            fft_sample_count: 1024,
            fft_window_type: FftWindowType::Hann,
            fft_scroll_mode: FftScrollMode::Both,
            fft_plot_bounds: None,
            td_scroll_mode: FftScrollMode::Both,
            td_plot_bounds: None,
            acq_frame_count: 0,
            acq_last_reset: Instant::now(),
            was_running: false,
            legend_open: true,
            highlighted: None,
            highlight_scrolled: false,
            interval_enabled: false,
            cursor_a: 0.0,
            cursor_b: 1.0,
            stats_cache: Vec::new(),
            stats_updated: Instant::now(),
            export_pending: None,
            export_status: String::new(),
            x_edit: String::new(),
            y_min_edit: String::new(),
            y_max_edit: String::new(),
        }
    }
}

impl ChartPluginState {
    pub fn add_legend(
        &mut self,
        variable_id: usize,
        pool: &VariablePool,
        curve_name: String,
        color: Color32,
    ) {
        if let Some(var) = pool.get(variable_id) {
            let mut legend = ChartLegend::new(variable_id, var.name.clone());
            legend.curve_name = if curve_name.is_empty() {
                var.name.clone()
            } else {
                curve_name
            };
            legend.color = color;
            self.legends.push(legend);
        }
    }
    pub fn remove_legend(&mut self, index: usize) {
        if index < self.legends.len() {
            let var_id = self.legends[index].variable_id;
            self.removed_var_ids.push(var_id);
            self.legends.remove(index);
            if self.editing_legend == Some(index) {
                self.editing_legend = None;
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SavedChartLegend {
    variable_name: String,
    variable_address: u64,
    #[serde(default)]
    variable_type: Option<ExtendType>,
    #[serde(default)]
    variable_size: Option<u32>,
    curve_name: String,
    #[serde(default)]
    comment: String,
    color: [u8; 4],
    visible: bool,
    buffer_size: usize,
    #[serde(default = "default_retain_all")]
    retain_all: bool,
}

#[derive(Serialize, Deserialize)]
struct SavedChartConfig {
    legends: Vec<SavedChartLegend>,
    #[serde(default)]
    show_sparse_points: bool,
    #[serde(default = "default_point_marker_threshold")]
    sparse_point_threshold: usize,
    #[serde(default)]
    x_mode: Option<XAxisMode>,
    #[serde(default)]
    y_mode: Option<YAxisMode>,
    #[serde(default = "default_retain_all")]
    legend_open: bool,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SavedChartPayload {
    Current(SavedChartConfig),
    Legacy(Vec<SavedChartLegend>),
}

fn default_point_marker_threshold() -> usize {
    DEFAULT_POINT_MARKER_THRESHOLD
}

fn default_retain_all() -> bool {
    true
}

impl MemRWPlugin for ChartPluginState {
    fn id(&self) -> &'static str {
        "chart"
    }

    fn title(&self) -> &'static str {
        "图表"
    }

    fn viewport_size(&self) -> egui::Vec2 {
        egui::vec2(720.0, 420.0)
    }

    fn min_viewport_size(&self) -> egui::Vec2 {
        egui::vec2(360.0, 240.0)
    }

    fn update(&mut self, ctx: PluginUpdateContext<'_>) -> Vec<PluginAction> {
        update_chart_data(
            self,
            ctx.pool,
            ctx.frame_data,
            ctx.running,
            ctx.acquisition_requested,
        );
        Vec::new()
    }

    fn add_chart_variables(
        &mut self,
        variables: &[(usize, String, String)],
        pool: &mut VariablePool,
    ) -> Result<bool, String> {
        for (id, _, _) in variables {
            let var = pool.get(*id).ok_or("变量已不存在")?;
            if var.ext_type == ExtendType::Other {
                return Err(format!("{} 不是可绘图的数值类型", var.name));
            }
        }
        let mut changed = false;
        for (id, name, comment) in variables {
            let var = pool.get(*id).ok_or("变量已不存在")?;
            if var.ext_type == ExtendType::Other {
                return Err(format!("{} 不是可绘图的数值类型", var.name));
            }
            if let Some(legend) = self.legends.iter_mut().find(|l| l.variable_id == *id) {
                legend.visible = true;
                legend.comment = comment.clone();
                self.highlighted = Some((*id, Instant::now()));
                self.highlight_scrolled = false;
                continue;
            }
            pool.bind(*id, true, VariableReadClass::Stream);
            self.add_legend(*id, pool, name.clone(), super::legend::default_color(*id));
            if let Some(legend) = self.legends.last_mut() {
                legend.comment = comment.clone();
            }
            self.highlighted = Some((*id, Instant::now()));
            self.highlight_scrolled = false;
            changed = true;
        }
        Ok(changed)
    }

    fn sync_annotations(&mut self, annotations: &[(usize, String, String)]) {
        for (id, _, comment) in annotations {
            if let Some(legend) = self.legends.iter_mut().find(|l| l.variable_id == *id) {
                legend.comment = comment.clone();
            }
        }
    }

    fn reset_data(&mut self) {
        if self.logging_active {
            self.log_stopped = true;
        }
        for legend in &mut self.legends {
            legend.data_history.clear();
            legend.plot_bridge = None;
        }
        self.stats_cache.clear();
        self.stats_updated = Instant::now() - std::time::Duration::from_secs(1);
        self.auto_scroll = true;
        self.td_plot_bounds = None;
        self.fft_plot_bounds = None;
        self.acq_hz = 0.0;
        self.acq_frame_count = 0;
        self.was_running = false;
        self.log_writer = None;
        self.logging_active = false;
    }

    fn render(&mut self, ui: &mut Ui, ctx: PluginRenderContext<'_>) -> Vec<PluginAction> {
        let mut actions = Vec::new();
        if chart_panel(ui, self, ctx.pool, ctx.running) {
            actions.push(PluginAction::OpenVariableTree {
                plugin_id: self.id().to_owned(),
                viewport_id: ctx.viewport_id,
            });
        }

        for var_id in self.removed_var_ids.drain(..) {
            actions.push(PluginAction::RemoveVariable {
                var_id,
                was_enabled: true,
                read_class: VariableReadClass::Stream,
            });
        }
        if self.reset_timer {
            self.reset_timer = false;
            actions.push(PluginAction::ResetTimer);
        }
        if self.log_started {
            self.log_started = false;
            actions.push(PluginAction::Toast {
                level: ToastLevel::Success,
                message: "Log 开始".to_owned(),
            });
        }
        if self.log_stopped {
            self.log_stopped = false;
            actions.push(PluginAction::Toast {
                level: ToastLevel::Success,
                message: "Log 停止".to_owned(),
            });
        }

        actions
    }

    fn add_variable_ui(
        &mut self,
        ui: &mut Ui,
        node_id: usize,
        default_name: &str,
        candidate: &mut dyn FnMut() -> Result<VariableCandidate, String>,
        pool: &mut VariablePool,
    ) -> Result<bool, String> {
        let color_id = ui.make_persistent_id(format!("chart_add_color_{node_id}"));
        let name_id = ui.make_persistent_id(format!("chart_add_name_{node_id}"));
        let name_default_id = ui.make_persistent_id(format!("chart_add_name_default_{node_id}"));
        let mut chart_color =
            ui.data_mut(|d| *d.get_temp_mut_or(color_id, Color32::from_rgb(66, 133, 244)));
        let mut curve_name = temp_text_value(ui, name_id, name_default_id, default_name);

        chart_add_config_ui(ui, default_name, &mut curve_name, &mut chart_color);
        let added = ui.button("添加到 Chart").clicked();

        ui.data_mut(|d| {
            d.insert_temp(color_id, chart_color);
            d.insert_temp(name_id, curve_name.clone());
        });

        if added {
            let candidate = candidate()?;
            let variable_id = if let Some(variable) = pool.find_compatible(
                &candidate.name,
                candidate.address,
                &candidate.ext_type,
                candidate.size,
            ) {
                variable.id
            } else {
                pool.add(&candidate.to_config())
            };
            self.add_legend(variable_id, pool, curve_name, chart_color);
            pool.bind(variable_id, true, VariableReadClass::Stream);
        }
        Ok(added)
    }

    fn is_dialog_open(&self) -> bool {
        self.show_line_dialog
    }

    fn readonly_history(
        &self,
        variable_id: usize,
        seconds: f64,
        limit: usize,
    ) -> Option<serde_json::Value> {
        let legend = self
            .legends
            .iter()
            .find(|line| line.variable_id == variable_id)?;
        let end = legend.data_history.back().map(|point| point.x);
        let cutoff = end.unwrap_or(0.0) - seconds;
        let mut points: Vec<_> = legend
            .data_history
            .iter()
            .rev()
            .take_while(|point| point.x >= cutoff)
            .take(limit.saturating_add(1))
            .map(|point| serde_json::json!({"time_s":point.x,"value":point.y}))
            .collect();
        let truncated = points.len() > limit;
        points.truncate(limit);
        points.reverse();
        Some(
            serde_json::json!({"curve_name":legend.curve_name,"retained_points":legend.data_history.len(),
            "end_time_s":end,"requested_seconds":seconds,"truncated":truncated,"points":points,
            "display_compressed":false,"retention_limited":legend.buffer_size>0}),
        )
    }

    fn save_config(&self, pool: &VariablePool) -> serde_json::Value {
        let legends: Vec<SavedChartLegend> = self
            .legends
            .iter()
            .map(|legend| {
                let var = pool.get(legend.variable_id);
                SavedChartLegend {
                    variable_name: var.map(|v| v.name.clone()).unwrap_or_default(),
                    variable_address: var.map(|v| v.address).unwrap_or(0),
                    variable_type: var.map(|v| v.ext_type.clone()),
                    variable_size: var.map(|v| v.size),
                    curve_name: legend.curve_name.clone(),
                    comment: legend.comment.clone(),
                    color: [
                        legend.color.r(),
                        legend.color.g(),
                        legend.color.b(),
                        legend.color.a(),
                    ],
                    visible: legend.visible,
                    buffer_size: legend.buffer_size,
                    retain_all: legend.buffer_size == 0,
                }
            })
            .collect();
        serde_json::to_value(SavedChartConfig {
            legends,
            x_mode: Some(self.x_mode.clone()),
            y_mode: Some(self.y_mode.clone()),
            legend_open: self.legend_open,
            show_sparse_points: self.show_sparse_points,
            sparse_point_threshold: self.sparse_point_threshold,
        })
        .unwrap_or(serde_json::Value::Null)
    }

    fn load_config(
        &mut self,
        payload: &serde_json::Value,
        pool: &mut VariablePool,
    ) -> Result<(), String> {
        let (legends, show_sparse_points, sparse_point_threshold) = match serde_json::from_value::<
            SavedChartPayload,
        >(payload.clone())
        .map_err(|e| e.to_string())?
        {
            SavedChartPayload::Current(config) => {
                self.x_mode = config.x_mode.unwrap_or(XAxisMode::Auto);
                self.y_mode = config.y_mode.unwrap_or(YAxisMode::Auto);
                self.legend_open = config.legend_open;
                if let XAxisMode::Fixed(value) = self.x_mode {
                    self.x_edit = value.to_string();
                }
                if let YAxisMode::Fixed { min, max } = self.y_mode {
                    self.y_min_edit = min.to_string();
                    self.y_max_edit = max.to_string();
                }
                (
                    config.legends,
                    config.show_sparse_points,
                    config
                        .sparse_point_threshold
                        .clamp(MIN_POINT_MARKER_THRESHOLD, MAX_POINT_MARKER_THRESHOLD),
                )
            }
            SavedChartPayload::Legacy(legends) => (legends, false, DEFAULT_POINT_MARKER_THRESHOLD),
        };

        self.legends.clear();
        self.show_sparse_points = show_sparse_points;
        self.sparse_point_threshold = sparse_point_threshold;
        for saved in legends {
            let var_id = saved
                .variable_type
                .as_ref()
                .zip(saved.variable_size)
                .and_then(|(ext_type, size)| {
                    pool.find_compatible(
                        &saved.variable_name,
                        saved.variable_address,
                        ext_type,
                        size,
                    )
                })
                .or_else(|| pool.find_by_name_addr(&saved.variable_name, saved.variable_address))
                .map(|v| v.id)
                .ok_or_else(|| format!("图表变量 \"{}\" 匹配失败", saved.variable_name))?;

            let mut legend = ChartLegend::new(var_id, saved.variable_name.clone());
            legend.curve_name = saved.curve_name;
            legend.comment = saved.comment;
            legend.color = Color32::from_rgba_premultiplied(
                saved.color[0],
                saved.color[1],
                saved.color[2],
                saved.color[3],
            );
            legend.visible = saved.visible;
            legend.buffer_size = if saved.retain_all {
                0
            } else {
                saved.buffer_size
            };
            self.legends.push(legend);
            pool.bind(var_id, true, VariableReadClass::Stream);
        }
        Ok(())
    }
}

pub fn chart_add_config_ui(
    ui: &mut Ui,
    node_name: &str,
    out_name: &mut String,
    out_color: &mut Color32,
) {
    if out_name.is_empty() {
        *out_name = node_name.to_string();
    }
    ui.horizontal(|ui| {
        ui.label("曲线名:");
        ui.text_edit_singleline(out_name);
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("颜色:");
        color_pick(ui, out_color);
    });
}

fn color_pick(ui: &mut Ui, current: &mut Color32) {
    ui.vertical(|ui| {
        ui.color_edit_button_srgba(current);
        let colors = super::legend::preset_colors();
        egui::Grid::new("add_color_grid").show(ui, |ui| {
            for (i, &c) in colors.iter().enumerate() {
                let fill = if *current == c {
                    c
                } else {
                    c.linear_multiply(0.5)
                };
                if ui
                    .add_sized([18.0, 18.0], egui::Button::new("").fill(fill))
                    .clicked()
                {
                    *current = c;
                }
                if (i + 1) % 6 == 0 {
                    ui.end_row();
                }
            }
        });
    });
}

fn update_chart_data(
    state: &mut ChartPluginState,
    pool: &VariablePool,
    frame_data: &HashMap<usize, Vec<(f64, [u8; 8])>>,
    running: bool,
    acquisition_requested: bool,
) {
    if acquisition_requested {
        if !state.was_running {
            state.acq_frame_count = 0;
            state.acq_last_reset = Instant::now();
            state.was_running = true;
            if let Some(log_file) = &state.log_file {
                match std::fs::File::create(log_file) {
                    Ok(file) => {
                        let mut writer = BufWriter::new(file);
                        let _ = write!(writer, "timestamp");
                        for legend in &state.legends {
                            let _ = write!(writer, ",{}", legend.curve_name);
                        }
                        let _ = writeln!(writer);
                        state.log_writer = Some(writer);
                        state.logging_active = true;
                        state.log_started = true;
                    }
                    Err(_) => state.logging_active = false,
                }
            }
        }
        if running {
            for legend in &mut state.legends {
                if let Some(data) = frame_data.get(&legend.variable_id) {
                    let Some(variable) = pool.get(legend.variable_id) else {
                        continue;
                    };
                    let sample_count = data.len() as u64;
                    let skip = legend.prepare_batch(data.len());
                    for (timestamp, raw) in data.iter().skip(skip) {
                        let value = decode_value_f64(raw, &variable.ext_type);
                        legend.push_prepared(*timestamp, value);
                    }
                    state.acq_frame_count += sample_count;
                }
            }
        }
        let elapsed = state.acq_last_reset.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            state.acq_hz = state.acq_frame_count as f64 / elapsed;
            state.acq_frame_count = 0;
            state.acq_last_reset = Instant::now();
        }
    } else {
        if state.logging_active {
            state.log_writer = None;
            state.logging_active = false;
            state.log_stopped = true;
        }
        state.was_running = false;
    }

    if running && state.logging_active {
        if let Some(writer) = &mut state.log_writer {
            write_log_frame(
                writer,
                &mut state.log_positions,
                &state.legends,
                pool,
                frame_data,
            );
            let _ = writer.flush();
        }
    }
}

pub fn chart_panel(
    ui: &mut Ui,
    state: &mut ChartPluginState,
    pool: &VariablePool,
    running: bool,
) -> bool {
    let mut open_tree = false;
    history_tools(ui, state);

    ui.vertical(|ui| {
        let dialog_is_open = state.show_line_dialog;

        ui.add_enabled_ui(!dialog_is_open, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading(RichText::new("实时数据图表").size(16.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(!state.logging_active, |ui| {
                        if ui
                            .button(RichText::new("📋 打开变量树").size(12.0))
                            .clicked()
                        {
                            open_tree = true;
                        }
                    });
                    if !state.cursor_txt.is_empty() {
                        ui.label(
                            RichText::new(&state.cursor_txt)
                                .size(11.0)
                                .color(theme::muted_text(ui)),
                        );
                    }
                });
            });
            ui.add_space(2.0);

            ui.horizontal_wrapped(|ui| {
                if ui.button("清空").clicked() {
                    for legend in &mut state.legends {
                        legend.data_history.clear();
                    }
                    state.auto_scroll = true;
                    state.td_plot_bounds = None;
                    state.acq_hz = 0.0;
                    state.acq_frame_count = 0;
                    state.reset_timer = true;
                }
                ui.toggle_value(&mut state.legend_open, "曲线列表");
                if ui.button("回到最新").clicked() {
                    state.auto_scroll = true;
                    state.td_plot_bounds = None;
                }
                if ui
                    .button("查看全部")
                    .on_hover_text("显示从开始到现在的全部曲线")
                    .clicked()
                {
                    state.x_mode = XAxisMode::Auto;
                    state.auto_scroll = true;
                    state.td_plot_bounds = None;
                }
                ui.separator();
                if fft_toggle_button(ui, state.show_fft).clicked() {
                    state.show_fft = !state.show_fft;
                }
                ui.checkbox(&mut state.show_sparse_points, "标注点")
                    .on_hover_text("当前 Plot 视图内的点数小于临界值时，在可见曲线上绘制采样点");
                if state.show_sparse_points {
                    ui.add(
                        egui::DragValue::new(&mut state.sparse_point_threshold)
                            .range(MIN_POINT_MARKER_THRESHOLD..=MAX_POINT_MARKER_THRESHOLD)
                            .prefix("临界 ")
                            .suffix(" 点"),
                    );
                }
                ui.separator();
                ui.label("缩放:");
                for &mode in &[FftScrollMode::X, FftScrollMode::Y, FftScrollMode::Both] {
                    if zoom_mode_button(ui, state.td_scroll_mode == mode, mode).clicked() {
                        state.td_scroll_mode = mode;
                        state.td_plot_bounds = None;
                    }
                }
                ui.separator();
                egui::ComboBox::from_label("X轴")
                    .selected_text(x_mode_label(&state.x_mode))
                    .width(80.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(state.x_mode == XAxisMode::Auto, "完整历史")
                            .clicked()
                        {
                            state.x_mode = XAxisMode::Auto;
                            state.auto_scroll = true;
                            state.td_plot_bounds = None;
                        }
                        if ui
                            .selectable_label(
                                matches!(state.x_mode, XAxisMode::Fixed(_)),
                                "最近时间窗",
                            )
                            .clicked()
                        {
                            state.x_mode = XAxisMode::Fixed(10.0);
                            state.x_edit = "10.000".to_owned();
                            state.auto_scroll = true;
                        }
                    });
                if let XAxisMode::Fixed(w) = &mut state.x_mode {
                    let response = ui.add_sized(
                        [65.0, 20.0],
                        egui::TextEdit::singleline(&mut state.x_edit).hint_text("s"),
                    );
                    if response.lost_focus() {
                        if let Ok(v) = state.x_edit.parse::<f64>() {
                            if v.is_finite() && v > 0.0 {
                                *w = v;
                                state.auto_scroll = true;
                            }
                        }
                    }
                    ui.label("s");
                }
                egui::ComboBox::from_label("Y轴")
                    .selected_text(y_mode_label(&state.y_mode))
                    .width(80.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(state.y_mode == YAxisMode::Auto, "自动")
                            .clicked()
                        {
                            state.y_mode = YAxisMode::Auto;
                            state.auto_scroll = true;
                            state.td_plot_bounds = None;
                        }
                        if ui
                            .selectable_label(
                                matches!(state.y_mode, YAxisMode::Fixed { .. }),
                                "固定",
                            )
                            .clicked()
                        {
                            let (lo, hi) = visible_history_bounds(&state.legends)
                                .map(|(_, _, y_min, y_max)| (y_min, y_max))
                                .unwrap_or((0.0, 0.0));
                            let range = (hi - lo).max(10.0);
                            let min = lo - range * 0.1;
                            let max = hi + range * 0.1;
                            state.y_mode = YAxisMode::Fixed { min, max };
                            state.y_min_edit = format!("{min:.2}");
                            state.y_max_edit = format!("{max:.2}");
                            state.auto_scroll = true;
                        }
                        if ui
                            .selectable_label(state.y_mode == YAxisMode::None, "无")
                            .clicked()
                        {
                            state.y_mode = YAxisMode::None;
                        }
                    });
                if let YAxisMode::Fixed { min, max } = &mut state.y_mode {
                    ui.label("最小值：");
                    let min_response = ui.add_sized(
                        [65.0, 20.0],
                        egui::TextEdit::singleline(&mut state.y_min_edit),
                    );
                    ui.label("最大值：");
                    let max_response = ui.add_sized(
                        [65.0, 20.0],
                        egui::TextEdit::singleline(&mut state.y_max_edit),
                    );
                    if min_response.lost_focus() || max_response.lost_focus() {
                        if let (Ok(lo), Ok(hi)) = (
                            state.y_min_edit.parse::<f64>(),
                            state.y_max_edit.parse::<f64>(),
                        ) {
                            if lo.is_finite() && hi.is_finite() && lo < hi {
                                *min = lo;
                                *max = hi;
                                state.auto_scroll = true;
                            }
                        }
                    }
                }
                if !state.auto_scroll {
                    ui.colored_label(theme::palette(ui).accent_hover, "手动查看中");
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("实时记录：");
                    ui.add_enabled_ui(!state.logging_active, |ui| {
                        if ui.button("选择文件").clicked() {
                            if let Some(p) = crate::ui::file_dialog()
                                .add_filter("CSV", &["csv"])
                                .set_file_name("data.csv")
                                .save_file()
                            {
                                state.log_file = Some(p);
                            }
                        }
                        if state.log_file.is_some() && ui.button("清除").clicked() {
                            state.log_file = None;
                        }
                    });
                    if let Some(ref p) = state.log_file {
                        ui.label(
                            p.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string(),
                        );
                    } else {
                        ui.label("未选择记录文件");
                    }
                });
            });
            ui.add_space(2.0);
            let points: usize = state.legends.iter().map(|l| l.data_history.len()).sum();
            let history = if state.legends.iter().all(|l| l.buffer_size == 0) {
                "完整历史"
            } else {
                "部分曲线使用限长缓冲"
            };
            ui.label(
                RichText::new(format!(
                    "{history} · 已保留 {points} 点 · 暂停后保留，清空后重新记录"
                ))
                .size(11.0)
                .color(theme::muted_text(ui)),
            );

            if state.legends.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("暂无监控变量")
                            .size(13.0)
                            .color(theme::muted_text(ui)),
                    );
                    ui.add_enabled_ui(!state.logging_active, |ui| {
                        if ui.button("📋 打开变量树").clicked() {
                            open_tree = true;
                        }
                    });
                });
            } else if state.show_fft {
                let total_h = ui.available_height();
                let td_h = (total_h * 0.55).max(120.0);
                let fft_h = (total_h - td_h - 6.0).max(80.0);

                let (td_rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), td_h),
                    egui::Sense::hover(),
                );
                let mut td_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(td_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                render_chart(&mut td_ui, state);

                ui.add_space(4.0);

                let (fft_rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), fft_h),
                    egui::Sense::hover(),
                );
                let mut fft_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(fft_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                render_fft_chart(&mut fft_ui, state);
            } else {
                render_chart(ui, state);
            }
        });

        if state.show_line_dialog {
            if let Some(edit_idx) = state.editing_legend {
                let (ext_info, win_title) = {
                    let legend = &state.legends[edit_idx];
                    let ext_info = pool
                        .get(legend.variable_id)
                        .map(|v| (v.name.clone(), v.address, v.ext_type.clone(), v.size));
                    (ext_info, legend.curve_name.clone())
                };
                let mut action: Option<super::line_dialog::DialogAction> = None;
                egui::Modal::new(egui::Id::new("line_dialog_modal")).show(ui.ctx(), |ui| {
                    ui.set_width(320.0);
                    egui::Frame::NONE
                        .inner_margin(egui::Margin {
                            left: 20,
                            right: 20,
                            top: 16,
                            bottom: 16,
                        })
                        .show(ui, |ui| {
                            ui.heading(format!("曲线属性 - {}", win_title));
                            ui.separator();
                            let (ext_name, ext_addr, ext_type, ext_size) =
                                ext_info.unwrap_or((String::new(), 0, ExtendType::U32, 0));
                            action = super::line_dialog::line_dialog_ui(
                                ui,
                                &mut state.edit_curve_name,
                                &mut state.edit_color,
                                &mut state.edit_buffer_size,
                                &mut state.edit_visible,
                                &ext_name,
                                ext_addr,
                                &ext_type,
                                ext_size,
                                running,
                                state.logging_active,
                            );
                        });
                });
                if let Some(act) = action {
                    match act {
                        super::line_dialog::DialogAction::Delete => {
                            state.remove_legend(edit_idx);
                        }
                        super::line_dialog::DialogAction::Cancel => {}
                        super::line_dialog::DialogAction::Confirm => {
                            let legend = &mut state.legends[edit_idx];
                            legend.curve_name = std::mem::take(&mut state.edit_curve_name);
                            legend.color = state.edit_color;
                            legend.visible = state.edit_visible;
                            if legend.buffer_size != state.edit_buffer_size {
                                legend.buffer_size = state.edit_buffer_size;
                                legend.prepare_batch(0);
                                legend.refresh_plot_bridge();
                            }
                        }
                    }
                    state.show_line_dialog = false;
                }
            } else {
                state.show_line_dialog = false;
            }
        }
    });
    open_tree
}

fn write_log_frame(
    writer: &mut impl Write,
    positions: &mut Vec<usize>,
    legends: &[ChartLegend],
    pool: &VariablePool,
    frame_data: &HashMap<usize, Vec<(f64, [u8; 8])>>,
) {
    positions.clear();
    positions.resize(legends.len(), 0);

    loop {
        let next_timestamp = legends
            .iter()
            .enumerate()
            .filter_map(|(index, legend)| {
                frame_data
                    .get(&legend.variable_id)
                    .and_then(|samples| samples.get(positions[index]))
                    .map(|(timestamp, _)| *timestamp)
            })
            .min_by(f64::total_cmp);
        let Some(timestamp) = next_timestamp else {
            break;
        };

        let _ = write!(writer, "{timestamp:.6}");
        for (index, legend) in legends.iter().enumerate() {
            let sample = frame_data
                .get(&legend.variable_id)
                .and_then(|samples| samples.get(positions[index]));
            if let Some((sample_timestamp, raw)) = sample {
                let same_timestamp = sample_timestamp.total_cmp(&timestamp).is_eq()
                    || (*sample_timestamp - timestamp).abs() < 1e-9;
                if same_timestamp {
                    positions[index] += 1;
                    if let Some(var) = pool.get(legend.variable_id) {
                        let value = decode_value_f64(raw, &var.ext_type);
                        let _ = write!(writer, ",{value:.6}");
                        continue;
                    }
                }
            }
            let _ = write!(writer, ",");
        }
        let _ = writeln!(writer);
    }
}

type HistoryExport = Vec<(String, String, Vec<(f64, f64)>)>;
fn write_history_csv(writer: &mut impl Write, histories: &HistoryExport) -> std::io::Result<usize> {
    use crate::ui::debug_tools::csv_field;
    writeln!(writer, "\u{feff}变量路径,中文注释,采样时间秒,数值")?;
    let mut count = 0;
    for (path, comment, points) in histories {
        let prefix = format!("{},{}", csv_field(path), csv_field(comment));
        for (time, value) in points {
            writeln!(writer, "{prefix},{time},{value}")?;
            count += 1;
        }
    }
    writer.flush()?;
    Ok(count)
}

fn history_tools(ui: &mut Ui, state: &mut ChartPluginState) {
    if let Some(receiver) = &state.export_pending {
        match receiver.try_recv() {
            Ok(result) => {
                state.export_status = result
                    .map(|count| format!("已导出 {count} 条采样"))
                    .unwrap_or_else(|e| format!("导出失败：{e}"));
                state.export_pending = None;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                state.export_status = "导出任务异常结束".into();
                state.export_pending = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }
    ui.horizontal_wrapped(|ui| {
        if ui
            .checkbox(&mut state.interval_enabled, "A/B 区间统计")
            .changed()
        {
            state.stats_updated = Instant::now() - std::time::Duration::from_secs(1);
        }
        if state.interval_enabled {
            let mut changed = false;
            ui.label("A(s)");
            changed |= ui
                .add(egui::DragValue::new(&mut state.cursor_a).speed(0.01))
                .changed();
            ui.label("B(s)");
            changed |= ui
                .add(egui::DragValue::new(&mut state.cursor_b).speed(0.01))
                .changed();
            if changed {
                state.stats_updated = Instant::now() - std::time::Duration::from_secs(1);
            }
            ui.label(format!(
                "时间差 {:.6} s",
                (state.cursor_b - state.cursor_a).abs()
            ));
            ui.label("Shift+点击设 A；Ctrl+点击设 B");
        }
        let all = ui
            .add_enabled(
                state.export_pending.is_none(),
                egui::Button::new("导出全部历史 CSV"),
            )
            .clicked();
        let interval = ui
            .add_enabled(
                state.export_pending.is_none() && state.interval_enabled,
                egui::Button::new("导出 A/B 区间 CSV"),
            )
            .clicked();
        if all || interval {
            if let Some(path) = crate::ui::file_dialog()
                .add_filter("CSV", &["csv"])
                .set_file_name("变量历史.csv")
                .save_file()
            {
                let lo = state.cursor_a.min(state.cursor_b);
                let hi = state.cursor_a.max(state.cursor_b);
                let histories = state
                    .legends
                    .iter()
                    .map(|l| {
                        (
                            l.variable_path.clone(),
                            l.comment.clone(),
                            l.data_history
                                .iter()
                                .filter(|p| !interval || (p.x >= lo && p.x <= hi))
                                .map(|p| (p.x, p.y))
                                .collect(),
                        )
                    })
                    .collect::<HistoryExport>();
                let (sender, receiver) = std::sync::mpsc::channel();
                let ctx = ui.ctx().clone();
                state.export_pending = Some(receiver);
                state.export_status = "正在后台导出…".into();
                std::thread::spawn(move || {
                    let result = std::fs::File::create(path)
                        .and_then(|file| write_history_csv(&mut BufWriter::new(file), &histories))
                        .map_err(|e| e.to_string());
                    let _ = sender.send(result);
                    ctx.request_repaint();
                });
            }
        }
        if !state.export_status.is_empty() {
            ui.label(&state.export_status);
        }
    });
    if state.interval_enabled {
        if state.stats_updated.elapsed().as_millis() >= 250 {
            state.stats_cache = state
                .legends
                .iter()
                .map(|l| {
                    (
                        l.curve_name.clone(),
                        crate::ui::debug_tools::IntervalStats::calculate(
                            l.data_history.iter().map(|p| (p.x, p.y)),
                            state.cursor_a,
                            state.cursor_b,
                        ),
                    )
                })
                .collect();
            state.stats_updated = Instant::now();
        }
        egui::ScrollArea::both()
            .id_salt("interval_stats")
            .max_height(85.0)
            .show(ui, |ui| {
                egui::Grid::new("interval_stats_grid")
                    .striped(true)
                    .show(ui, |ui| {
                        for title in ["曲线", "有效样本", "最小值", "最大值", "均值", "峰峰值"]
                        {
                            ui.strong(title);
                        }
                        ui.end_row();
                        for (name, stats) in &state.stats_cache {
                            ui.label(name);
                            ui.label(stats.count.to_string());
                            for value in [stats.min, stats.max, stats.mean, stats.max - stats.min] {
                                ui.label(if stats.count == 0 {
                                    "—".into()
                                } else {
                                    format!("{value:.6}")
                                });
                            }
                            ui.end_row();
                        }
                    });
            });
    }
}

fn render_chart(ui: &mut Ui, state: &mut ChartPluginState) {
    if state.legend_open {
        egui::Panel::right("chart_series_list")
            .frame(egui::Frame::NONE)
            .resizable(true)
            .default_size(260.0)
            .size_range(150.0..=900.0)
            .show_inside(ui, |ui| {
                ui.strong("曲线与当前值");
                ui.label(
                    RichText::new("名称自动换行；拖动左边缘可加宽")
                        .size(10.5)
                        .color(theme::muted_text(ui)),
                );
                ui.separator();
                let mut edit_index = None;
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (i, legend) in state.legends.iter_mut().enumerate() {
                        if state.highlighted.is_some_and(|(id, when)| {
                            id == legend.variable_id && when.elapsed().as_secs_f32() < 2.0
                        }) {
                            ui.colored_label(theme::palette(ui).accent_hover, "已定位曲线");
                            if !state.highlight_scrolled {
                                ui.scroll_to_cursor(Some(egui::Align::Center));
                                state.highlight_scrolled = true;
                            }
                        }
                        // 名称完整换行显示，注释另起一行。
                        let response = ui
                            .horizontal_top(|ui| {
                                ui.checkbox(&mut legend.visible, "");
                                ui.vertical(|ui| {
                                    let response =
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&legend.curve_name)
                                                    .color(legend.color),
                                            )
                                            .wrap()
                                            .sense(egui::Sense::click()),
                                        )
                                        .on_hover_text(
                                            format!("{}\n{}", legend.variable_path, legend.comment),
                                        );
                                    if response.clicked() {
                                        legend.visible = !legend.visible;
                                    }
                                    if !legend.comment.is_empty()
                                        && legend.comment != legend.curve_name
                                    {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&legend.comment)
                                                    .size(11.0)
                                                    .color(theme::muted_text(ui)),
                                            )
                                            .wrap(),
                                        );
                                    }
                                    response
                                })
                                .inner
                            })
                            .inner;
                        response.context_menu(|ui| {
                            if ui.button("曲线属性").clicked() {
                                edit_index = Some(i);
                                ui.close();
                            }
                        });
                        let value = legend
                            .data_history
                            .back()
                            .map(|p| format!("{:.6}", p.y))
                            .unwrap_or_else(|| "--".to_owned());
                        ui.add(egui::Label::new(RichText::new(&value).monospace()).truncate())
                            .on_hover_text(value);
                        ui.separator();
                    }
                });
                if let Some(i) = edit_index {
                    let legend = &state.legends[i];
                    state.editing_legend = Some(i);
                    state.show_line_dialog = true;
                    state.edit_curve_name = legend.curve_name.clone();
                    state.edit_color = legend.color;
                    state.edit_visible = legend.visible;
                    state.edit_buffer_size = legend.buffer_size;
                }
            });
    }
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show_inside(ui, |ui| render_chart_canvas(ui, state));
}
fn render_chart_canvas(ui: &mut Ui, state: &mut ChartPluginState) {
    let available_h = ui.available_height();
    let plot_height = (available_h - 24.0).max(100.0);

    for legend in &mut state.legends {
        legend.refresh_plot_bridge();
    }

    let show_y_axis = !matches!(state.y_mode, YAxisMode::None);

    let auto_bounds: Option<(f64, f64, f64, f64)> = {
        if let Some((t_min, t_max, g_min, g_max)) = visible_history_bounds(&state.legends) {
            let xr = (t_max - t_min).max(6.0);
            let window = state.x_mode.window(xr);
            let x_min = t_max - window;
            let x_max = t_max + window * 0.02;
            let (y_min, y_max) = match &state.y_mode {
                YAxisMode::Auto => {
                    let y_pad = (g_max - g_min).max(10.0) * 0.1;
                    (g_min - y_pad, g_max + y_pad)
                }
                YAxisMode::Fixed { min, max } => (*min, *max),
                YAxisMode::None => {
                    let y_pad = (g_max - g_min).max(10.0) * 0.1;
                    (g_min - y_pad, g_max + y_pad)
                }
            };
            Some((x_min, x_max, y_min, y_max))
        } else if matches!(state.x_mode, XAxisMode::Auto) {
            Some((0.0, 6.0, 0.0, 1.0))
        } else {
            None
        }
    };

    let plot_rect = egui::Rect::from_min_size(
        ui.next_widget_position(),
        egui::vec2(ui.available_width(), plot_height),
    );

    let td_hovered = ui
        .ctx()
        .input(|i| i.pointer.hover_pos().is_some_and(|p| plot_rect.contains(p)));
    let td_scroll = ui.ctx().input(|i| i.smooth_scroll_delta);

    if td_hovered && td_scroll.y != 0.0 {
        state.auto_scroll = false;
        let factor = if td_scroll.y > 0.0 {
            1.0 / SCROLL_ZOOM_STEP
        } else {
            SCROLL_ZOOM_STEP
        };
        let current = state.td_plot_bounds;
        state.td_plot_bounds = Some(compute_td_scroll_zoom(
            current,
            factor,
            state.td_scroll_mode,
            state,
        ));
    } else {
        if let Some(bounds) = &state.td_plot_bounds {
            let range = bounds.3 - bounds.2;
            if range <= 0.0 || !range.is_finite() {
                state.td_plot_bounds = None;
            }
        }
    }

    let mut cursor_labels: Option<CursorOverlay> = None;
    let cursor_line = theme::palette(ui).border_strong.linear_multiply(0.8);

    let marker_colors = theme::palette(ui);
    let plot_response = Plot::new("chart_plot")
        .show_background(false)
        .height(plot_height)
        .show_axes([true, show_y_axis])
        .show_grid([true, true])
        .allow_zoom([true, true])
        .allow_drag([true, true])
        .allow_scroll(false)
        .allow_boxed_zoom(true)
        .allow_double_click_reset(false)
        .x_axis_formatter(|t, _range| fmt_time(t.value))
        .y_axis_formatter(|v, _range| y_axis_fmt(v.value))
        .set_margin_fraction(egui::vec2(0.02, 0.05))
        .show(ui, |plot_ui| {
            for (legend_index, legend) in state.legends.iter().enumerate() {
                if !legend.visible || legend.data_history.len() < 2 {
                    continue;
                }
                let bounds = if state.auto_scroll {
                    auto_bounds
                } else {
                    state.td_plot_bounds
                };
                let (x_min, x_max) = bounds
                    .map(|b| (b.0, b.1))
                    .unwrap_or((f64::NEG_INFINITY, f64::INFINITY));
                let points = legend.cached_plot_points(
                    x_min,
                    x_max,
                    (plot_rect.width().max(100.0) as usize) * 4,
                );
                plot_ui.line(
                    Line::new(legend.curve_name.clone(), PlotPoints::Owned(points))
                        .id(egui::Id::new(("chart_line", legend_index, 0)))
                        .color(legend.color)
                        .width(
                            if state.highlighted.is_some_and(|(id, when)| {
                                id == legend.variable_id && when.elapsed().as_secs_f32() < 2.0
                            }) {
                                3.0_f32
                            } else {
                                1.5_f32
                            },
                        ),
                );
            }

            if state.interval_enabled {
                plot_ui.vline(
                    egui_plot::VLine::new("游标 A", state.cursor_a).color(marker_colors.accent),
                );
                plot_ui.vline(
                    egui_plot::VLine::new("游标 B", state.cursor_b).color(marker_colors.warning),
                );
            }
            if let Some(cursor) = plot_ui.pointer_coordinate() {
                if state.interval_enabled && plot_ui.response().clicked() {
                    if plot_ui.ctx().input(|i| i.modifiers.shift) {
                        state.cursor_a = cursor.x;
                    }
                    if plot_ui.ctx().input(|i| i.modifiers.ctrl) {
                        state.cursor_b = cursor.x;
                    }
                }
                let t = cursor.x;
                let screen = plot_ui.screen_from_plot(cursor);
                let mut data: Vec<(String, f64, f64, Color32)> = Vec::new();
                for legend in &state.legends {
                    if !legend.visible || legend.data_history.len() < 2 {
                        continue;
                    }
                    let (dt, dv) = find_point_at(&legend.data_history, t);
                    data.push((legend.curve_name.clone(), dt, dv, legend.color));
                }
                if !data.is_empty() {
                    cursor_labels = Some((screen.x, screen.y, data));
                }
                state.cursor_txt = format!("t:{} v:{:.3}", fmt_time(t), cursor.y);
                plot_ui.vline(
                    egui_plot::VLine::new("cursor", t)
                        .color(cursor_line)
                        .width(1.0),
                );
            } else {
                state.cursor_txt.clear();
            }

            if plot_ui.response().drag_started() {
                state.auto_scroll = false;
                state.td_plot_bounds = None;
            }
            if plot_ui.response().double_clicked() {
                state.auto_scroll = true;
                state.td_plot_bounds = None;
            }

            if state.auto_scroll {
                if let Some((x_min, x_max, y_min, y_max)) = auto_bounds {
                    plot_ui
                        .set_plot_bounds(PlotBounds::from_min_max([x_min, y_min], [x_max, y_max]));
                }
            } else if !plot_ui.response().dragged() && !plot_ui.response().drag_started() {
                if let Some((x_min, x_max, y_min, y_max)) = state.td_plot_bounds {
                    plot_ui
                        .set_plot_bounds(PlotBounds::from_min_max([x_min, y_min], [x_max, y_max]));
                }
            }

            if state.show_sparse_points {
                let bounds = plot_ui.plot_bounds();
                let visible_bounds = [
                    bounds.min()[0],
                    bounds.max()[0],
                    bounds.min()[1],
                    bounds.max()[1],
                ];
                for (legend_index, legend) in state.legends.iter().enumerate() {
                    if let Some(points) =
                        sparse_visible_points(legend, visible_bounds, state.sparse_point_threshold)
                    {
                        plot_ui.points(
                            Points::new(
                                format!("{} 可见点", legend.curve_name),
                                PlotPoints::Owned(points),
                            )
                            .id(egui::Id::new(("chart_visible_points", legend_index)))
                            .color(legend.color)
                            .radius(3.0)
                            .allow_hover(false),
                        );
                    }
                }
            }
        });
    let plot_frame = *plot_response.transform.frame();
    let plot_bounds = plot_response.transform.bounds();
    state.td_plot_bounds = Some((
        plot_bounds.min()[0],
        plot_bounds.max()[0],
        plot_bounds.min()[1],
        plot_bounds.max()[1],
    ));

    if let Some((sx, sy, cursor_data)) = &cursor_labels {
        let font_id = egui::FontId::proportional(11.0);
        let mut max_w = 0.0f32;
        let mut total_h = 0.0f32;
        for (name, dt, dv, _) in cursor_data {
            let line = format!("{}: {:.3} @ {}", name, dv, fmt_time(*dt));
            let g = ui
                .painter()
                .layout_no_wrap(line, font_id.clone(), theme::palette(ui).text);
            max_w = max_w.max(g.size().x);
            total_h += g.size().y + 1.0;
        }
        let w = max_w + 8.0;
        let h = total_h + 4.0;
        let r = confined_overlay_rect(plot_frame, egui::pos2(*sx, *sy), egui::vec2(w, h));
        let painter = ui.painter().with_clip_rect(plot_frame);
        painter.rect_filled(
            r,
            egui::CornerRadius::same(3),
            theme::palette(ui).tooltip_bg,
        );
        let mut ty = r.top() + 2.0;
        for (name, dt, dv, color) in cursor_data {
            let line = format!("{}: {} @ {:.3}", name, fmt_time(*dt), dv);
            let g = painter.layout_no_wrap(line, font_id.clone(), *color);
            let gh = g.size().y;
            painter.galley(egui::pos2(r.left() + 4.0, ty), g, *color);
            ty += gh + 1.0;
        }
    }

    // Series controls live outside the plotting area.
}

fn render_fft_chart(ui: &mut Ui, state: &mut ChartPluginState) {
    let mut fft_series: Vec<FftSeries<'_>> = Vec::new();

    for legend in &mut state.legends {
        if !legend.visible || legend.data_history.len() < 4 {
            continue;
        }
        if let Some(fft) = legend.fft_cache.get(
            &legend.data_history,
            state.fft_sample_count,
            state.fft_window_type,
        ) {
            fft_series.push((
                legend.curve_name.as_str(),
                legend.color,
                std::sync::Arc::clone(&fft),
                fft.sample_rate,
            ));
        }
    }

    if fft_series.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(10.0);
            ui.label(
                RichText::new("FFT: 需要至少 4 个连续有效数据点")
                    .size(12.0)
                    .color(theme::muted_text(ui)),
            );
        });
        return;
    }

    let min_sample_rate = fft_series
        .iter()
        .map(|series| series.3)
        .fold(f64::INFINITY, f64::min);
    let max_sample_rate = fft_series
        .iter()
        .map(|series| series.3)
        .fold(f64::NEG_INFINITY, f64::max);
    let sample_rate_text = if max_sample_rate - min_sample_rate <= max_sample_rate.max(1.0) * 0.01 {
        format!(
            "采样率 ≈ {:.1} Hz",
            (min_sample_rate + max_sample_rate) * 0.5
        )
    } else {
        format!("采样率 {:.1}–{:.1} Hz", min_sample_rate, max_sample_rate)
    };

    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("📊 FFT | {sample_rate_text}"))
                .size(11.0)
                .color(theme::muted_text(ui)),
        );
        ui.add_space(8.0);
        egui::ComboBox::from_id_salt("fft_window_cfg")
            .selected_text(state.fft_window_type.label())
            .width(80.0)
            .show_ui(ui, |ui| {
                for &w in FftWindowType::ALL {
                    if ui
                        .selectable_label(state.fft_window_type == w, w.label())
                        .clicked()
                    {
                        state.fft_window_type = w;
                        state.fft_plot_bounds = None;
                    }
                }
            });
        ui.label("取样:");
        let mut sc_str = state.fft_sample_count.to_string();
        if ui
            .add_sized(
                [50.0, 18.0],
                egui::TextEdit::singleline(&mut sc_str).hint_text("1024"),
            )
            .changed()
        {
            if let Ok(v) = sc_str.parse::<usize>() {
                state.fft_sample_count = v.clamp(4, 65536);
                state.fft_plot_bounds = None;
            }
        }
        ui.separator();
        ui.label("缩放:");
        for &mode in &[FftScrollMode::X, FftScrollMode::Y, FftScrollMode::Both] {
            if zoom_mode_button(ui, state.fft_scroll_mode == mode, mode).clicked() {
                state.fft_scroll_mode = mode;
            }
        }
    });

    let available_h = ui.available_height();
    let plot_height = (available_h - 4.0).max(60.0);

    let plot_pos = ui.next_widget_position();
    let plot_rect =
        egui::Rect::from_min_size(plot_pos, egui::vec2(ui.available_width(), plot_height));

    let hovered = ui
        .ctx()
        .input(|i| i.pointer.hover_pos().is_some_and(|p| plot_rect.contains(p)));
    let scroll_delta = ui.ctx().input(|i| i.smooth_scroll_delta);

    if hovered && scroll_delta.y != 0.0 {
        let factor = if scroll_delta.y > 0.0 {
            1.0 / SCROLL_ZOOM_STEP
        } else {
            SCROLL_ZOOM_STEP
        };
        let current_bounds = state.fft_plot_bounds;
        let new_bounds =
            compute_scroll_zoom(current_bounds, factor, state.fft_scroll_mode, &fft_series);
        state.fft_plot_bounds = Some(new_bounds);
    } else {
        if let Some(bounds) = &state.fft_plot_bounds {
            let range = bounds.3 - bounds.2;
            if range <= 0.0 || !range.is_finite() {
                state.fft_plot_bounds = None;
            }
        }
    }

    let mut cursor_labels: Option<CursorOverlay> = None;
    let cursor_line = theme::palette(ui).border_strong.linear_multiply(0.8);

    let plot_response = Plot::new("fft_plot")
        .show_background(false)
        .height(plot_height)
        .show_axes([true, true])
        .show_grid([true, true])
        .allow_zoom([true, true])
        .allow_drag([true, true])
        .allow_scroll(false)
        .allow_boxed_zoom(true)
        .x_axis_formatter(|f, _range| format!("{:.0} Hz", f.value))
        .y_axis_formatter(|v, _range| {
            if v.value.abs() < 0.001 && v.value != 0.0 {
                format!("{:.5}", v.value)
            } else {
                format!("{:.3}", v.value)
            }
        })
        .set_margin_fraction(egui::vec2(0.02, 0.05))
        .show(ui, |plot_ui| {
            if let Some((x_min, x_max, y_min, y_max)) = state.fft_plot_bounds {
                plot_ui.set_plot_bounds(PlotBounds::from_min_max([x_min, y_min], [x_max, y_max]));
            }

            for (name, color, fft, _sr) in &fft_series {
                plot_ui.line(
                    Line::new(*name, PlotPoints::Borrowed(&fft.points))
                        .color(*color)
                        .width(1.2),
                );
            }

            if let Some(cursor) = plot_ui.pointer_coordinate() {
                let freq = cursor.x;
                let screen = plot_ui.screen_from_plot(cursor);
                let mut data: Vec<(String, f64, f64, Color32)> = Vec::new();
                for (name, color, fft, _sr) in &fft_series {
                    let mag = nearest_mag(&fft.points, freq);
                    data.push((name.to_string(), freq, mag, *color));
                }
                if !data.is_empty() {
                    cursor_labels = Some((screen.x, screen.y, data));
                }
                state.cursor_txt = format!("FFT: {:.1} Hz | {:.3}", freq, cursor.y);
                plot_ui.vline(
                    egui_plot::VLine::new("fft_cursor", freq)
                        .color(cursor_line)
                        .width(1.0),
                );
            }
        });
    let plot_frame = *plot_response.transform.frame();
    let plot_bounds = plot_response.transform.bounds();
    state.fft_plot_bounds = Some((
        plot_bounds.min()[0],
        plot_bounds.max()[0],
        plot_bounds.min()[1],
        plot_bounds.max()[1],
    ));

    if let Some((sx, sy, cursor_data)) = &cursor_labels {
        let font_id = egui::FontId::proportional(11.0);
        let mut max_w = 0.0f32;
        let mut total_h = 0.0f32;
        for (name, freq, mag, _) in cursor_data {
            let line = format!("{}: {:.1} Hz → {:.3}", name, freq, mag);
            let g = ui
                .painter()
                .layout_no_wrap(line, font_id.clone(), theme::palette(ui).text);
            max_w = max_w.max(g.size().x);
            total_h += g.size().y + 1.0;
        }
        let w = max_w + 8.0;
        let h = total_h + 4.0;
        let r = confined_overlay_rect(plot_frame, egui::pos2(*sx, *sy), egui::vec2(w, h));
        let painter = ui.painter().with_clip_rect(plot_frame);
        painter.rect_filled(
            r,
            egui::CornerRadius::same(3),
            theme::palette(ui).tooltip_bg,
        );
        let mut ty = r.top() + 2.0;
        for (name, freq, mag, color) in cursor_data {
            let line = format!("{}: {:.1} Hz → {:.3}", name, freq, mag);
            let g = painter.layout_no_wrap(line, font_id.clone(), *color);
            let gh = g.size().y;
            painter.galley(egui::pos2(r.left() + 4.0, ty), g, *color);
            ty += gh + 1.0;
        }
    }
}

fn confined_overlay_rect(
    plot_rect: egui::Rect,
    anchor: egui::Pos2,
    desired_size: egui::Vec2,
) -> egui::Rect {
    let size = egui::vec2(
        desired_size.x.min(plot_rect.width()).max(0.0),
        desired_size.y.min(plot_rect.height()).max(0.0),
    );
    let mut x = anchor.x + 16.0;
    if x + size.x > plot_rect.right() {
        x = anchor.x - 16.0 - size.x;
    }
    let mut y = anchor.y + 8.0;
    if y + size.y > plot_rect.bottom() {
        y = anchor.y - 8.0 - size.y;
    }
    let max_x = plot_rect.right() - size.x;
    let max_y = plot_rect.bottom() - size.y;
    egui::Rect::from_min_size(
        egui::pos2(
            x.clamp(plot_rect.left(), max_x),
            y.clamp(plot_rect.top(), max_y),
        ),
        size,
    )
}

fn zoom_mode_button(ui: &mut Ui, selected: bool, mode: FftScrollMode) -> egui::Response {
    ui.add_sized(
        ZOOM_MODE_BUTTON_SIZE,
        egui::Button::new(RichText::new(mode.label()).size(12.0))
            .selected(selected)
            .frame(true)
            .frame_when_inactive(true),
    )
}

fn fft_toggle_button(ui: &mut Ui, enabled: bool) -> egui::Response {
    let label = if enabled { "📊 FFT 关" } else { "📊 FFT" };
    ui.add_sized(
        FFT_TOGGLE_BUTTON_SIZE,
        egui::Button::new(RichText::new(label).size(12.0))
            .selected(enabled)
            .frame(true)
            .frame_when_inactive(true),
    )
}

fn compute_scroll_zoom(
    current: Option<(f64, f64, f64, f64)>,
    factor: f64,
    mode: FftScrollMode,
    fft_series: &[FftSeries<'_>],
) -> (f64, f64, f64, f64) {
    let (x_min, x_max, y_min, y_max) = current.unwrap_or_else(|| {
        let x_min = fft_series
            .iter()
            .filter_map(|s| s.2.points.first().map(|point| point.x))
            .fold(f64::MAX, f64::min);
        let x_max = fft_series
            .iter()
            .filter_map(|s| s.2.points.last().map(|point| point.x))
            .fold(0.0, f64::max);
        let y_min = fft_series
            .iter()
            .flat_map(|s| s.2.points.iter().map(|point| point.y))
            .fold(f64::MAX, f64::min);
        let y_max = fft_series
            .iter()
            .flat_map(|s| s.2.points.iter().map(|point| point.y))
            .fold(f64::NEG_INFINITY, f64::max);
        let y_pad = ((y_max - y_min).max(0.001) * 0.1).max(0.001);
        (x_min, x_max, y_min - y_pad, y_max + y_pad)
    });

    match mode {
        FftScrollMode::Both => {
            let cx = (x_min + x_max) / 2.0;
            let hx = (x_max - x_min) / 2.0 * factor;
            let cy = (y_min + y_max) / 2.0;
            let hy = (y_max - y_min) / 2.0 * factor;
            (cx - hx, cx + hx, cy - hy, cy + hy)
        }
        FftScrollMode::X => {
            let cx = (x_min + x_max) / 2.0;
            let hx = (x_max - x_min) / 2.0 * factor;
            (cx - hx, cx + hx, y_min, y_max)
        }
        FftScrollMode::Y => {
            let cy = (y_min + y_max) / 2.0;
            let hy = (y_max - y_min) / 2.0 * factor;
            (x_min, x_max, cy - hy, cy + hy)
        }
    }
}

/// Reduce only the drawn copy, keeping first/last and each bucket's extrema.
/// The acquisition history, cursor lookup, CSV and FFT retain the original samples.
pub(super) fn history_plot_points(
    data: &std::collections::VecDeque<PlotPoint>,
    x_min: f64,
    x_max: f64,
    budget: usize,
) -> Vec<PlotPoint> {
    let start = data.partition_point(|p| p.x < x_min).saturating_sub(1);
    let end = (data.partition_point(|p| p.x <= x_max) + 1).min(data.len());
    if start >= end {
        return Vec::new();
    }
    let count = end - start;
    if count <= budget.max(4) {
        return data.range(start..end).copied().collect();
    }
    let buckets = (budget.max(4) - 2) / 2;
    let chunk = (count - 2).div_ceil(buckets);
    let mut points = Vec::with_capacity(budget.max(4));
    points.push(data[start]);
    for from in (start + 1..end - 1).step_by(chunk) {
        let to = (from + chunk).min(end - 1);
        let mut min = from;
        let mut max = from;
        for index in from + 1..to {
            if data[index].y < data[min].y {
                min = index;
            }
            if data[index].y > data[max].y {
                max = index;
            }
        }
        points.push(data[min.min(max)]);
        if min != max {
            points.push(data[min.max(max)]);
        }
    }
    points.push(data[end - 1]);
    points
}

fn visible_history_bounds(legends: &[ChartLegend]) -> Option<(f64, f64, f64, f64)> {
    let mut bounds: Option<(f64, f64, f64, f64)> = None;
    for legend in legends
        .iter()
        .filter(|l| l.visible && l.data_history.len() >= 2)
    {
        if let Some((a, b, c, d)) = legend.cached_bounds() {
            bounds = Some(match bounds {
                Some((x0, x1, y0, y1)) => (x0.min(a), x1.max(b), y0.min(c), y1.max(d)),
                None => (a, b, c, d),
            });
        }
    }
    bounds
}

/// Return points that are actually inside the current plot viewport, but only
/// while their count is strictly below the configured threshold. The scan uses
/// timestamp partitioning and exits as soon as the threshold is reached.
fn sparse_visible_points(
    legend: &ChartLegend,
    [x_min, x_max, y_min, y_max]: [f64; 4],
    threshold: usize,
) -> Option<Vec<PlotPoint>> {
    if !legend.visible || threshold < MIN_POINT_MARKER_THRESHOLD {
        return None;
    }
    let mut points = Vec::with_capacity(threshold.min(legend.data_history.len()));
    let (first, second) = legend.data_history.as_slices();
    for slice in [first, second] {
        let start = slice.partition_point(|point| point.x < x_min);
        let end = slice.partition_point(|point| point.x <= x_max);
        for point in &slice[start..end] {
            if point.x.is_finite() && point.y.is_finite() && (y_min..=y_max).contains(&point.y) {
                points.push(*point);
                if points.len() >= threshold {
                    return None;
                }
            }
        }
    }
    (!points.is_empty()).then_some(points)
}

fn compute_td_scroll_zoom(
    current: Option<(f64, f64, f64, f64)>,
    factor: f64,
    mode: FftScrollMode,
    state: &ChartPluginState,
) -> (f64, f64, f64, f64) {
    let (x_min, x_max, y_min, y_max) = current.unwrap_or_else(|| {
        let (x_min, x_max, y_min, y_max) =
            visible_history_bounds(&state.legends).unwrap_or((0.0, 6.0, 0.0, 1.0));
        let y_pad = ((y_max - y_min).max(10.0) * 0.1).max(0.001);
        (x_min, x_max, y_min - y_pad, y_max + y_pad)
    });

    match mode {
        FftScrollMode::Both => {
            let cx = (x_min + x_max) / 2.0;
            let hx = (x_max - x_min) / 2.0 * factor;
            let cy = (y_min + y_max) / 2.0;
            let hy = (y_max - y_min) / 2.0 * factor;
            (cx - hx, cx + hx, cy - hy, cy + hy)
        }
        FftScrollMode::X => {
            let cx = (x_min + x_max) / 2.0;
            let hx = (x_max - x_min) / 2.0 * factor;
            (cx - hx, cx + hx, y_min, y_max)
        }
        FftScrollMode::Y => {
            let cy = (y_min + y_max) / 2.0;
            let hy = (y_max - y_min) / 2.0 * factor;
            (x_min, x_max, cy - hy, cy + hy)
        }
    }
}

fn nearest_mag(points: &[egui_plot::PlotPoint], target: f64) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    let idx = points.partition_point(|point| point.x < target);
    if idx == 0 {
        return points[0].y;
    }
    if idx >= points.len() {
        return points[points.len() - 1].y;
    }
    let left = points[idx - 1].x;
    let right = points[idx].x;
    if (target - left).abs() <= (right - target).abs() {
        points[idx - 1].y
    } else {
        points[idx].y
    }
}

#[cfg(test)]
fn right_aligned_legend_rect(
    plot_rect: egui::Rect,
    y: f32,
    desired_size: egui::Vec2,
) -> egui::Rect {
    let right = plot_rect.right() - 5.0;
    let available_width = (right - plot_rect.left() - 5.0).max(0.0);
    let width = desired_size.x.min(available_width);
    egui::Rect::from_min_size(
        egui::pos2(right - width, y),
        egui::vec2(width, desired_size.y),
    )
}

fn decode_value_f64(data: &[u8], ext_type: &crate::dwarf::types::ExtendType) -> f64 {
    use crate::dwarf::types::ExtendType::*;
    if data.is_empty() {
        return 0.0;
    }
    match ext_type {
        U8 => *data.first().unwrap_or(&0) as f64,
        I8 => *data.first().unwrap_or(&0) as i8 as f64,
        U16 => {
            if data.len() >= 2 {
                u16::from_le_bytes([data[0], data[1]]) as f64
            } else {
                0.0
            }
        }
        I16 => {
            if data.len() >= 2 {
                i16::from_le_bytes([data[0], data[1]]) as f64
            } else {
                0.0
            }
        }
        U32 => {
            if data.len() >= 4 {
                u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as f64
            } else {
                0.0
            }
        }
        I32 => {
            if data.len() >= 4 {
                i32::from_le_bytes([data[0], data[1], data[2], data[3]]) as f64
            } else {
                0.0
            }
        }
        U64 => {
            if data.len() >= 8 {
                u64::from_le_bytes([
                    data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
                ]) as f64
            } else {
                0.0
            }
        }
        I64 => {
            if data.len() >= 8 {
                i64::from_le_bytes([
                    data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
                ]) as f64
            } else {
                0.0
            }
        }
        Float => {
            if data.len() >= 4 {
                f32::from_le_bytes([data[0], data[1], data[2], data[3]]) as f64
            } else {
                0.0
            }
        }
        Double => {
            if data.len() >= 8 {
                f64::from_le_bytes([
                    data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
                ])
            } else {
                0.0
            }
        }
        Other => 0.0,
    }
}

fn find_point_at(data: &std::collections::VecDeque<egui_plot::PlotPoint>, t: f64) -> (f64, f64) {
    if data.is_empty() {
        return (t, 0.0);
    }
    let idx = data.partition_point(|point| point.x < t);
    if idx == 0 {
        let point = data[0];
        return (point.x, point.y);
    }
    if idx >= data.len() {
        let point = data[data.len() - 1];
        return (point.x, point.y);
    }
    let p0 = data[idx - 1];
    let p1 = data[idx];
    let point = if (t - p0.x).abs() < (p1.x - t).abs() {
        p0
    } else {
        p1
    };
    (point.x, point.y)
}

fn y_axis_fmt(v: f64) -> String {
    if v.abs() < 0.01 && v != 0.0 {
        format!("{v:.5}")
    } else {
        format!("{v:.3}")
    }
}

fn fmt_time(t: f64) -> String {
    let s = format!("{:.6}", t);
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() {
        "0s".to_string()
    } else {
        format!("{}s", trimmed)
    }
}

fn y_mode_label(mode: &YAxisMode) -> String {
    match mode {
        YAxisMode::Auto => "自动".to_string(),
        YAxisMode::Fixed { .. } => "固定".to_string(),
        YAxisMode::None => "无".to_string(),
    }
}

fn x_mode_label(mode: &XAxisMode) -> String {
    match mode {
        XAxisMode::Auto => "完整历史".to_string(),
        XAxisMode::Fixed(w) => format!("最近 {w:.3}s"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use eframe::egui;

    use crate::dwarf::types::{ExtendConfig, ExtendType};
    use crate::model::VariablePool;
    use crate::ui::plugin::MemRWPlugin;

    use super::{
        ChartLegend, ChartPluginState, FFT_TOGGLE_BUTTON_SIZE, FftScrollMode,
        ZOOM_MODE_BUTTON_SIZE, confined_overlay_rect, fft_toggle_button, right_aligned_legend_rect,
        sparse_visible_points, update_chart_data, visible_history_bounds, write_log_frame,
        zoom_mode_button,
    };

    fn add_u8(pool: &mut VariablePool, name: &str, address: u64) -> usize {
        pool.add(&ExtendConfig {
            name: name.to_owned(),
            address,
            ext_type: ExtendType::U8,
            size: 1,
            array_index: None,
            array_count: None,
        })
    }

    #[test]
    fn long_capture_drawing_preserves_endpoints_and_spikes() {
        let data = (0..100_000)
            .map(|i| {
                egui_plot::PlotPoint::new(
                    i as f64,
                    if i == 12_345 {
                        999.0
                    } else if i == 54_321 {
                        -777.0
                    } else {
                        0.0
                    },
                )
            })
            .collect();
        let points = super::history_plot_points(&data, 0.0, 99_999.0, 800);
        assert!(points.len() <= 800);
        assert_eq!(points.first().unwrap().x, 0.0);
        assert_eq!(points.last().unwrap().x, 99_999.0);
        assert!(points.iter().any(|p| p.y == 999.0));
        assert!(points.iter().any(|p| p.y == -777.0));
        assert!(points.windows(2).all(|p| p[0].x < p[1].x));
        assert_eq!(data.len(), 100_000);
        let zoom = super::history_plot_points(&data, 100.0, 110.0, 800);
        assert_eq!(zoom.len(), 13);
    }

    #[test]
    fn old_chart_settings_default_to_complete_history() {
        let old = serde_json::json!({"variable_name":"test", "variable_address":4096,
            "curve_name":"test", "color":[0,0,0,255], "visible":true,"buffer_size":5000});
        let saved: super::SavedChartLegend = serde_json::from_value(old).unwrap();
        assert!(saved.retain_all);
    }

    #[test]
    fn log_writer_merges_fifo_batches_without_timestamp_scratch() {
        let mut pool = VariablePool::default();
        let first_id = add_u8(&mut pool, "first", 0x1000);
        let second_id = add_u8(&mut pool, "second", 0x1001);
        let legends = vec![
            ChartLegend::new(first_id, "first".to_owned()),
            ChartLegend::new(second_id, "second".to_owned()),
        ];
        let mut frame_data = HashMap::new();
        frame_data.insert(first_id, vec![(1.0, [1; 8]), (2.0, [2; 8])]);
        frame_data.insert(second_id, vec![(1.0, [10; 8]), (3.0, [30; 8])]);

        let mut output = Vec::new();
        let mut positions = Vec::new();
        write_log_frame(&mut output, &mut positions, &legends, &pool, &frame_data);

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "1.000000,1.000000,10.000000\n\
             2.000000,2.000000,\n\
             3.000000,,30.000000\n"
        );
    }

    #[test]
    fn hidden_curves_do_not_affect_view_bounds() {
        let mut visible = ChartLegend::new(0, "visible".to_owned());
        visible.push_prepared(1.0, -2.0);
        visible.push_prepared(3.0, 4.0);

        let mut hidden = ChartLegend::new(1, "hidden".to_owned());
        hidden.push_prepared(-100.0, -1000.0);
        hidden.push_prepared(100.0, 1000.0);
        hidden.visible = false;

        assert_eq!(
            visible_history_bounds(&[visible, hidden]),
            Some((1.0, 3.0, -2.0, 4.0))
        );
    }

    #[test]
    fn point_markers_use_only_points_inside_the_current_plot_view() {
        let mut legend = ChartLegend::new(0, "visible".to_owned());
        for value in 0..10 {
            legend.push_prepared(value as f64, value as f64);
        }

        let points = sparse_visible_points(&legend, [2.0, 7.0, 3.0, 6.0], 5).unwrap();
        assert_eq!(
            points
                .iter()
                .map(|point| (point.x, point.y))
                .collect::<Vec<_>>(),
            vec![(3.0, 3.0), (4.0, 4.0), (5.0, 5.0), (6.0, 6.0)]
        );
        assert!(sparse_visible_points(&legend, [2.0, 7.0, 3.0, 6.0], 4).is_none());

        legend.visible = false;
        assert!(sparse_visible_points(&legend, [0.0, 9.0, 0.0, 9.0], 20).is_none());
    }

    #[test]
    fn sparse_point_settings_are_saved_and_legacy_configs_still_load() {
        let current = serde_json::json!({
            "legends": [],
            "show_sparse_points": true,
            "sparse_point_threshold": 17
        });
        match serde_json::from_value::<super::SavedChartPayload>(current).unwrap() {
            super::SavedChartPayload::Current(config) => {
                assert!(config.show_sparse_points);
                assert_eq!(config.sparse_point_threshold, 17);
            }
            super::SavedChartPayload::Legacy(_) => panic!("expected current chart config"),
        }

        match serde_json::from_value::<super::SavedChartPayload>(serde_json::json!([])).unwrap() {
            super::SavedChartPayload::Legacy(legends) => assert!(legends.is_empty()),
            super::SavedChartPayload::Current(_) => panic!("expected legacy chart config"),
        }
    }

    #[test]
    fn ingests_samples_without_rendering_the_chart() {
        let mut pool = VariablePool::default();
        let variable_id = add_u8(&mut pool, "value", 0x2000_0000);
        let mut state = ChartPluginState::default();
        state.add_legend(variable_id, &pool, "value".to_owned(), egui::Color32::WHITE);
        let mut frame_data = HashMap::new();
        frame_data.insert(variable_id, vec![(0.0, [1; 8]), (0.1, [2; 8])]);

        update_chart_data(&mut state, &pool, &frame_data, true, true);

        assert_eq!(state.legends[0].data_history.len(), 2);
        assert_eq!(state.legends[0].data_history.back().unwrap().y, 2.0);
    }

    #[test]
    fn debug_halt_keeps_the_acquisition_session_open() {
        let pool = VariablePool::default();
        let frame_data = std::collections::HashMap::new();
        let mut state = ChartPluginState::default();

        update_chart_data(&mut state, &pool, &frame_data, true, true);
        assert!(state.was_running);

        update_chart_data(&mut state, &pool, &frame_data, false, true);
        assert!(state.was_running);

        update_chart_data(&mut state, &pool, &frame_data, false, false);
        assert!(!state.was_running);
    }

    #[test]
    fn resetting_data_starts_with_an_empty_time_epoch() {
        let mut pool = VariablePool::default();
        let variable_id = add_u8(&mut pool, "value", 0x2000_0000);
        let mut state = ChartPluginState::default();
        state.add_legend(variable_id, &pool, "value".to_owned(), egui::Color32::WHITE);
        state.legends[0].push_prepared(10.0, 1.0);
        state.was_running = true;

        state.reset_data();

        assert!(state.legends[0].data_history.is_empty());
        assert!(!state.was_running);
        assert!(state.auto_scroll);
    }

    #[test]
    fn zoom_buttons_keep_the_same_allocated_size_in_every_state() {
        egui::__run_test_ui(|ui| {
            let inactive = zoom_mode_button(ui, false, FftScrollMode::X);
            let active = zoom_mode_button(ui, true, FftScrollMode::Both);

            assert_eq!(inactive.rect.size(), active.rect.size());
            assert_eq!(
                inactive.rect.size(),
                egui::Vec2::from(ZOOM_MODE_BUTTON_SIZE)
            );
        });
    }

    #[test]
    fn fft_toggle_keeps_the_same_allocated_size_in_every_state() {
        egui::__run_test_ui(|ui| {
            let inactive = fft_toggle_button(ui, false);
            let active = fft_toggle_button(ui, true);

            assert_eq!(inactive.rect.size(), active.rect.size());
            assert_eq!(
                inactive.rect.size(),
                egui::Vec2::from(FFT_TOGGLE_BUTTON_SIZE)
            );
        });
    }

    #[test]
    fn cursor_overlays_are_confined_to_their_plot_rect() {
        let plot = egui::Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(210.0, 140.0));
        for anchor in [
            plot.left_top(),
            plot.right_top(),
            plot.left_bottom(),
            plot.right_bottom(),
            plot.center(),
        ] {
            let overlay = confined_overlay_rect(plot, anchor, egui::vec2(90.0, 50.0));
            assert!(plot.contains_rect(overlay));
        }

        let oversized = confined_overlay_rect(plot, plot.center(), egui::vec2(500.0, 300.0));
        assert_eq!(oversized, plot);
    }

    #[test]
    fn legend_rows_with_different_widths_share_the_plot_right_edge() {
        let plot = egui::Rect::from_min_max(egui::pos2(50.0, 20.0), egui::pos2(450.0, 300.0));
        let short = right_aligned_legend_rect(plot, 30.0, egui::vec2(80.0, 20.0));
        let long = right_aligned_legend_rect(plot, 55.0, egui::vec2(220.0, 20.0));

        assert_eq!(short.right(), plot.right() - 5.0);
        assert_eq!(long.right(), plot.right() - 5.0);
        assert!(short.left() > long.left());

        let oversized = right_aligned_legend_rect(plot, 80.0, egui::vec2(800.0, 20.0));
        assert_eq!(oversized.left(), plot.left() + 5.0);
        assert_eq!(oversized.right(), plot.right() - 5.0);
    }
}

#[cfg(test)]
mod enhanced_chart_interaction_tests {
    use super::*;

    #[test]
    fn synthetic_chart_frames_keep_manual_view_and_raw_history() {
        let mut state = ChartPluginState::default();
        for index in 0..3 {
            let mut legend = ChartLegend::new(index, format!("long_curve_name_{index}"));
            for point in 0..120_000 {
                legend.push_prepared(
                    point as f64 * 0.001,
                    if point == 60_001 {
                        999.0
                    } else {
                        (point as f64 * 0.01).sin()
                    },
                );
            }
            state.legends.push(legend);
        }
        let ctx = egui::Context::default();
        let started = std::time::Instant::now();
        for frame in 0..30 {
            if frame == 5 {
                state.auto_scroll = false;
                state.td_plot_bounds = Some((20.0, 25.0, -2.0, 2.0));
            }
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 650.0),
                    )),
                    time: Some(frame as f64 / 60.0),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        render_chart(ui, &mut state);
                    });
                },
            );
            if frame >= 5 {
                assert!(!state.auto_scroll);
                let bounds = state.td_plot_bounds.unwrap();
                assert!((bounds.0 - 20.0).abs() < 0.01 && (bounds.1 - 25.0).abs() < 0.01);
            }
        }
        assert!(
            state
                .legends
                .iter()
                .all(|l| l.data_history.len() == 120_000)
        );
        eprintln!(
            "headless chart: 30 frames, 3 x 120000 points, {:?}",
            started.elapsed()
        );
    }
}

#[cfg(test)]
mod readonly_history_tests {
    use super::*;
    #[test]
    fn history_is_uncompressed_bounded_and_keeps_raw_samples() {
        let mut chart = ChartPluginState::default();
        let mut legend = ChartLegend::new(3, "speed".into());
        for i in 0..100 {
            legend.push_prepared(i as f64, i as f64);
        }
        chart.legends.push(legend);
        let history = chart.readonly_history(3, 10.0, 5).unwrap();
        assert_eq!(history["points"].as_array().unwrap().len(), 5);
        assert_eq!(history["points"][0]["time_s"], 95.0);
        assert_eq!(history["truncated"], true);
        assert_eq!(history["display_compressed"], false);
        assert_eq!(chart.legends[0].data_history.len(), 100);
        assert!(chart.readonly_history(4, 10.0, 5).is_none());
    }
}

#[cfg(test)]
mod workspace_feature_tests {
    use super::*;
    use crate::dwarf::types::ExtendConfig;

    fn pool_and_id() -> (VariablePool, usize) {
        let mut pool = VariablePool::default();
        let id = pool.add(&ExtendConfig {
            name: "motor.kp".into(),
            address: 0x20000000,
            ext_type: ExtendType::Float,
            size: 4,
            array_index: None,
            array_count: None,
        });
        pool.bind(id, true, VariableReadClass::Latest);
        (pool, id)
    }

    #[test]
    fn watch_to_chart_deduplicates_and_preserves_latest_binding() {
        let (mut pool, id) = pool_and_id();
        let mut chart = ChartPluginState::default();
        let variables = vec![(id, "kp".into(), "比例增益".into())];
        assert!(chart.add_chart_variables(&variables, &mut pool).unwrap());
        assert!(!chart.add_chart_variables(&variables, &mut pool).unwrap());
        assert_eq!(chart.legends.len(), 1);
        assert_eq!(pool.get(id).unwrap().stream_readers, 1);
        assert_eq!(pool.get(id).unwrap().latest_readers, 1);
        let saved = chart.save_config(&pool);
        let mut restored_pool = VariablePool::default();
        restored_pool.add(&ExtendConfig {
            name: "motor.kp".into(),
            address: 0x20000000,
            ext_type: ExtendType::Float,
            size: 4,
            array_index: None,
            array_count: None,
        });
        let mut restored = ChartPluginState::default();
        restored.load_config(&saved, &mut restored_pool).unwrap();
        assert_eq!(restored.legends[0].variable_path, "motor.kp");
        assert_eq!(restored.legends[0].comment, "比例增益");
        chart.remove_legend(0);
        pool.unbind(id, true, VariableReadClass::Stream);
        assert_eq!(pool.get(id).unwrap().latest_readers, 1);
        assert_eq!(pool.get(id).unwrap().stream_readers, 0);
    }

    #[test]
    fn invalid_batch_does_not_partially_bind() {
        let (mut pool, id) = pool_and_id();
        let mut chart = ChartPluginState::default();
        assert!(
            chart
                .add_chart_variables(
                    &[
                        (id, "kp".into(), String::new()),
                        (999, "invalid".into(), String::new())
                    ],
                    &mut pool
                )
                .is_err()
        );
        assert!(chart.legends.is_empty());
        assert_eq!(pool.get(id).unwrap().stream_readers, 0);
    }

    #[test]
    fn history_csv_keeps_distinct_sampling_times_and_escaped_comments() {
        let histories = vec![
            (
                "motor.kp".into(),
                "中文,\"说明\"".into(),
                vec![(0.25, 1.0), (0.75, 2.0)],
            ),
            ("motor.speed".into(), String::new(), vec![(0.5, 3.0)]),
        ];
        let mut output = Vec::new();
        assert_eq!(write_history_csv(&mut output, &histories).unwrap(), 3);
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("\"motor.kp\",\"中文,\"\"说明\"\"\",0.25,1"));
        assert!(text.contains("\"motor.speed\",\"\",0.5,3"));
        assert_eq!(text.lines().count(), 4);
    }
}
