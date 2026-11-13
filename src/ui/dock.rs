use std::collections::HashMap;

use eframe::egui::{self, RichText, Ui};

use crate::model::VariablePool;
use crate::ui::plugin::{MemRWPlugin, PluginAction, PluginRenderContext};
use crate::ui::theme;
use crate::ui::variable_tree_panel::VariableTreePanel;

#[derive(Debug, Clone, Default)]
pub struct DockLayoutState {
    pub workspace: super::workspace::WorkspaceSettings,
    maximized: Option<String>,
    popped: HashMap<String, bool>,
    paused: HashMap<String, bool>,
    active_plugin: Option<String>,
    pending_pop_in: Option<String>,
    dragging: Option<String>,
}

impl DockLayoutState {
    fn is_popped(&self, plugin_id: &str) -> bool {
        self.popped.get(plugin_id).copied().unwrap_or(false)
    }

    fn set_popped(&mut self, plugin_id: &str, popped: bool) {
        if popped && self.maximized.as_deref() == Some(plugin_id) {
            self.maximized = None;
        }
        self.popped.insert(plugin_id.to_owned(), popped);
    }

    pub fn is_plugin_paused(&self, plugin_id: &str) -> bool {
        self.paused.get(plugin_id).copied().unwrap_or(false)
    }

    fn toggle_plugin_paused(&mut self, plugin_id: &str) {
        let paused = !self.is_plugin_paused(plugin_id);
        self.paused.insert(plugin_id.to_owned(), paused);
    }

    fn active_plugin_id(&self) -> Option<&str> {
        self.active_plugin.as_deref()
    }

    fn set_active_plugin(&mut self, plugin_id: impl Into<String>) {
        self.active_plugin = Some(plugin_id.into());
    }

    pub fn restore_regions(&mut self) {
        self.maximized = None;
    }

    pub fn focus_plugin(&mut self, plugin_id: &str) {
        if self.maximized.as_deref().is_some_and(|id| id != plugin_id) {
            self.maximized = None;
        }
        self.set_popped(plugin_id, false);
        self.workspace.set_hidden(plugin_id, false);
        self.set_active_plugin(plugin_id);
    }

    /// 侧栏图标：隐藏的显示，已显示的隐藏。
    fn toggle_plugin_visible(&mut self, plugin_id: &str) {
        if self.workspace.is_hidden(plugin_id) {
            self.focus_plugin(plugin_id);
        } else if self.workspace.set_hidden(plugin_id, true) {
            if self.maximized.as_deref() == Some(plugin_id) {
                self.maximized = None;
            }
        }
    }

    /// 主窗口内实际占位的面板，按槽位顺序。
    fn visible_slots(&self) -> Vec<String> {
        self.workspace
            .slots()
            .into_iter()
            .filter(|id| !self.workspace.is_hidden(id) && !self.is_popped(id))
            .collect()
    }

    fn request_pop_in(&mut self, plugin_id: &str) {
        self.pending_pop_in = Some(plugin_id.to_owned());
    }

    fn confirm_pop_in(&mut self) {
        if let Some(plugin_id) = self.pending_pop_in.take() {
            self.focus_plugin(&plugin_id);
        }
    }
}

pub fn show_plugin_activity_bar(
    ui: &mut Ui,
    dock: &mut DockLayoutState,
    plugins: &mut [Box<dyn MemRWPlugin>],
    side_panel_open: &mut bool,
) {
    if plugins.is_empty() {
        return;
    }
    ensure_active_plugin(dock, plugins);
    show_activity_bar(ui, dock, plugins, side_panel_open);
    show_pop_in_confirmation(ui.ctx(), dock, plugins);
}

pub fn show_active_plugin_content(
    ui: &mut Ui,
    dock: &mut DockLayoutState,
    plugins: &mut [Box<dyn MemRWPlugin>],
    pool: &mut VariablePool,
    running: bool,
    interaction_enabled: bool,
    variable_tree: &mut VariableTreePanel,
) -> Vec<PluginAction> {
    let mut actions = Vec::new();

    if plugins.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label("没有已加载的插件。");
        });
        return actions;
    }

    let rect = ui.available_rect_before_wrap();
    if dock
        .maximized
        .as_ref()
        .is_some_and(|id| dock.workspace.is_hidden(id) || dock.is_popped(id))
    {
        dock.maximized = None;
    }
    let visible = dock.visible_slots();
    if visible.is_empty() {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child.centered_and_justified(|ui| {
            ui.label("面板都已弹出为独立窗口；点击左侧图标可移回主窗口。");
        });
        ui.allocate_rect(rect, egui::Sense::hover());
        return actions;
    }
    let areas = layout_areas(ui, rect, &visible, dock);
    for (id, area) in &areas {
        if dock.maximized.as_ref().is_some_and(|max| max != id) {
            continue;
        }
        let area = if dock.maximized.is_some() {
            rect
        } else {
            *area
        };
        let Some(index) = plugins.iter().position(|p| p.id() == id) else {
            continue;
        };
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(id)
                .max_rect(area)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        child.set_clip_rect(area);
        show_plugin_docked(
            &mut child,
            dock,
            plugins[index].as_mut(),
            pool,
            running,
            interaction_enabled,
            variable_tree,
            &mut actions,
            visible.len() > 1,
        );
    }
    handle_panel_drag(ui, dock, plugins, &areas);
    ui.allocate_rect(rect, egui::Sense::hover());
    actions
}

/// 按排列方式切分区域并绘制可拖动的分隔条。
fn layout_areas(
    ui: &mut Ui,
    rect: egui::Rect,
    visible: &[String],
    dock: &mut DockLayoutState,
) -> Vec<(String, egui::Rect)> {
    use super::workspace::Arrangement;
    let arrangement = dock.workspace.arrangement();
    // 第一条分隔的方向：true 为上下排列。
    let first_vertical = matches!(arrangement, Arrangement::MainTop | Arrangement::Rows);
    let second_vertical = matches!(arrangement, Arrangement::MainLeft | Arrangement::Rows);
    match visible {
        [only] => vec![(only.clone(), rect)],
        [first, second] => {
            let (a, b) = split_region(
                ui,
                rect,
                first_vertical,
                &mut dock.workspace.primary_ratio,
                "primary_split",
            );
            vec![(first.clone(), a), (second.clone(), b)]
        }
        [first, second, third, ..] => {
            let (a, rest) = split_region(
                ui,
                rect,
                first_vertical,
                &mut dock.workspace.primary_ratio,
                "primary_split",
            );
            let (b, c) = split_region(
                ui,
                rest,
                second_vertical,
                &mut dock.workspace.secondary_ratio,
                "secondary_split",
            );
            vec![(first.clone(), a), (second.clone(), b), (third.clone(), c)]
        }
        [] => Vec::new(),
    }
}

/// 拖动面板标题：悬停目标高亮，松开后交换位置。
fn handle_panel_drag(
    ui: &Ui,
    dock: &mut DockLayoutState,
    plugins: &[Box<dyn MemRWPlugin>],
    areas: &[(String, egui::Rect)],
) {
    let Some(source) = dock.dragging.clone() else {
        return;
    };
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let target = pointer.and_then(|pos| {
        areas
            .iter()
            .find(|(id, area)| *id != source && area.contains(pos))
    });
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    if let Some((id, area)) = target {
        let colors = theme::palette(ui);
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("dock_panel_drag"),
        ));
        painter.rect_filled(*area, 4.0, colors.accent_weak.gamma_multiply(0.75));
        let title = |id: &str| {
            plugins
                .iter()
                .find(|p| p.id() == id)
                .map(|p| p.title())
                .unwrap_or_default()
        };
        painter.text(
            area.center(),
            egui::Align2::CENTER_CENTER,
            format!("松开：「{}」与「{}」交换位置", title(&source), title(id)),
            egui::FontId::proportional(16.0),
            colors.text,
        );
    }
    if !ui.input(|i| i.pointer.any_down()) {
        if let Some((id, _)) = target {
            dock.workspace.swap_panels(&source, id);
        }
        dock.dragging = None;
    }
}

fn split_region(
    ui: &mut Ui,
    rect: egui::Rect,
    vertical: bool,
    ratio: &mut f32,
    id: &str,
) -> (egui::Rect, egui::Rect) {
    let length = if vertical {
        rect.height()
    } else {
        rect.width()
    };
    let min = if vertical { 180.0 } else { 280.0 };
    // Small workspaces scroll inside panes instead of overlapping their neighbours.
    let lower = (min / length.max(1.0)).min(0.45);
    *ratio = ratio.clamp(lower, 1.0 - lower);
    let offset = length * *ratio;
    let mut first = rect;
    let mut second = rect;
    let grip = if vertical {
        first.max.y = rect.min.y + offset - 3.0;
        second.min.y = rect.min.y + offset + 3.0;
        egui::Rect::from_min_max(
            egui::pos2(rect.min.x, first.max.y),
            egui::pos2(rect.max.x, second.min.y),
        )
    } else {
        first.max.x = rect.min.x + offset - 3.0;
        second.min.x = rect.min.x + offset + 3.0;
        egui::Rect::from_min_max(
            egui::pos2(first.max.x, rect.min.y),
            egui::pos2(second.min.x, rect.max.y),
        )
    };
    let response = ui
        .interact(grip, ui.id().with(id), egui::Sense::drag())
        .on_hover_cursor(if vertical {
            egui::CursorIcon::ResizeVertical
        } else {
            egui::CursorIcon::ResizeHorizontal
        });
    ui.painter()
        .rect_filled(grip, 0.0, theme::palette(ui).border);
    if response.dragged() {
        let delta = ui.input(|i| {
            if vertical {
                i.pointer.delta().y
            } else {
                i.pointer.delta().x
            }
        });
        *ratio = (*ratio + delta / length.max(1.0)).clamp(lower, 1.0 - lower);
    }
    (first, second)
}

pub fn show_plugin_popouts(
    ui: &mut Ui,
    dock: &mut DockLayoutState,
    plugins: &mut [Box<dyn MemRWPlugin>],
    pool: &mut VariablePool,
    running: bool,
    interaction_enabled: bool,
    variable_tree: &mut VariableTreePanel,
) -> Vec<PluginAction> {
    let mut actions = Vec::new();
    show_popout_viewports(
        ui,
        dock,
        plugins,
        pool,
        running,
        interaction_enabled,
        variable_tree,
        &mut actions,
    );
    actions
}

fn ensure_active_plugin(dock: &mut DockLayoutState, plugins: &[Box<dyn MemRWPlugin>]) {
    let active_is_available = dock.active_plugin_id().is_some_and(|active_id| {
        plugins
            .iter()
            .any(|plugin| plugin.id() == active_id && !dock.is_popped(plugin.id()))
    });

    if active_is_available {
        return;
    }

    dock.active_plugin = plugins
        .iter()
        .find(|plugin| !dock.is_popped(plugin.id()))
        .map(|plugin| plugin.id().to_owned());
}

fn show_activity_bar(
    ui: &mut Ui,
    dock: &mut DockLayoutState,
    plugins: &[Box<dyn MemRWPlugin>],
    side_panel_open: &mut bool,
) {
    let colors = theme::palette(ui);
    egui::Frame::NONE
        .fill(colors.sidebar_bg)
        .stroke(theme::panel_stroke(ui))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_height(ui.available_height());
            ui.add_space(4.0);
            ui.add_space(4.0);

            for plugin in plugins {
                let plugin_id = plugin.id();
                let is_hidden = dock.workspace.is_hidden(plugin_id);
                // 所有可见面板同屏显示，指示条表示“正在显示”。
                let is_active = !is_hidden;
                let is_popped = dock.is_popped(plugin_id);
                let is_paused = dock.is_plugin_paused(plugin_id);
                let response = activity_button(
                    ui,
                    plugin.as_ref(),
                    is_active,
                    is_popped,
                    is_paused,
                    is_hidden,
                );
                let button_rect = response.rect;

                if response.clicked() {
                    if is_popped {
                        dock.request_pop_in(plugin_id);
                    } else {
                        dock.toggle_plugin_visible(plugin_id);
                    }
                }

                if is_active {
                    let indicator = egui::Rect::from_min_size(
                        button_rect.left_center() - egui::vec2(0.0, 14.0),
                        egui::vec2(3.0, 28.0),
                    );
                    ui.painter()
                        .rect_filled(indicator, egui::CornerRadius::same(2), colors.accent);
                }
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                let fill = if *side_panel_open {
                    colors.accent_weak
                } else {
                    egui::Color32::TRANSPARENT
                };
                if ui
                    .add_sized(
                        [44.0, 42.0],
                        egui::Button::new(RichText::new("🔧").size(20.0))
                            .fill(fill)
                            .frame(*side_panel_open),
                    )
                    .on_hover_text("调试助手：目标状态、变量搜索、表达式、断点/调用栈、内存查看")
                    .clicked()
                {
                    *side_panel_open = !*side_panel_open;
                }
            });
        });
}

fn show_pop_in_confirmation(
    ctx: &egui::Context,
    dock: &mut DockLayoutState,
    plugins: &[Box<dyn MemRWPlugin>],
) {
    let Some(plugin_id) = dock.pending_pop_in.clone() else {
        return;
    };
    if !dock.is_popped(&plugin_id) {
        dock.pending_pop_in = None;
        return;
    }
    let title = plugins
        .iter()
        .find(|plugin| plugin.id() == plugin_id)
        .map(|plugin| plugin.title())
        .unwrap_or(plugin_id.as_str());
    let mut confirm = false;
    let mut cancel = false;
    let response = egui::Modal::new(egui::Id::new("confirm_plugin_pop_in")).show(ctx, |ui| {
        ui.set_min_width(300.0);
        ui.heading("确认返回主窗口");
        ui.label(format!("是否将“{title}”从独立窗口移回主窗口？"));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            cancel = ui.button("取消").clicked();
            confirm = ui.button("确认移回").clicked();
        });
    });
    cancel |= response.backdrop_response.clicked()
        || ctx.input(|input| input.key_pressed(egui::Key::Escape));
    if confirm {
        dock.confirm_pop_in();
    } else if cancel {
        dock.pending_pop_in = None;
    }
}

fn activity_button(
    ui: &mut Ui,
    plugin: &dyn MemRWPlugin,
    is_active: bool,
    is_popped: bool,
    is_paused: bool,
    is_hidden: bool,
) -> egui::Response {
    let colors = theme::palette(ui);
    let fill = if is_active {
        colors.accent_weak
    } else {
        egui::Color32::TRANSPARENT
    };
    let text_color = if is_paused || is_hidden {
        colors.text_muted
    } else if is_active {
        colors.accent_hover
    } else if is_popped {
        colors.text_muted
    } else {
        colors.text
    };

    ui.add_sized(
        [44.0, 42.0],
        egui::Button::new(
            RichText::new(plugin_icon(plugin.id(), plugin.title()))
                .size(20.0)
                .color(text_color),
        )
        .fill(fill)
        .frame(is_active),
    )
    .on_hover_text(if is_popped {
        format!("{} 已弹出，点击返回主区域", plugin.title())
    } else if is_hidden {
        format!("{} 已隐藏，点击显示", plugin.title())
    } else if is_paused {
        format!("{} 已暂停更新；点击隐藏面板", plugin.title())
    } else {
        format!("{} 正在显示，点击隐藏", plugin.title())
    })
}

fn plugin_icon(plugin_id: &str, title: &str) -> String {
    match plugin_id {
        "chart" => "📈".to_owned(),
        "table" => "📋".to_owned(),
        "debug" => "🐞".to_owned(),
        _ => title.chars().next().unwrap_or('□').to_string(),
    }
}

fn native_window_title(plugin_id: &str, title: &str) -> String {
    match plugin_id {
        "chart" => "图表".to_owned(),
        "table" => "变量".to_owned(),
        "debug" => "调试".to_owned(),
        _ => {
            let title = title
                .chars()
                .filter(|ch| ch.is_ascii_graphic() || ch.is_ascii_whitespace())
                .collect::<String>();
            if title.trim().is_empty() {
                plugin_id.to_owned()
            } else {
                title.trim().to_owned()
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn show_plugin_docked(
    ui: &mut Ui,
    dock: &mut DockLayoutState,
    plugin: &mut dyn MemRWPlugin,
    pool: &mut VariablePool,
    running: bool,
    interaction_enabled: bool,
    variable_tree: &mut VariableTreePanel,
    actions: &mut Vec<PluginAction>,
    can_rearrange: bool,
) {
    let colors = theme::palette(ui);
    egui::Frame::NONE
        .fill(colors.panel_bg)
        .stroke(theme::panel_stroke(ui))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_height(ui.available_height());
            let overlay_open = variable_tree.is_open_in(ui.ctx().viewport_id());
            let paused = dock.is_plugin_paused(plugin.id());
            let maximized = dock.maximized.as_deref() == Some(plugin.id());
            let controls = dock_control_bar(
                ui,
                plugin.title(),
                "独立窗口",
                paused,
                Some(DockedControls {
                    maximized,
                    can_rearrange: can_rearrange && !maximized,
                }),
            );
            if controls.drag_started {
                dock.dragging = Some(plugin.id().to_owned());
            }
            if controls.toggle_maximized {
                dock.maximized = if maximized {
                    None
                } else {
                    Some(plugin.id().into())
                };
            }
            if controls.hide {
                dock.toggle_plugin_visible(plugin.id());
            }
            if controls.toggle_paused {
                dock.toggle_plugin_paused(plugin.id());
                actions.extend(plugin.on_enabled_changed(!dock.is_plugin_paused(plugin.id())));
            }
            if controls.move_viewport {
                dock.set_popped(plugin.id(), true);
            } else {
                let paused = dock.is_plugin_paused(plugin.id());
                ui.add_enabled_ui(interaction_enabled && !overlay_open && !paused, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt((plugin.id(), "pane_scroll"))
                        .show(ui, |ui| {
                            ui.set_min_height(ui.available_height());
                            render_plugin_content(ui, plugin, pool, running, actions);
                        });
                });
            }
            variable_tree.show(ui, plugin, pool, actions);
        });
}

fn show_popout_viewports(
    ui: &mut Ui,
    dock: &mut DockLayoutState,
    plugins: &mut [Box<dyn MemRWPlugin>],
    pool: &mut VariablePool,
    running: bool,
    interaction_enabled: bool,
    variable_tree: &mut VariableTreePanel,
    actions: &mut Vec<PluginAction>,
) {
    for plugin in plugins.iter_mut() {
        if !dock.is_popped(plugin.id()) {
            continue;
        }

        let plugin_id = plugin.id().to_owned();
        let viewport_id = egui::ViewportId::from_hash_of(format!("{plugin_id}_popout_viewport"));
        let title = native_window_title(plugin.id(), plugin.title());
        let keep_popped = ui.ctx().show_viewport_immediate(
            viewport_id,
            egui::ViewportBuilder::default()
                .with_title(title)
                .with_inner_size(plugin.viewport_size())
                .with_min_inner_size(plugin.min_viewport_size())
                .with_resizable(true),
            |viewport_ui, _class| {
                if viewport_ui.ctx().input(|i| i.viewport().close_requested()) {
                    return false;
                }
                let mut pop_in = false;
                egui::CentralPanel::default().show_inside(viewport_ui, |ui| {
                    let overlay_open = variable_tree.is_open_in(ui.ctx().viewport_id());
                    let paused = dock.is_plugin_paused(plugin.id());
                    let controls = dock_control_bar(ui, plugin.title(), "移回主界面", paused, None);
                    if controls.toggle_paused {
                        dock.toggle_plugin_paused(plugin.id());
                        actions
                            .extend(plugin.on_enabled_changed(!dock.is_plugin_paused(plugin.id())));
                    }
                    if controls.move_viewport {
                        pop_in = true;
                    }
                    let paused = dock.is_plugin_paused(plugin.id());
                    ui.add_enabled_ui(interaction_enabled && !overlay_open && !paused, |ui| {
                        render_plugin_content(ui, plugin.as_mut(), pool, running, actions);
                    });
                });
                variable_tree.show(viewport_ui, plugin.as_mut(), pool, actions);
                !pop_in
            },
        );
        if !keep_popped {
            variable_tree.close_in(viewport_id);
            dock.set_popped(&plugin_id, false);
            if dock.pending_pop_in.as_deref() == Some(plugin_id.as_str()) {
                dock.pending_pop_in = None;
            }
        }
    }
}

fn render_plugin_content(
    ui: &mut Ui,
    plugin: &mut dyn MemRWPlugin,
    pool: &VariablePool,
    running: bool,
    actions: &mut Vec<PluginAction>,
) {
    #[cfg(test)]
    assert!(
        ui.available_height().is_finite(),
        "插件 {} 收到了无界高度",
        plugin.id()
    );
    actions.extend(plugin.render(
        ui,
        PluginRenderContext {
            pool,
            running,
            viewport_id: ui.ctx().viewport_id(),
        },
    ));
}

#[derive(Default)]
struct DockControlAction {
    move_viewport: bool,
    toggle_paused: bool,
    toggle_maximized: bool,
    hide: bool,
    drag_started: bool,
}

/// 仅主窗口内的面板才有的标题栏控件。
struct DockedControls {
    maximized: bool,     // 当前是否最大化
    can_rearrange: bool, // 是否允许拖动换位和隐藏
}

fn dock_control_bar(
    ui: &mut Ui,
    title: &str,
    button: &str,
    paused: bool,
    docked: Option<DockedControls>,
) -> DockControlAction {
    let colors = theme::palette(ui);
    let mut action = DockControlAction::default();
    ui.horizontal(|ui| {
        let can_drag = docked.as_ref().is_some_and(|d| d.can_rearrange);
        if can_drag {
            // 标题与左侧握把一起作为拖动区域。
            let handle = ui
                .scope(|ui| {
                    let (grip, _) =
                        ui.allocate_exact_size(egui::vec2(12.0, 18.0), egui::Sense::hover());
                    for row in 0..3 {
                        for column in 0..2 {
                            ui.painter().circle_filled(
                                grip.left_top()
                                    + egui::vec2(3.0 + column as f32 * 5.0, 4.0 + row as f32 * 5.0),
                                1.3,
                                colors.text_muted,
                            );
                        }
                    }
                    ui.label(RichText::new(title).strong().color(colors.text));
                })
                .response;
            let drag = ui
                .interact(
                    handle.rect,
                    ui.id().with("panel_drag_handle"),
                    egui::Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::Grab)
                .on_hover_text("按住拖到另一个面板上，交换两者位置");
            action.drag_started = drag.drag_started();
        } else {
            ui.label(RichText::new(title).strong().color(colors.text));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            action.move_viewport = ui.small_button(button).clicked();
            let pause_label = if paused {
                "启用插件"
            } else {
                "暂停插件"
            };
            action.toggle_paused = ui
                .small_button(pause_label)
                .on_hover_text(if paused {
                    "恢复此插件的数据更新和交互"
                } else {
                    "暂停此插件的数据更新和交互，保留当前画面"
                })
                .clicked();
            if let Some(docked) = &docked {
                if docked.can_rearrange || docked.maximized {
                    action.toggle_maximized = ui
                        .small_button(if docked.maximized {
                            "还原"
                        } else {
                            "最大化"
                        })
                        .on_hover_text("临时占满工作区；不改变已保存的布局")
                        .clicked();
                }
                if docked.can_rearrange {
                    action.hide = ui
                        .small_button("隐藏")
                        .on_hover_text("隐藏此面板；点击最左侧对应图标可重新显示")
                        .clicked();
                }
            }
            if paused {
                ui.label(RichText::new("已暂停").color(colors.warning));
            }
        });
    });
    ui.separator();
    action
}

#[cfg(test)]
mod tests {
    use super::DockLayoutState;

    #[test]
    fn plugins_are_active_by_default_and_pause_independently() {
        let mut dock = DockLayoutState::default();
        assert!(!dock.is_plugin_paused("chart"));
        assert!(!dock.is_plugin_paused("table"));

        dock.toggle_plugin_paused("chart");
        assert!(dock.is_plugin_paused("chart"));
        assert!(!dock.is_plugin_paused("table"));

        dock.toggle_plugin_paused("chart");
        assert!(!dock.is_plugin_paused("chart"));
    }

    #[test]
    fn focusing_plugin_for_variable_tree_pops_it_back_in() {
        let mut dock = DockLayoutState::default();
        dock.set_popped("table", true);

        dock.focus_plugin("table");

        assert_eq!(dock.active_plugin_id(), Some("table"));
        assert!(!dock.is_popped("table"));
    }

    #[test]
    fn activity_bar_pop_in_requires_explicit_confirmation() {
        let mut dock = DockLayoutState::default();
        dock.set_popped("debug", true);
        dock.request_pop_in("debug");

        assert!(dock.is_popped("debug"));
        assert_eq!(dock.pending_pop_in.as_deref(), Some("debug"));

        dock.confirm_pop_in();
        assert!(!dock.is_popped("debug"));
        assert_eq!(dock.active_plugin_id(), Some("debug"));
        assert!(dock.pending_pop_in.is_none());
    }
}

#[cfg(test)]
mod workspace_tests {
    use super::*;
    #[test]
    fn default_workspace_shows_chart_and_table_and_icons_toggle_panels() {
        let mut dock = DockLayoutState::default();
        assert_eq!(dock.visible_slots(), ["chart", "table"].map(String::from));
        dock.toggle_plugin_visible("debug");
        assert_eq!(dock.visible_slots().len(), 3);
        dock.maximized = Some("debug".into());
        dock.toggle_plugin_visible("debug");
        assert!(dock.maximized.is_none());
        dock.toggle_plugin_visible("chart");
        assert_eq!(dock.visible_slots(), ["table"].map(String::from));
        dock.toggle_plugin_visible("table");
        assert_eq!(dock.visible_slots(), ["table"].map(String::from));
        // 加入图表等操作会把隐藏的面板重新显示。
        dock.focus_plugin("chart");
        assert_eq!(dock.visible_slots(), ["chart", "table"].map(String::from));
        dock.set_popped("chart", true);
        assert_eq!(dock.visible_slots(), ["table"].map(String::from));
    }
    #[test]
    fn focusing_or_popping_maximized_pane_restores_layout() {
        let mut dock = DockLayoutState::default();
        dock.maximized = Some("table".into());
        dock.focus_plugin("chart");
        assert!(dock.maximized.is_none());
        dock.maximized = Some("chart".into());
        dock.set_popped("chart", true);
        assert!(dock.maximized.is_none());
    }
    #[test]
    fn all_three_plugins_render_in_both_layouts_at_small_and_large_sizes() {
        use crate::ui::{
            chart_plugin::ChartPluginState, debug_plugin::DebugPluginState,
            table_plugin::TablePluginState,
        };
        let context = egui::Context::default();
        crate::ui::theme::install(&context);
        let mut dock = DockLayoutState::default();
        dock.workspace.hidden.clear();
        let mut pool = VariablePool::default();
        let mut entries = Vec::new();
        let mut chart = ChartPluginState::default();
        for i in 0..132 {
            let name = format!("motor.parameters.long_parameter_name_{i}");
            let address = 0x20000000 + i * 4;
            let id = pool.add(&crate::dwarf::types::ExtendConfig {
                name: name.clone(),
                address,
                ext_type: crate::dwarf::types::ExtendType::Float,
                size: 4,
                array_index: None,
                array_count: None,
            });
            entries.push(serde_json::json!({"variable_name":name,"variable_address":address,"display_name":format!("parameter_{i} | 中文长注释：用于核对参数和说明分列，支持横向滚动完整查看")}));
            if i < 3 {
                chart
                    .add_chart_variables(
                        &[(id, format!("curve_{i}"), "模拟数据".into())],
                        &mut pool,
                    )
                    .unwrap();
                for point in 0..120_000 {
                    chart.legends.last_mut().unwrap().push_prepared(
                        point as f64 * 0.001,
                        if point == 60_001 {
                            999.0
                        } else {
                            (point as f64 * 0.01).sin()
                        },
                    );
                }
            }
        }
        let mut table = TablePluginState::default();
        table
            .load_config(&serde_json::Value::Array(entries), &mut pool)
            .unwrap();
        let mut plugins: Vec<Box<dyn MemRWPlugin>> = vec![
            Box::new(chart),
            Box::new(table),
            Box::new(DebugPluginState::default()),
        ];
        let mut tree = VariableTreePanel::new(crate::dwarf::types::DwarfState::new(vec![]));
        for size in [
            egui::vec2(800.0, 500.0),
            egui::vec2(1280.0, 720.0),
            egui::vec2(1920.0, 1080.0),
        ] {
            use crate::ui::workspace::Arrangement;
            for top in [
                Arrangement::MainTop,
                Arrangement::MainLeft,
                Arrangement::Columns,
                Arrangement::Rows,
            ] {
                dock.workspace.set_arrangement(top);
                let output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |ui| {
                        let actions = show_active_plugin_content(
                            ui,
                            &mut dock,
                            &mut plugins,
                            &mut pool,
                            false,
                            true,
                            &mut tree,
                        );
                        assert!(actions.is_empty());
                    },
                );
                let texts = output
                    .shapes
                    .iter()
                    .filter_map(|shape| {
                        if let egui::epaint::Shape::Text(text) = &shape.shape {
                            Some(text.galley.job.text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                for title in ["图表", "变量", "调试"] {
                    assert!(
                        texts.contains(&title),
                        "missing pane {title} at {size:?}, arrangement={top:?}"
                    );
                }
            }
        }
    }
}
