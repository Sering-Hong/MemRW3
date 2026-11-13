use crate::ui::debug_tools::{NumberFormat, format_number};
use std::fmt::Write as _;
use std::path::PathBuf;

use eframe::egui::{self, RichText, Ui};
use serde::{Deserialize, Serialize};

use super::svd_panel::{SavedSvdRegister, SvdPanelState, render_svd_panel};
use super::tree::{CheckState, SavedTableLeaf, SavedTableNode, TableNode};
use crate::dwarf::types::ExtendType;
use crate::model::VariablePool;
use crate::model::VariableReadClass;
use crate::ui::plugin::{
    MemRWPlugin, PluginAction, PluginRenderContext, PluginUpdateContext, ToastLevel,
    VariableCandidate, temp_text_value,
};
use crate::ui::theme;

pub struct TablePluginState {
    roots: Vec<TableNode>,
    next_node_id: u64,
    pending_removals: Vec<(usize, bool)>,
    pending_enabled: Vec<(usize, bool)>,
    pending_writes: Vec<(usize, u64)>,
    pending_chart: Vec<(usize, String, String)>,
    pending_svd_actions: Vec<PluginAction>,
    errors: Vec<String>,
    svd: SvdPanelState,
    filter: String,
    number_format: NumberFormat,
    connected: bool,
    show_svd: bool,
    main_widths: Vec<f32>,
    popout_widths: Vec<f32>,
    hidden: [bool; COLUMN_COUNT],
    wrap_text: bool,
    pending_autofit: bool,
}

impl Default for TablePluginState {
    fn default() -> Self {
        let prefs = crate::ui::prefs::get();
        Self {
            hidden: hidden_from_keys(prefs.table_hidden_columns.as_deref().unwrap_or(&[])),
            wrap_text: prefs.table_wrap_text.unwrap_or(true),
            pending_autofit: false,
            roots: Vec::new(),
            next_node_id: 1,
            pending_removals: Vec::new(),
            pending_enabled: Vec::new(),
            pending_writes: Vec::new(),
            pending_chart: Vec::new(),
            pending_svd_actions: Vec::new(),
            errors: Vec::new(),
            svd: SvdPanelState::default(),
            filter: String::new(),
            number_format: NumberFormat::default(),
            connected: false,
            show_svd: false,
            main_widths: default_column_widths(),
            popout_widths: default_column_widths(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct SavedTableConfig {
    roots: Vec<SavedTableNode>,
    #[serde(default)]
    show_svd: bool,
    #[serde(default)]
    svd_path: Option<String>,
    #[serde(default)]
    svd_registers: Vec<SavedSvdRegister>,
    #[serde(default)]
    main_widths: Option<Vec<f32>>,
    #[serde(default)]
    popout_widths: Option<Vec<f32>>,
    #[serde(default)]
    hidden_columns: Option<Vec<String>>,
    #[serde(default)]
    wrap_text: Option<bool>,
}

#[derive(Deserialize)]
struct LegacyTableEntry {
    variable_name: String,
    variable_address: u64,
    display_name: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SavedTablePayload {
    Current(SavedTableConfig),
    Legacy(Vec<LegacyTableEntry>),
}

impl MemRWPlugin for TablePluginState {
    fn id(&self) -> &'static str {
        "table"
    }

    fn title(&self) -> &'static str {
        "变量"
    }

    fn viewport_size(&self) -> egui::Vec2 {
        egui::vec2(1080.0, 620.0)
    }

    fn min_viewport_size(&self) -> egui::Vec2 {
        egui::vec2(720.0, 420.0)
    }

    fn supports_composite_variables(&self) -> bool {
        true
    }

    fn update(&mut self, ctx: PluginUpdateContext<'_>) -> Vec<PluginAction> {
        self.connected = ctx.connected;
        for root in &mut self.roots {
            root.update_values(ctx.pool, ctx.frame_data, format_value_into);
        }
        self.svd.update(
            ctx.register_data,
            ctx.connected,
            ctx.hardware_busy,
            ctx.egui_ctx,
        )
    }

    fn reset_data(&mut self) {
        for root in &mut self.roots {
            root.reset_values();
        }
        self.svd.reset_values();
    }

    fn render(&mut self, ui: &mut Ui, ctx: PluginRenderContext<'_>) -> Vec<PluginAction> {
        let mut actions = Vec::new();
        if table_panel(ui, self, ctx.pool) {
            actions.push(PluginAction::OpenVariableTree {
                plugin_id: self.id().to_owned(),
                viewport_id: ctx.viewport_id,
            });
        }
        if !self.pending_chart.is_empty() {
            actions.push(PluginAction::AddToChart {
                variables: std::mem::take(&mut self.pending_chart),
            });
        }
        actions.extend(self.pending_enabled.drain(..).map(|(var_id, enabled)| {
            PluginAction::SetVariableEnabled {
                var_id,
                enabled,
                read_class: VariableReadClass::Latest,
            }
        }));
        actions.extend(
            self.pending_removals
                .drain(..)
                .map(|(var_id, was_enabled)| PluginAction::RemoveVariable {
                    var_id,
                    was_enabled,
                    read_class: VariableReadClass::Latest,
                }),
        );
        actions.extend(
            self.pending_writes
                .drain(..)
                .map(|(var_id, value)| PluginAction::WriteVariable { var_id, value }),
        );
        actions.extend(self.errors.drain(..).map(|message| PluginAction::Toast {
            level: ToastLevel::Error,
            message,
        }));
        actions.append(&mut self.pending_svd_actions);
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
        let name_id = ui.make_persistent_id(format!("table_add_name_{node_id}"));
        let name_default_id = ui.make_persistent_id(format!("table_add_name_default_{node_id}"));
        let mut display_name = temp_text_value(ui, name_id, name_default_id, default_name);

        ui.horizontal(|ui| {
            ui.label("显示名:");
            ui.text_edit_singleline(&mut display_name);
        });
        let comment_id = ui.make_persistent_id(format!("table_add_comment_{node_id}"));
        let mut comment =
            ui.data_mut(|data| data.get_temp::<String>(comment_id).unwrap_or_default());
        ui.horizontal(|ui| {
            ui.label("中文注释:");
            ui.text_edit_singleline(&mut comment);
        });
        ui.data_mut(|data| data.insert_temp(comment_id, comment.clone()));
        let added = ui.button("添加到 Table").clicked();

        ui.data_mut(|data| data.insert_temp(name_id, display_name.clone()));
        if !added {
            return Ok(false);
        }
        let candidate = candidate()?;
        if self.roots.iter().any(|root| root.matches_root(&candidate)) {
            return Err(format!("{} 已添加到 Table", candidate.name));
        }

        match TableNode::from_candidate(&candidate, display_name, pool, &mut self.next_node_id) {
            Ok(mut root) => {
                if !comment.is_empty() {
                    root.comment = comment;
                }
                self.roots.push(root);
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }

    fn readonly_annotations(&self) -> Vec<(usize, String, String)> {
        fn visit(node: &TableNode, result: &mut Vec<(usize, String, String)>) {
            if let Some(leaf) = &node.leaf {
                result.push((leaf.variable_id, node.label.clone(), node.comment.clone()));
            }
            for child in &node.children {
                visit(child, result);
            }
        }
        let mut result = Vec::new();
        for root in &self.roots {
            visit(root, &mut result);
        }
        result
    }

    fn save_config(&self, pool: &VariablePool) -> serde_json::Value {
        let config = SavedTableConfig {
            roots: self.roots.iter().map(|root| root.to_saved(pool)).collect(),
            show_svd: self.show_svd,
            svd_path: self.svd.path().map(|path| path.display().to_string()),
            svd_registers: self.svd.saved_registers(),
            main_widths: Some(self.main_widths.clone()),
            popout_widths: Some(self.popout_widths.clone()),
            hidden_columns: Some(hidden_to_keys(&self.hidden)),
            wrap_text: Some(self.wrap_text),
        };
        serde_json::to_value(config).unwrap_or(serde_json::Value::Null)
    }

    fn load_config(
        &mut self,
        payload: &serde_json::Value,
        pool: &mut VariablePool,
    ) -> Result<(), String> {
        let payload: SavedTablePayload =
            serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?;
        let (saved_roots, svd_path, svd_registers) = match payload {
            SavedTablePayload::Current(config) => {
                self.show_svd = config.show_svd;
                // 旧配置没有列宽：首次显示时按内容适配。
                self.pending_autofit = config.main_widths.is_none();
                self.main_widths =
                    sanitize_widths(config.main_widths.unwrap_or_else(default_column_widths));
                self.popout_widths =
                    sanitize_widths(config.popout_widths.unwrap_or_else(default_column_widths));
                // 配置里没有的项沿用本机偏好。
                if let Some(keys) = &config.hidden_columns {
                    self.hidden = hidden_from_keys(keys);
                }
                if let Some(wrap) = config.wrap_text {
                    self.wrap_text = wrap;
                }
                (config.roots, config.svd_path, config.svd_registers)
            }
            SavedTablePayload::Legacy(entries) => {
                self.pending_autofit = true;
                (
                    entries
                        .into_iter()
                        .map(|entry| SavedTableNode {
                            label: entry.display_name,
                            comment: None,
                            source_name: entry.variable_name.clone(),
                            source_address: entry.variable_address,
                            expanded: true,
                            variable: Some(SavedTableLeaf {
                                variable_name: entry.variable_name,
                                variable_address: entry.variable_address,
                                variable_type: None,
                                variable_size: None,
                                enabled: true,
                                refresh_hz: 10,
                            }),
                            children: Vec::new(),
                        })
                        .collect(),
                    None,
                    Vec::new(),
                )
            }
        };

        self.roots.clear();
        self.next_node_id = 1;
        for saved in saved_roots {
            self.roots
                .push(TableNode::from_saved(saved, pool, &mut self.next_node_id)?);
        }
        self.svd.set_saved_registers(svd_registers);
        if let Some(path) = svd_path.filter(|path| !path.is_empty()) {
            self.svd.load_path(PathBuf::from(path));
        }
        Ok(())
    }
}

fn table_panel(ui: &mut Ui, state: &mut TablePluginState, pool: &VariablePool) -> bool {
    ui.horizontal_wrapped(|ui| {
        if !state.connected {
            ui.colored_label(theme::palette(ui).warning, "未连接，数值可能来自缓存");
        }
        ui.checkbox(&mut state.show_svd, "显示 SVD 寄存器");
        egui::ComboBox::from_id_salt("watch_number_format")
            .selected_text(match state.number_format {
                NumberFormat::Decimal => "十进制/常规",
                NumberFormat::Hex => "十六进制",
                NumberFormat::Binary => "二进制",
                NumberFormat::Scientific => "科学计数法",
            })
            .show_ui(ui, |ui| {
                for (format, label) in [
                    (NumberFormat::Decimal, "十进制/常规"),
                    (NumberFormat::Hex, "十六进制"),
                    (NumberFormat::Binary, "二进制"),
                    (NumberFormat::Scientific, "科学计数法"),
                ] {
                    ui.selectable_value(&mut state.number_format, format, label);
                }
            });
        column_menu(ui, state);
        if ui
            .checkbox(&mut state.wrap_text, "长文本换行")
            .on_hover_text("名称和中文注释超出列宽时换行完整显示；关闭后单行显示，悬停看全称")
            .changed()
        {
            let wrap = state.wrap_text;
            crate::ui::prefs::update(|prefs| prefs.table_wrap_text = Some(wrap));
        }
        if ui
            .button("适配列宽")
            .on_hover_text("按当前展开的内容自动调整所有列宽")
            .clicked()
        {
            state.pending_autofit = true;
        }
        ui.label(
            RichText::new("右键变量名可加入图表；右键表头可隐藏列").color(theme::muted_text(ui)),
        );
    });
    if !state.show_svd {
        return render_variable_panel(ui, state, pool);
    }
    let mut open_tree = false;
    egui::Panel::left("table_variable_panel")
        .resizable(true)
        .default_size((ui.available_width() * 0.56).max(380.0))
        .size_range(340.0..=760.0)
        .show_inside(ui, |ui| {
            open_tree = render_variable_panel(ui, state, pool);
        });
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show_inside(ui, |ui| {
            state
                .pending_svd_actions
                .extend(render_svd_panel(ui, &mut state.svd));
        });
    open_tree
}

fn render_variable_panel(ui: &mut Ui, state: &mut TablePluginState, pool: &VariablePool) -> bool {
    let mut open_tree = false;
    ui.horizontal(|ui| {
        ui.heading(RichText::new("变量监控").size(15.0));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("打开变量树").clicked() {
                open_tree = true;
            }
            let leaf_count = state.roots.iter().map(TableNode::leaf_count).sum::<usize>();
            ui.label(
                RichText::new(format!("{} 组 / {} 个变量", state.roots.len(), leaf_count))
                    .size(10.5)
                    .color(theme::muted_text(ui)),
            );
        });
    });
    ui.horizontal_wrapped(|ui| {
        ui.label("搜索:");
        ui.add(
            egui::TextEdit::singleline(&mut state.filter)
                .hint_text("变量名称 / 注释 / 地址")
                .desired_width(260.0),
        );
        if ui.button("展开全部").clicked() {
            for root in &mut state.roots {
                set_expanded(root, true);
            }
        }
        if ui.button("折叠全部").clicked() {
            for root in &mut state.roots {
                set_expanded(root, false);
            }
        }
    });
    ui.separator();

    if state.roots.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("暂无变量、结构体或数组").color(theme::muted_text(ui)));
            if ui.button("打开变量树").clicked() {
                open_tree = true;
            }
        });
        return open_tree;
    }

    let popped = ui.ctx().viewport_id() != egui::ViewportId::ROOT;
    let mut widths = if popped {
        state.popout_widths.clone()
    } else {
        state.main_widths.clone()
    };
    if state.pending_autofit {
        state.pending_autofit = false;
        for column in 0..COLUMN_COUNT {
            widths[column] = auto_width(ui, &state.roots, column, pool, state.wrap_text);
        }
    }
    let mut remove_root = None;
    let hidden_before = state.hidden;
    let visible_width = (0..COLUMN_COUNT)
        .filter(|&i| !state.hidden[i])
        .map(|i| widths[i] + 12.0)
        .sum::<f32>();
    egui::ScrollArea::horizontal()
        .id_salt("watch_horizontal")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(visible_width);
            let frame_widths = widths.clone();
            render_headers(
                ui,
                &mut widths,
                &mut state.hidden,
                &state.roots,
                pool,
                state.wrap_text,
            );
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("watch_vertical")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let filter = state.filter.trim().to_lowercase();
                    let view = RowView {
                        widths: &frame_widths,
                        hidden: hidden_before,
                        wrap_text: state.wrap_text,
                        number_format: state.number_format,
                    };
                    for (index, root) in state.roots.iter_mut().enumerate() {
                        if render_node(
                            ui,
                            root,
                            0,
                            true,
                            pool,
                            &mut state.pending_enabled,
                            &mut state.pending_writes,
                            &mut state.pending_chart,
                            &mut state.errors,
                            &filter,
                            &view,
                        ) {
                            remove_root = Some(index);
                        }
                    }
                });
        });
    if state.hidden != hidden_before {
        store_hidden_columns(&state.hidden);
    }
    if popped {
        state.popout_widths = widths;
    } else {
        state.main_widths = widths;
    }

    if let Some(index) = remove_root {
        let root = state.roots.remove(index);
        root.collect_removals(&mut state.pending_removals);
    }
    open_tree
}

fn set_expanded(node: &mut TableNode, expanded: bool) {
    node.expanded = expanded;
    for child in &mut node.children {
        set_expanded(child, expanded);
    }
}

fn matches_filter(node: &TableNode, filter: &str) -> bool {
    filter.is_empty()
        || node.label.to_lowercase().contains(filter)
        || node.comment.to_lowercase().contains(filter)
        || node.source_name.to_lowercase().contains(filter)
        || format!("0x{:x}", node.source_address).contains(filter)
        || node
            .children
            .iter()
            .any(|child| matches_filter(child, filter))
}

const COLUMN_COUNT: usize = 9;
const COL_NAME: usize = 1;
const COL_COMMENT: usize = 2;
const HEADINGS: [&str; COLUMN_COUNT] = [
    "采集",
    "参数名称",
    "中文注释（双击编辑）",
    "类型",
    "地址",
    "当前值",
    "刷新频率",
    "写入值",
    "操作",
];
/// 配置和本机偏好里使用的稳定列键，与显示文字无关。
const COLUMN_KEYS: [&str; COLUMN_COUNT] = [
    "enable", "name", "comment", "type", "address", "value", "refresh", "write", "actions",
];
const MIN_WIDTHS: [f32; COLUMN_COUNT] = [36.0, 100.0, 120.0, 60.0, 100.0, 90.0, 80.0, 100.0, 210.0];
const ROW_HEIGHT: f32 = 28.0;
/// 换行模式下适配列宽的上限，超出部分换行显示。
const WRAP_FIT_LIMIT: [f32; 2] = [460.0, 620.0];
fn default_column_widths() -> Vec<f32> {
    vec![44.0, 240.0, 360.0, 80.0, 120.0, 180.0, 90.0, 120.0, 210.0]
}
fn hidden_from_keys(keys: &[String]) -> [bool; COLUMN_COUNT] {
    let mut hidden = [false; COLUMN_COUNT];
    for (index, key) in COLUMN_KEYS.iter().enumerate() {
        // 参数名称列始终显示，否则无法识别行。
        hidden[index] = index != COL_NAME && keys.iter().any(|k| k == key);
    }
    hidden
}
fn hidden_to_keys(hidden: &[bool; COLUMN_COUNT]) -> Vec<String> {
    COLUMN_KEYS
        .iter()
        .zip(hidden)
        .filter(|(_, hidden)| **hidden)
        .map(|(key, _)| (*key).to_owned())
        .collect()
}
fn store_hidden_columns(hidden: &[bool; COLUMN_COUNT]) {
    let keys = hidden_to_keys(hidden);
    crate::ui::prefs::update(|prefs| prefs.table_hidden_columns = Some(keys));
}
fn column_checkboxes(ui: &mut Ui, hidden: &mut [bool; COLUMN_COUNT]) {
    for index in 0..COLUMN_COUNT {
        if index == COL_NAME {
            continue;
        }
        let mut shown = !hidden[index];
        if ui.checkbox(&mut shown, HEADINGS[index]).changed() {
            hidden[index] = !shown;
        }
    }
    ui.separator();
    if ui.button("显示全部列").clicked() {
        *hidden = [false; COLUMN_COUNT];
    }
    if ui.button("精简（只留名称/注释/当前值/写入）").clicked() {
        *hidden = hidden_from_keys(&["type", "address", "refresh"].map(String::from));
    }
}
fn column_menu(ui: &mut Ui, state: &mut TablePluginState) {
    let before = state.hidden;
    let hidden_count = state.hidden.iter().filter(|h| **h).count();
    let title = if hidden_count == 0 {
        "列".to_owned()
    } else {
        format!("列（已隐藏 {hidden_count}）")
    };
    ui.menu_button(title, |ui| {
        ui.label("勾选要显示的列，设置自动记住");
        column_checkboxes(ui, &mut state.hidden);
        ui.separator();
        if ui.button("恢复默认列宽").clicked() {
            state.main_widths = default_column_widths();
            state.popout_widths = default_column_widths();
        }
    });
    if state.hidden != before {
        store_hidden_columns(&state.hidden);
    }
}
fn sanitize_widths(widths: Vec<f32>) -> Vec<f32> {
    if widths.len() != HEADINGS.len() {
        return default_column_widths();
    }
    widths
        .into_iter()
        .enumerate()
        .map(|(i, w)| {
            if w.is_finite() {
                w.clamp(MIN_WIDTHS[i], 2400.0)
            } else {
                default_column_widths()[i]
            }
        })
        .collect()
}
fn cell(ui: &mut Ui, width: f32, height: f32, add: impl FnOnce(&mut Ui)) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    child.set_clip_rect(ui.clip_rect().intersect(rect));
    add(&mut child);
    response
}
fn name_text(node: &TableNode) -> &str {
    if node.children.is_empty() {
        node.source_name
            .rsplit('.')
            .next()
            .unwrap_or(&node.source_name)
    } else {
        &node.label
    }
}
/// 悬停提示：完整路径、显示名和注释一次看全。
fn full_description(node: &TableNode) -> String {
    let mut text = node.source_name.clone();
    if !node.label.is_empty() && node.label != node.source_name {
        text.push_str("\n显示名：");
        text.push_str(&node.label);
    }
    if !node.comment.is_empty() {
        text.push_str("\n注释：");
        text.push_str(&node.comment);
    }
    text
}
fn text_label(text: &str, wrap: bool) -> egui::Label {
    let label = egui::Label::new(text).sense(egui::Sense::click());
    if wrap { label.wrap() } else { label.truncate() }
}
fn copy_label(ui: &mut Ui, text: &str, copy: &str, wrap: bool) -> egui::Response {
    let response = ui.add(text_label(text, wrap)).on_hover_text(copy);
    response.context_menu(|ui| {
        if ui.button("复制完整内容").clicked() {
            ui.ctx().copy_text(copy.to_owned());
            ui.close();
        }
    });
    response
}
fn name_indent(depth: usize) -> f32 {
    (depth as f32 * 16.0).min(160.0)
}
/// 分组行没有注释时，标题横跨名称和注释两列。
fn group_spans_comment(node: &TableNode, hidden: &[bool; COLUMN_COUNT]) -> bool {
    node.leaf.is_none()
        && node.comment.is_empty()
        && node.comment_edit.is_none()
        && !hidden[COL_COMMENT]
}
fn wrapped_height(ui: &Ui, text: &str, wrap_width: f32) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    ui.painter()
        .layout(
            text.to_owned(),
            egui::TextStyle::Body.resolve(ui.style()),
            theme::palette(ui).text,
            wrap_width.max(40.0),
        )
        .size()
        .y
}
fn row_height(ui: &Ui, node: &TableNode, depth: usize, view: &RowView<'_>) -> f32 {
    if !view.wrap_text {
        return ROW_HEIGHT;
    }
    let spans = group_spans_comment(node, &view.hidden);
    let name_width = if spans {
        view.widths[COL_NAME] + 12.0 + view.widths[COL_COMMENT]
    } else {
        view.widths[COL_NAME]
    } - name_indent(depth)
        - 36.0;
    let mut height = wrapped_height(ui, name_text(node), name_width);
    if !view.hidden[COL_COMMENT] && !spans && node.comment_edit.is_none() {
        height = height.max(wrapped_height(
            ui,
            &node.comment,
            view.widths[COL_COMMENT] - 4.0,
        ));
    }
    (height + 8.0).max(ROW_HEIGHT)
}
fn auto_width(
    ui: &Ui,
    roots: &[TableNode],
    column: usize,
    pool: &VariablePool,
    wrap_text: bool,
) -> f32 {
    fn visit(
        ui: &Ui,
        node: &TableNode,
        depth: usize,
        column: usize,
        width: &mut f32,
        pool: &VariablePool,
    ) {
        let text = match column {
            // 无注释的分组标题会跨列或换行，不撑宽名称列。
            1 if node.leaf.is_none() && node.comment.is_empty() => String::new(),
            1 => name_text(node).to_owned(),
            2 => node.comment.clone(),
            3 => node
                .leaf
                .as_ref()
                .and_then(|l| pool.get(l.variable_id))
                .map(|v| crate::dwarf::types::extend_type_label(&v.ext_type))
                .unwrap_or("结构 / 数组")
                .to_owned(),
            6 => node
                .leaf
                .as_ref()
                .map(|l| format!("{} Hz", l.refresh_hz))
                .unwrap_or_default(),
            7 => node
                .leaf
                .as_ref()
                .map(|l| l.edit_buffer.clone())
                .unwrap_or_default(),
            8 => "加入图表 写入 删除".to_owned(),
            4 => format!("0x{:08X}", node.source_address),
            5 => node
                .leaf
                .as_ref()
                .map(|l| l.current_value.clone())
                .unwrap_or_default(),
            _ => String::new(),
        };
        let extra = if column == 1 {
            name_indent(depth) + 40.0
        } else {
            16.0
        };
        if !text.is_empty() {
            *width = width.max(
                ui.painter()
                    .layout_no_wrap(
                        text,
                        egui::TextStyle::Body.resolve(ui.style()),
                        theme::palette(ui).text,
                    )
                    .size()
                    .x
                    + extra,
            );
        }
        if node.expanded {
            for child in &node.children {
                visit(ui, child, depth + 1, column, width, pool);
            }
        }
    }
    let mut width = MIN_WIDTHS[column];
    for root in roots {
        visit(ui, root, 0, column, &mut width, pool);
    }
    let limit = match column {
        COL_NAME | COL_COMMENT if wrap_text => WRAP_FIT_LIMIT[column - COL_NAME],
        _ => 2400.0,
    };
    width.clamp(MIN_WIDTHS[column], limit)
}
fn render_headers(
    ui: &mut Ui,
    widths: &mut [f32],
    hidden: &mut [bool; COLUMN_COUNT],
    roots: &[TableNode],
    pool: &VariablePool,
    wrap_text: bool,
) {
    let shown = *hidden;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        for i in 0..HEADINGS.len() {
            if shown[i] {
                continue;
            }
            let response = cell(ui, widths[i], ROW_HEIGHT, |ui| {
                ui.label(RichText::new(HEADINGS[i]).strong());
            });
            ui.interact(
                response.rect,
                ui.id().with(("column_header", i)),
                egui::Sense::click(),
            )
            .on_hover_text("右键：选择显示哪些列")
            .context_menu(|ui| {
                if i != COL_NAME && ui.button(format!("隐藏“{}”列", HEADINGS[i])).clicked() {
                    hidden[i] = true;
                    ui.close();
                }
                ui.separator();
                column_checkboxes(ui, hidden);
            });
            let grip = egui::Rect::from_center_size(
                response.rect.right_center() + egui::vec2(5.0, 0.0),
                egui::vec2(10.0, ROW_HEIGHT),
            );
            let drag = ui
                .interact(
                    grip,
                    ui.id().with(("column_grip", i)),
                    egui::Sense::click_and_drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                .on_hover_text("拖拽调整；双击按展开内容适配");
            ui.painter()
                .vline(grip.center().x, grip.y_range(), theme::panel_stroke(ui));
            if drag.dragged() {
                widths[i] = (widths[i] + ui.input(|input| input.pointer.delta().x))
                    .clamp(MIN_WIDTHS[i], 2400.0);
            }
            if drag.double_clicked() {
                widths[i] = auto_width(ui, roots, i, pool, wrap_text);
            }
        }
    });
}
/// 一帧内所有行共用的列显示参数。
struct RowView<'a> {
    widths: &'a [f32],            // 各列宽度，像素
    hidden: [bool; COLUMN_COUNT], // 各列是否隐藏
    wrap_text: bool,              // 长文本是否换行
    number_format: NumberFormat,  // 当前值显示格式
}
#[allow(clippy::too_many_arguments)]
fn render_node(
    ui: &mut Ui,
    node: &mut TableNode,
    depth: usize,
    is_root: bool,
    pool: &VariablePool,
    enabled_changes: &mut Vec<(usize, bool)>,
    pending_writes: &mut Vec<(usize, u64)>,
    pending_chart: &mut Vec<(usize, String, String)>,
    errors: &mut Vec<String>,
    filter: &str,
    view: &RowView<'_>,
) -> bool {
    if !matches_filter(node, filter) {
        return false;
    }
    let widths = view.widths;
    let hidden = &view.hidden;
    let number_format = view.number_format;
    let mut remove = false;
    let variable = node
        .leaf
        .as_ref()
        .and_then(|leaf| pool.get(leaf.variable_id));
    let mut write = false;
    ui.push_id(node.id, |ui| {
        let fill = if node.id % 2 == 0 {
            theme::palette(ui).surface_bg
        } else {
            egui::Color32::TRANSPARENT
        };
        let height = row_height(ui, node, depth, view);
        let spans = group_spans_comment(node, hidden);
        egui::Frame::NONE.fill(fill).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                if !hidden[0] {
                    cell(ui, widths[0], height, |ui| {
                        if let Some(leaf) = &mut node.leaf {
                            if ui.checkbox(&mut leaf.enabled, "").changed() {
                                enabled_changes.push((leaf.variable_id, leaf.enabled));
                            }
                        } else {
                            let check = node.check_state();
                            let mut enabled = check == CheckState::All;
                            if ui
                                .add_enabled(
                                    check != CheckState::Unavailable,
                                    egui::Checkbox::new(&mut enabled, "")
                                        .indeterminate(check == CheckState::Partial),
                                )
                                .changed()
                            {
                                node.set_enabled_recursive(enabled, enabled_changes);
                            }
                        }
                    });
                }
                let name_width = if spans {
                    widths[COL_NAME] + 12.0 + widths[COL_COMMENT]
                } else {
                    widths[COL_NAME]
                };
                cell(ui, name_width, height, |ui| {
                    ui.add_space(name_indent(depth));
                    if node.children.is_empty() {
                        ui.add_space(22.0);
                    } else if ui
                        .add_sized(
                            [20.0, 20.0],
                            egui::Button::new(if node.expanded || !filter.is_empty() {
                                "−"
                            } else {
                                "+"
                            }),
                        )
                        .clicked()
                    {
                        node.expanded = !node.expanded;
                    }
                    let name_response = ui
                        .add(text_label(name_text(node), view.wrap_text))
                        .on_hover_text(full_description(node));
                    name_response.context_menu(|ui| {
                        if ui.button("复制完整变量路径").clicked() {
                            ui.ctx().copy_text(node.source_name.clone());
                            ui.close();
                        }
                        if ui
                            .button(if node.leaf.is_some() {
                                "加入图表"
                            } else {
                                "将组内数值变量加入图表"
                            })
                            .clicked()
                        {
                            collect_chart_variables(node, pending_chart);
                            ui.close();
                        }
                        if !hidden[COL_COMMENT] && ui.button("编辑中文注释").clicked() {
                            node.comment_edit = Some(node.comment.clone());
                            node.comment_focus = true;
                            ui.close();
                        }
                    });
                });
                if !hidden[COL_COMMENT] && !spans {
                    cell(ui, widths[COL_COMMENT], height, |ui| {
                        if node.comment_edit.is_some() {
                            let buffer = node.comment_edit.as_mut().unwrap();
                            let edit = ui.add_sized(
                                [widths[COL_COMMENT], 24.0],
                                egui::TextEdit::singleline(buffer).id_salt("comment_editor"),
                            );
                            if node.comment_focus {
                                edit.request_focus();
                                node.comment_focus = false;
                            }
                            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                node.comment_edit = None;
                            } else if edit.lost_focus()
                                || ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                node.comment = node.comment_edit.take().unwrap();
                            }
                        } else {
                            let response = copy_label(
                                ui,
                                if node.comment.is_empty() {
                                    "双击添加注释"
                                } else {
                                    &node.comment
                                },
                                &node.comment,
                                view.wrap_text,
                            );
                            if response.double_clicked() {
                                node.comment_edit = Some(node.comment.clone());
                                node.comment_focus = true;
                            }
                        }
                    });
                }
                if !hidden[3] {
                    cell(ui, widths[3], height, |ui| {
                        ui.label(
                            variable
                                .map(|v| crate::dwarf::types::extend_type_label(&v.ext_type))
                                .unwrap_or("结构 / 数组"),
                        );
                    });
                }
                if !hidden[4] {
                    cell(ui, widths[4], height, |ui| {
                        ui.monospace(format!(
                            "0x{:08X}",
                            variable.map(|v| v.address).unwrap_or(node.source_address)
                        ));
                    });
                }
                if !hidden[5] {
                    cell(ui, widths[5], height, |ui| {
                        if let Some(leaf) = &node.leaf {
                            let value = leaf
                                .raw
                                .zip(variable)
                                .map(|(raw, var)| format_number(raw, &var.ext_type, number_format))
                                .unwrap_or_else(|| leaf.current_value.clone());
                            let changed = leaf
                                .changed_at
                                .is_some_and(|t| t.elapsed().as_secs_f32() < 0.8);
                            let stale = leaf.enabled
                                && leaf
                                    .sample_at
                                    .is_some_and(|t| t.elapsed().as_secs_f32() > 2.0);
                            let response = if changed {
                                ui.scope(|ui| {
                                    ui.visuals_mut().override_text_color =
                                        Some(theme::palette(ui).accent_hover);
                                    copy_label(ui, &value, &value, false)
                                })
                                .inner
                            } else {
                                copy_label(ui, &value, &value, false)
                            };
                            if stale {
                                response.on_hover_text("采样超过 2 秒未更新，数值可能陈旧");
                                ui.colored_label(theme::palette(ui).warning, "陈旧");
                            } else if leaf.raw.is_none() {
                                response.on_hover_text("尚未收到采样；不能据此判定 MCU 数值");
                            }
                        } else {
                            ui.label(format!("{} 个变量", node.leaf_count()));
                        }
                    });
                }
                if !hidden[6] {
                    cell(ui, widths[6], height, |ui| {
                        if let Some(leaf) = &mut node.leaf {
                            ui.add(
                                egui::DragValue::new(&mut leaf.refresh_hz)
                                    .range(1..=60)
                                    .suffix(" Hz"),
                            )
                            .on_hover_text("只改变显示刷新，不改变硬件采样率");
                        }
                    });
                }
                if !hidden[7] {
                    cell(ui, widths[7], height, |ui| {
                        if let Some(leaf) = &mut node.leaf {
                            let edit = ui.add_sized(
                                [widths[7], 24.0],
                                egui::TextEdit::singleline(&mut leaf.edit_buffer)
                                    .id_salt("write_value")
                                    .font(egui::TextStyle::Monospace),
                            );
                            write =
                                edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        }
                    });
                }
                if !hidden[8] {
                    cell(ui, widths[8], height, |ui| {
                        if ui.small_button("加入图表").clicked() {
                            collect_chart_variables(node, pending_chart);
                        }
                        // 写入值列隐藏时不提供写入按钮，避免误写旧输入。
                        if node.leaf.is_some() && !hidden[7] {
                            write |= ui.small_button("写入").clicked();
                        }
                        if is_root && ui.small_button("删除").clicked() {
                            remove = true;
                        }
                    });
                }
            });
        });
    });
    if write {
        if let (Some(leaf), Some(variable)) = (&node.leaf, variable) {
            match validate_write(&leaf.edit_buffer, &variable.ext_type) {
                Ok(value) => pending_writes.push((leaf.variable_id, value)),
                Err(error) => errors.push(error),
            }
        }
    }
    if node.expanded || !filter.is_empty() {
        let child_filter = if node.label.to_lowercase().contains(filter)
            || node.comment.to_lowercase().contains(filter)
            || node.source_name.to_lowercase().contains(filter)
            || format!("0x{:x}", node.source_address).contains(filter)
        {
            ""
        } else {
            filter
        };
        for child in &mut node.children {
            render_node(
                ui,
                child,
                depth + 1,
                false,
                pool,
                enabled_changes,
                pending_writes,
                pending_chart,
                errors,
                child_filter,
                view,
            );
        }
    }
    remove
}

fn collect_chart_variables(node: &TableNode, output: &mut Vec<(usize, String, String)>) {
    if let Some(leaf) = &node.leaf {
        output.push((
            leaf.variable_id,
            name_text(node).to_owned(),
            node.comment.clone(),
        ));
    }
    for child in &node.children {
        collect_chart_variables(child, output);
    }
}

fn validate_write(input: &str, ext_type: &ExtendType) -> Result<u64, String> {
    let value = input.trim();
    if value.is_empty() {
        return Err("请输入值".to_owned());
    }
    match ext_type {
        ExtendType::U8 => value
            .parse::<u8>()
            .map(u64::from)
            .map_err(|_| "超出 u8 范围 (0-255)".to_owned()),
        ExtendType::I8 => value
            .parse::<i8>()
            .map(|value| value as u64)
            .map_err(|_| "超出 i8 范围 (-128~127)".to_owned()),
        ExtendType::U16 => value
            .parse::<u16>()
            .map(u64::from)
            .map_err(|_| "超出 u16 范围".to_owned()),
        ExtendType::I16 => value
            .parse::<i16>()
            .map(|value| value as u64)
            .map_err(|_| "超出 i16 范围".to_owned()),
        ExtendType::U32 => value
            .parse::<u32>()
            .map(u64::from)
            .map_err(|_| "超出 u32 范围".to_owned()),
        ExtendType::I32 => value
            .parse::<i32>()
            .map(|value| value as u64)
            .map_err(|_| "超出 i32 范围".to_owned()),
        ExtendType::U64 => value.parse::<u64>().map_err(|_| "超出 u64 范围".to_owned()),
        ExtendType::I64 => value
            .parse::<i64>()
            .map(|value| value as u64)
            .map_err(|_| "超出 i64 范围".to_owned()),
        ExtendType::Float => value
            .parse::<f32>()
            .map(|value| u64::from(value.to_bits()))
            .map_err(|_| "无效的 float".to_owned()),
        ExtendType::Double => value
            .parse::<f64>()
            .map(f64::to_bits)
            .map_err(|_| "无效的 double".to_owned()),
        ExtendType::Other => Err("Other 类型不支持写入".to_owned()),
    }
}

fn format_value_into(data: &[u8], ext_type: &ExtendType, output: &mut String) {
    use ExtendType::*;
    output.clear();
    if data.is_empty() {
        output.push_str("--");
        return;
    }
    match ext_type {
        U8 => write!(output, "0x{:02X} ({})", data[0], data[0]),
        I8 => {
            let value = i8::from_le_bytes([data[0]]);
            write!(output, "0x{:02X} ({value})", data[0])
        }
        U16 if data.len() >= 2 => {
            let value = u16::from_le_bytes([data[0], data[1]]);
            write!(output, "0x{value:04X} ({value})")
        }
        I16 if data.len() >= 2 => {
            let value = i16::from_le_bytes([data[0], data[1]]);
            write!(output, "0x{value:04X} ({value})")
        }
        U32 if data.len() >= 4 => {
            let value = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            write!(output, "0x{value:08X} ({value})")
        }
        I32 if data.len() >= 4 => {
            let value = i32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            write!(output, "0x{value:08X} ({value})")
        }
        U64 if data.len() >= 8 => {
            let value = u64::from_le_bytes(data[..8].try_into().unwrap());
            write!(output, "0x{value:016X} ({value})")
        }
        I64 if data.len() >= 8 => {
            let value = i64::from_le_bytes(data[..8].try_into().unwrap());
            write!(output, "0x{value:016X} ({value})")
        }
        Float if data.len() >= 4 => {
            let value = f32::from_le_bytes(data[..4].try_into().unwrap());
            write!(output, "{value:.4}")
        }
        Double if data.len() >= 8 => {
            let value = f64::from_le_bytes(data[..8].try_into().unwrap());
            write!(output, "{value:.6}")
        }
        _ => write!(output, "{data:02X?}"),
    }
    .expect("writing to a String cannot fail");
}

#[cfg(test)]
mod tests {
    use super::{SavedTablePayload, validate_write};
    use crate::dwarf::types::ExtendType;

    #[test]
    fn watch_filter_reveals_matching_children_and_addresses() {
        use crate::ui::plugin::VariableCandidate;
        let child = VariableCandidate {
            label: "speed".to_owned(),
            name: "motor.speed".to_owned(),
            address: 0x2000_0004,
            ext_type: ExtendType::Float,
            size: 4,
            children: Vec::new(),
        };
        let parent = VariableCandidate {
            label: "motor".to_owned(),
            name: "motor".to_owned(),
            address: 0x2000_0000,
            ext_type: ExtendType::Other,
            size: 8,
            children: vec![child],
        };
        let mut pool = crate::model::VariablePool::default();
        let mut id = 1;
        let mut node =
            super::TableNode::from_candidate(&parent, "电机".to_owned(), &mut pool, &mut id)
                .unwrap();
        assert!(super::matches_filter(&node, "speed"));
        assert!(super::matches_filter(&node, "0x20000004"));
        assert!(!super::matches_filter(&node, "missing"));
        super::set_expanded(&mut node, false);
        assert!(!node.expanded && !node.children[0].expanded);
        assert!(super::matches_filter(&node, "speed"));
    }

    #[test]
    fn watch_table_renders_scalar_rows_without_hardware() {
        use crate::ui::plugin::VariableCandidate;
        let mut state = super::TablePluginState::default();
        let mut pool = crate::model::VariablePool::default();
        for index in 0..3 {
            let candidate = VariableCandidate {
                label: format!("value_{index}"),
                name: format!("value_{index}"),
                address: 0x2000_0000 + index * 4,
                ext_type: ExtendType::Float,
                size: 4,
                children: Vec::new(),
            };
            state.roots.push(
                super::TableNode::from_candidate(
                    &candidate,
                    candidate.label.clone(),
                    &mut pool,
                    &mut state.next_node_id,
                )
                .unwrap(),
            );
        }
        let ctx = eframe::egui::Context::default();
        for _ in 0..2 {
            let _ = ctx.run(eframe::egui::RawInput::default(), |ctx| {
                eframe::egui::CentralPanel::default().show(ctx, |ui| {
                    super::table_panel(ui, &mut state, &pool);
                });
            });
        }
        assert!(state.pending_writes.is_empty());
        assert_eq!(state.roots.len(), 3);
    }

    #[test]
    fn validates_integer_write_ranges() {
        assert_eq!(validate_write("255", &ExtendType::U8), Ok(255));
        assert!(validate_write("256", &ExtendType::U8).is_err());
        assert_eq!(validate_write("-1", &ExtendType::I8), Ok(u64::MAX));
    }

    #[test]
    fn accepts_legacy_flat_table_configuration() {
        let payload = serde_json::json!([{
            "variable_name": "counter",
            "variable_address": 536870912,
            "display_name": "Counter"
        }]);

        assert!(matches!(
            serde_json::from_value::<SavedTablePayload>(payload).unwrap(),
            SavedTablePayload::Legacy(entries) if entries.len() == 1
        ));
    }
}

#[cfg(test)]
mod enhanced_table_tests {
    use super::*;
    #[test]
    fn widths_reject_invalid_config_and_preserve_valid_sizes() {
        assert_eq!(sanitize_widths(vec![1.0]), default_column_widths());
        let mut widths = default_column_widths();
        widths[1] = 650.0;
        widths[2] = f32::NAN;
        let widths = sanitize_widths(widths);
        assert_eq!(widths[1], 650.0);
        assert_eq!(widths[2], 360.0);
    }
    #[test]
    fn synthetic_132_rows_migrate_in_memory() {
        // Synthetic rows exercise the same legacy JSON shape without storing firmware data.
        let roots: Vec<_> = (0..132).map(|i| serde_json::json!({"label":format!("field_{i} ｜ 参数中文说明"),"source_name":format!("axis.field_{i}"),"source_address":536870912u64+i*4,"variable":null,"children":[]})).collect();
        let payload =
            serde_json::json!({"roots":roots,"show_svd":false,"svd_path":null,"svd_registers":[]});
        let mut state = TablePluginState::default();
        let mut pool = VariablePool::default();
        state.load_config(&payload, &mut pool).unwrap();
        assert_eq!(state.roots.len(), 132);
        assert_eq!(state.roots[131].comment, "参数中文说明");
        state.main_widths[1] = 480.0;
        let saved = state.save_config(&pool);
        let mut again = TablePluginState::default();
        again.load_config(&saved, &mut pool).unwrap();
        assert_eq!(again.main_widths[1], 480.0);
        assert_eq!(again.roots[0].comment, "参数中文说明");
        assert!(matches_filter(&again.roots[0], "中文"));
        assert!(again.pending_writes.is_empty());
    }
}

#[cfg(test)]
mod column_tests {
    use super::*;

    fn long_table() -> (TablePluginState, VariablePool) {
        let long_comment = "可写：0退出，1启用；最后再启用。这是一段很长的中文说明，用来确认超出列宽时不会被截断而是换行完整显示";
        let payload = serde_json::json!({"roots":[{
            "label":"01 调参控制 g_dart_tune（只在手动模式生效）","source_name":"g_dart_tune",
            "source_address":603980224u64,"expanded":true,"variable":null,
            "children":[{"label":format!("enable ｜ {long_comment}"),"source_name":"g_dart_tune.enable",
                "source_address":603980224u64,"expanded":true,"children":[],
                "variable":{"variable_name":"g_dart_tune.enable","variable_address":603980224u64,
                    "variable_type":"U8","variable_size":1,"enabled":true,"refresh_hz":10}}]}],
            "show_svd":false,"svd_path":null,"svd_registers":[]});
        let mut pool = VariablePool::default();
        pool.add(&crate::dwarf::types::ExtendConfig {
            name: "g_dart_tune.enable".into(),
            address: 603980224,
            ext_type: ExtendType::U8,
            size: 1,
            array_index: None,
            array_count: None,
        });
        let mut state = TablePluginState::default();
        state.load_config(&payload, &mut pool).unwrap();
        (state, pool)
    }

    fn render_texts(state: &mut TablePluginState, pool: &VariablePool) -> Vec<String> {
        let ctx = egui::Context::default();
        crate::ui::theme::install(&ctx);
        let mut texts = Vec::new();
        // 第一帧执行待定的列宽适配，第二帧取稳定结果。
        for _ in 0..2 {
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1400.0, 700.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    render_variable_panel(ui, state, pool);
                },
            );
            texts = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::epaint::Shape::Text(text) => Some(text.galley.job.text.clone()),
                    _ => None,
                })
                .collect();
        }
        texts
    }

    #[test]
    fn hidden_column_keys_roundtrip_and_name_column_cannot_be_hidden() {
        let hidden = hidden_from_keys(&["address", "name", "refresh", "bogus"].map(String::from));
        assert!(hidden[4] && hidden[6]);
        assert!(!hidden[COL_NAME]);
        assert_eq!(hidden_to_keys(&hidden), ["address", "refresh"]);
        assert_eq!(COLUMN_KEYS.len(), HEADINGS.len());
    }

    #[test]
    fn old_config_autofits_and_new_fields_survive_save_and_load() {
        let (mut state, mut pool) = long_table();
        assert!(state.pending_autofit, "旧配置没有列宽，应按内容适配");
        assert!(state.wrap_text);
        state.hidden = hidden_from_keys(&["address".to_owned()]);
        state.wrap_text = false;
        state.main_widths[COL_NAME] = 333.0;
        let saved = state.save_config(&pool);
        assert_eq!(saved["hidden_columns"], serde_json::json!(["address"]));
        assert_eq!(saved["wrap_text"], false);
        // 旧字段格式不变。
        assert_eq!(saved["roots"][0]["children"][0]["label"], "enable");
        assert!(
            saved["roots"][0]["children"][0]["comment"]
                .as_str()
                .unwrap()
                .starts_with("可写：0退出")
        );
        let mut again = TablePluginState::default();
        again.load_config(&saved, &mut pool).unwrap();
        assert!(!again.pending_autofit);
        assert!(again.hidden[4]);
        assert!(!again.wrap_text);
        assert_eq!(again.main_widths[COL_NAME], 333.0);
    }

    #[test]
    fn long_labels_render_in_full_and_wrapped_rows_grow() {
        let (mut state, pool) = long_table();
        let texts = render_texts(&mut state, &pool);
        assert!(!state.pending_autofit);
        // 换行模式下列宽受上限约束，注释完整出现在绘制结果中。
        assert!(state.main_widths[COL_COMMENT] <= WRAP_FIT_LIMIT[1]);
        assert!(
            texts
                .iter()
                .any(|t| t == "01 调参控制 g_dart_tune（只在手动模式生效）")
        );
        assert!(texts.iter().any(|t| t.ends_with("而是换行完整显示")));

        let ctx = egui::Context::default();
        crate::ui::theme::install(&ctx);
        let _ = ctx.run_ui(Default::default(), |ui| {
            let widths = default_column_widths();
            let mut view = RowView {
                widths: &widths,
                hidden: [false; COLUMN_COUNT],
                wrap_text: true,
                number_format: NumberFormat::Decimal,
            };
            let leaf = &state.roots[0].children[0];
            let wrapped = row_height(ui, leaf, 1, &view);
            assert!(wrapped > ROW_HEIGHT, "长注释应增加行高，实际 {wrapped}");
            assert!(group_spans_comment(&state.roots[0], &view.hidden));
            assert!(!group_spans_comment(leaf, &view.hidden));
            view.wrap_text = false;
            assert_eq!(row_height(ui, leaf, 1, &view), ROW_HEIGHT);
        });
    }

    #[test]
    fn hidden_columns_are_not_drawn() {
        let (mut state, pool) = long_table();
        let texts = render_texts(&mut state, &pool);
        assert!(texts.iter().any(|t| t == "地址"));
        assert!(texts.iter().any(|t| t == "0x240001C0"));
        state.hidden = hidden_from_keys(&["address", "type", "refresh"].map(String::from));
        let texts = render_texts(&mut state, &pool);
        for gone in ["地址", "0x240001C0", "类型", "刷新频率"] {
            assert!(!texts.iter().any(|t| t == gone), "{gone} 列应已隐藏");
        }
        assert!(texts.iter().any(|t| t == "当前值"));
    }
}

#[cfg(test)]
mod comment_interaction_tests {
    use super::*;

    #[test]
    fn comment_enter_confirms_escape_cancels_and_neither_queues_writes() {
        let ctx = egui::Context::default();
        let mut pool = VariablePool::default();
        let mut state = TablePluginState::default();
        let candidate = VariableCandidate {
            label: "speed_rpm".into(),
            name: "axis.speed_rpm".into(),
            address: 0x20000000,
            ext_type: ExtendType::Float,
            size: 4,
            children: vec![],
        };
        state.roots.push(
            TableNode::from_candidate(
                &candidate,
                candidate.label.clone(),
                &mut pool,
                &mut state.next_node_id,
            )
            .unwrap(),
        );
        state.roots[0].comment = "original".into();
        for (key, expected) in [
            (egui::Key::Enter, "updated"),
            (egui::Key::Escape, "updated"),
        ] {
            state.roots[0].comment_edit = Some(
                if key == egui::Key::Enter {
                    "updated"
                } else {
                    "cancelled"
                }
                .into(),
            );
            state.roots[0].comment_focus = true;
            for event in [None, Some(key)] {
                let mut input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1600.0, 800.0),
                    )),
                    ..Default::default()
                };
                if let Some(key) = event {
                    input.events.push(egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        render_variable_panel(ui, &mut state, &pool);
                    });
                });
            }
            assert_eq!(state.roots[0].comment, expected);
            assert!(state.pending_writes.is_empty());
        }
    }
}
