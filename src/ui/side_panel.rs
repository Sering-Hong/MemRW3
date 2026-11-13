//! 左侧“调试助手”栏：目标状态、变量快速搜索、表达式 Watch、断点/调用栈摘要和内存查看。
//!
//! 本模块只读取应用已有的数据并产生 `PluginAction`，不直接访问探针。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::model::{
    BreakpointSpec, DebugSnapshot, RegisterData, RegisterReadRequest, TargetState, VariablePool,
};
use crate::ui::debug_tools::{NumberFormat, format_number, raw_to_f64};
use crate::ui::expr::{self, Expr};
use crate::ui::plugin::{FrameData, PluginAction};
use crate::ui::theme;

pub const DEFAULT_WIDTH: f32 = 300.0;
pub const WIDTH_RANGE: std::ops::RangeInclusive<f32> = 220.0..=560.0;
const MAX_SEARCH_RESULTS: usize = 60;
const MAX_EXPRESSIONS: usize = 32;
/// 内存读取请求 ID 的高 16 位标记，避免与 SVD 寄存器 ID 冲突。
const MEMORY_ID_TAG: u64 = 0x4D52;
const MEMORY_MAX_BYTES: u32 = 256;
const MEMORY_AUTO_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusLevel {
    Idle, // 未连接或空闲
    Good, // 正常运行
    Warn, // 已暂停或进行中
    Bad,  // 故障
}

/// 判定目标状态所需的输入，全部来自应用已有状态。
pub struct StatusInput<'a> {
    pub connected: bool,               // 探针已连接
    pub connection_pending: bool,      // 正在连接
    pub flashing: bool,                // 正在烧录
    pub acquisition_requested: bool,   // 使用者要求采集
    pub running: bool,                 // 实际正在采集
    pub sampling_hz: f64,              // 采样率，Hz
    pub debug_active: bool,            // 调试器已启动
    pub target_state: &'a TargetState, // 调试器观察到的内核状态
}

/// 返回状态标题、补充说明和显示等级。
pub fn target_status(input: &StatusInput<'_>) -> (String, String, StatusLevel) {
    if input.flashing {
        return (
            "烧录中".into(),
            "请保持连接，完成后目标自动复位".into(),
            StatusLevel::Warn,
        );
    }
    if !input.connected {
        return if input.connection_pending {
            ("连接中…".into(), String::new(), StatusLevel::Warn)
        } else {
            (
                "未连接".into(),
                "点击上方“连接”后才能读取 MCU".into(),
                StatusLevel::Idle,
            )
        };
    }
    // NOTE: 内核状态只有调试器启动后才被查询，否则只能报告采集状态。
    if input.debug_active {
        match input.target_state {
            TargetState::Halted { reason } => {
                return (
                    "已暂停（Halt）".into(),
                    format!("原因：{reason}；曲线停止更新，变量约 1 Hz 刷新"),
                    StatusLevel::Warn,
                );
            }
            TargetState::LockedUp => {
                return (
                    "内核 LockUp".into(),
                    "MCU 进入锁死状态，通常由 Fault 中再次出错引起".into(),
                    StatusLevel::Bad,
                );
            }
            _ => {}
        }
    }
    if input.running {
        let debug = if input.debug_active {
            "；调试器已启动"
        } else {
            ""
        };
        (
            "运行中 · 采集中".into(),
            format!("采样率 {:.1} Hz{debug}", input.sampling_hz),
            StatusLevel::Good,
        )
    } else if input.acquisition_requested {
        (
            "已连接 · 采集暂停".into(),
            "已请求采集但当前没有数据，检查目标是否暂停".into(),
            StatusLevel::Warn,
        )
    } else {
        (
            "已连接 · 未采集".into(),
            "点击上方“开始”后变量和曲线才会更新".into(),
            StatusLevel::Idle,
        )
    }
}

/// 随调参配置保存的侧栏内容。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedSidePanel {
    pub expressions: Vec<String>, // 表达式 Watch 列表
    pub memory_address: String,   // 内存查看的起始地址或变量名
    pub memory_bytes: u32,        // 内存查看长度，字节
}

struct WatchExpression {
    source: String,               // 使用者输入的表达式
    parsed: Result<Expr, String>, // 解析结果或错误说明
}

impl WatchExpression {
    fn new(source: String) -> Self {
        let parsed = expr::parse(&source);
        Self { source, parsed }
    }
}

struct MemoryView {
    address_text: String,                    // 起始地址或变量名
    bytes: u32,                              // 读取长度，4–256 字节
    auto_refresh: bool,                      // 是否约 2 Hz 自动读取
    epoch: u16,                              // 地址或长度变化的代次
    base: u64,                               // 已请求的对齐起始地址
    words: Vec<Option<Result<u32, String>>>, // 各 32 位字的读取结果
    requested_at: Option<Instant>,           // 最近一次请求时间
    seen_sequence: u64,                      // 已显示的最新结果序号
    updated_at: Option<Instant>,             // 最近一次收到结果的时间
    error: Option<String>,                   // 地址解析等输入错误
}

impl Default for MemoryView {
    fn default() -> Self {
        Self {
            address_text: "0x20000000".into(),
            bytes: 64,
            auto_refresh: false,
            epoch: 0,
            base: 0,
            words: Vec::new(),
            requested_at: None,
            seen_sequence: 0,
            updated_at: None,
            error: None,
        }
    }
}

fn memory_id(epoch: u16, index: usize) -> u64 {
    (MEMORY_ID_TAG << 48) | (u64::from(epoch) << 16) | index as u64
}

/// 解析内存地址：十六进制、十进制，或已加载变量的名称。
pub fn parse_address(text: &str, pool: &VariablePool) -> Result<u64, String> {
    let text = text.trim().replace('_', "");
    if text.is_empty() {
        return Err("请输入地址或变量名".into());
    }
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).map_err(|_| "无效的十六进制地址".to_owned());
    }
    if text.chars().all(|c| c.is_ascii_digit()) {
        return text.parse::<u64>().map_err(|_| "地址超出范围".to_owned());
    }
    expr::resolve_name(&text, pool.iter().map(|v| (v.name.as_str(), v.address)))
}

/// 把 32 位小端字展开成十六进制转储行，每行 8 字节。
pub fn hex_dump_lines(base: u64, words: &[Option<Result<u32, String>>]) -> Vec<String> {
    let mut bytes: Vec<Option<u8>> = Vec::with_capacity(words.len() * 4);
    for word in words {
        match word {
            Some(Ok(value)) => bytes.extend(value.to_le_bytes().map(Some)),
            _ => bytes.extend([None; 4]),
        }
    }
    bytes
        .chunks(8)
        .enumerate()
        .map(|(row, chunk)| {
            let mut line = format!("{:08X} ", base + row as u64 * 8);
            let mut ascii = String::new();
            for byte in chunk {
                match byte {
                    Some(byte) => {
                        line.push_str(&format!(" {byte:02X}"));
                        ascii.push(if byte.is_ascii_graphic() || *byte == b' ' {
                            *byte as char
                        } else {
                            '.'
                        });
                    }
                    None => {
                        line.push_str(" ??");
                        ascii.push('?');
                    }
                }
            }
            format!("{line}  {ascii}")
        })
        .collect()
}

pub struct SidePanelState {
    pub open: bool,                                 // 侧栏是否展开
    pub width: f32,                                 // 侧栏宽度，像素
    search: String,                                 // 变量搜索关键字
    expressions: Vec<WatchExpression>,              // 表达式 Watch 列表
    new_expression: String,                         // 待添加的表达式输入
    values: HashMap<usize, (Option<u64>, [u8; 8])>, // 变量最近的采样原始值
    memory: MemoryView,                             // 内存查看状态
}

impl Default for SidePanelState {
    fn default() -> Self {
        let prefs = crate::ui::prefs::get();
        Self {
            open: prefs.side_panel_open.unwrap_or(true),
            width: prefs
                .side_panel_width
                .filter(|w| w.is_finite())
                .unwrap_or(DEFAULT_WIDTH)
                .clamp(*WIDTH_RANGE.start(), *WIDTH_RANGE.end()),
            search: String::new(),
            expressions: Vec::new(),
            new_expression: String::new(),
            values: HashMap::new(),
            memory: MemoryView::default(),
        }
    }
}

/// 渲染侧栏所需的只读上下文。
pub struct SideContext<'a> {
    pub pool: &'a VariablePool,                     // 已加载变量
    pub annotations: &'a [(usize, String, String)], // 变量 ID、显示名、中文注释
    pub status: StatusInput<'a>,                    // 目标状态输入
    pub snapshot: &'a DebugSnapshot,                // 调试器快照
    pub elf_path: &'a str,                          // 当前 ELF 路径
    pub chip: &'a str,                              // 目标芯片型号
}

#[derive(Default)]
pub struct SideOutput {
    pub actions: Vec<PluginAction>,       // 交给应用执行的动作
    pub show_panel: Option<&'static str>, // 需要显示的面板 ID
}

impl SidePanelState {
    pub fn to_saved(&self) -> SavedSidePanel {
        SavedSidePanel {
            expressions: self.expressions.iter().map(|e| e.source.clone()).collect(),
            memory_address: self.memory.address_text.clone(),
            memory_bytes: self.memory.bytes,
        }
    }

    pub fn load_saved(&mut self, saved: SavedSidePanel) {
        self.expressions = saved
            .expressions
            .into_iter()
            .filter(|source| !source.trim().is_empty())
            .take(MAX_EXPRESSIONS)
            .map(WatchExpression::new)
            .collect();
        let defaults = MemoryView::default();
        self.memory = MemoryView {
            address_text: if saved.memory_address.trim().is_empty() {
                defaults.address_text
            } else {
                saved.memory_address
            },
            bytes: if saved.memory_bytes == 0 {
                defaults.bytes
            } else {
                saved.memory_bytes.clamp(4, MEMORY_MAX_BYTES) / 4 * 4
            },
            // 换配置后旧结果作废。
            epoch: self.memory.epoch.wrapping_add(1),
            ..MemoryView::default()
        };
        self.values.clear();
    }

    /// 应用据此清理已不再显示的内存读取结果。
    pub fn is_stale_memory_id(&self, id: u64) -> bool {
        id >> 48 == MEMORY_ID_TAG && (id >> 16) & 0xFFFF != u64::from(self.memory.epoch)
    }

    /// 每帧调用：缓存变量最新值并收取内存读取结果。
    pub fn update(
        &mut self,
        pool: &VariablePool,
        frame_data: &FrameData,
        registers: &RegisterData,
    ) {
        self.values.retain(|id, _| pool.contains(*id));
        for variable in pool.iter() {
            let latest = variable.latest.load();
            let stream = frame_data
                .get(&variable.id)
                .and_then(|batch| batch.last())
                .map(|(_, bytes)| *bytes);
            let entry = self.values.get(&variable.id).copied();
            let seen = entry.and_then(|(sequence, _)| sequence);
            if let Some((sequence, bytes)) = latest {
                // 流数据比同一帧里未变化的 Latest 更新。
                if seen != Some(sequence) || (entry.is_none() && stream.is_none()) {
                    self.values.insert(variable.id, (Some(sequence), bytes));
                    continue;
                }
            }
            if let Some(bytes) = stream {
                self.values.insert(variable.id, (seen, bytes));
            }
        }

        let memory = &mut self.memory;
        let mut newest = memory.seen_sequence;
        for (index, word) in memory.words.iter_mut().enumerate() {
            if let Some(result) = registers.get(&memory_id(memory.epoch, index)) {
                newest = newest.max(result.sequence);
                *word = Some(result.value.clone().map(|value| value as u32));
            }
        }
        if newest != memory.seen_sequence {
            memory.seen_sequence = newest;
            memory.updated_at = Some(Instant::now());
        }
    }

    fn value_of(&self, pool: &VariablePool, id: usize) -> Option<f64> {
        let (_, raw) = self.values.get(&id)?;
        raw_to_f64(*raw, &pool.get(id)?.ext_type)
    }

    fn value_text(&self, pool: &VariablePool, id: usize) -> String {
        match (self.values.get(&id), pool.get(id)) {
            (Some((_, raw)), Some(variable)) => {
                format_number(*raw, &variable.ext_type, NumberFormat::Decimal)
            }
            _ => "未采样".into(),
        }
    }

    fn evaluate(&self, expression: &WatchExpression, pool: &VariablePool) -> Result<f64, String> {
        let parsed = expression.parsed.as_ref().map_err(Clone::clone)?;
        parsed.eval(&|name| {
            let id = expr::resolve_name(name, pool.iter().map(|v| (v.name.as_str(), v.id)))?;
            self.value_of(pool, id)
                .ok_or_else(|| format!("{name} 尚无采样（需在变量表勾选采集或加入图表）"))
        })
    }

    pub fn show(&mut self, ui: &mut Ui, ctx: SideContext<'_>) -> SideOutput {
        let mut output = SideOutput::default();
        let colors = theme::palette(ui);
        egui::Frame::NONE
            .fill(colors.sidebar_bg)
            .stroke(theme::panel_stroke(ui))
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("调试助手").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("收起")
                            .on_hover_text("点击最左侧扳手图标可重新打开")
                            .clicked()
                        {
                            self.open = false;
                        }
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt("side_panel_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.status_section(ui, &ctx);
                        egui::CollapsingHeader::new("变量快速搜索")
                            .default_open(true)
                            .show(ui, |ui| self.search_section(ui, &ctx, &mut output));
                        egui::CollapsingHeader::new("表达式 Watch")
                            .default_open(true)
                            .show(ui, |ui| self.expression_section(ui, &ctx));
                        egui::CollapsingHeader::new("断点与调用栈")
                            .default_open(false)
                            .show(ui, |ui| debug_section(ui, &ctx, &mut output));
                        egui::CollapsingHeader::new("内存查看")
                            .default_open(false)
                            .show(ui, |ui| self.memory_section(ui, &ctx, &mut output));
                        egui::CollapsingHeader::new("其他调试视图在哪")
                            .default_open(false)
                            .show(ui, |ui| {
                                ui.label("· 外设寄存器（SVD）：变量面板勾选“显示 SVD 寄存器”");
                                ui.label("· CPU 寄存器、局部变量、源码/汇编、单步：调试面板");
                                ui.label("· 参数修改记录：工作区工具栏");
                                if ui.button("显示调试面板").clicked() {
                                    output.show_panel = Some("debug");
                                }
                            });
                    });
            });
        output
    }

    fn status_section(&self, ui: &mut Ui, ctx: &SideContext<'_>) {
        let colors = theme::palette(ui);
        let (title, detail, level) = target_status(&ctx.status);
        let color = match level {
            StatusLevel::Idle => colors.text_muted,
            StatusLevel::Good => colors.success,
            StatusLevel::Warn => colors.warning,
            StatusLevel::Bad => colors.danger,
        };
        egui::Frame::NONE
            .fill(colors.surface_bg)
            .stroke(egui::Stroke::new(1.5, color))
            .corner_radius(4)
            .inner_margin(egui::Margin::symmetric(8, 6))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(14.0, 18.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 5.5, color);
                    ui.label(RichText::new(title).size(16.0).strong().color(color));
                });
                if !detail.is_empty() {
                    ui.add(egui::Label::new(RichText::new(detail).size(11.5)).wrap());
                }
                let snapshot = ctx.snapshot;
                if ctx.status.connected && snapshot.active && snapshot.target_state.is_halted() {
                    if let Some(pc) = snapshot.pc {
                        ui.monospace(format!("PC = 0x{pc:08X}"));
                    }
                    if let Some(frame) = snapshot.frames.first() {
                        ui.add(egui::Label::new(format!("位置：{}", frame_text(frame))).wrap());
                    }
                }
                let elf = std::path::Path::new(ctx.elf_path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "未加载".into());
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "芯片 {} · ELF {} · {} 个变量",
                            ctx.chip,
                            elf,
                            ctx.pool.iter().count()
                        ))
                        .size(11.0)
                        .color(colors.text_muted),
                    )
                    .wrap(),
                )
                .on_hover_text(ctx.elf_path);
            });
        ui.add_space(4.0);
    }

    fn search_section(&mut self, ui: &mut Ui, ctx: &SideContext<'_>, output: &mut SideOutput) {
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("名称 / 中文注释 / 0x地址")
                .desired_width(f32::INFINITY),
        );
        let query = self.search.trim().to_lowercase();
        if query.is_empty() {
            ui.label(
                RichText::new("在已加载的变量里按名称或注释查找，可直接加入图表。")
                    .size(11.0)
                    .color(theme::muted_text(ui)),
            );
            if ui
                .button("从 ELF 添加新变量…")
                .on_hover_text("打开变量树，把固件里的全局变量加入变量表")
                .clicked()
            {
                output.show_panel = Some("table");
                output.actions.push(PluginAction::OpenVariableTree {
                    plugin_id: "table".into(),
                    viewport_id: egui::ViewportId::ROOT,
                });
            }
            return;
        }
        let notes: HashMap<usize, (&str, &str)> = ctx
            .annotations
            .iter()
            .map(|(id, label, comment)| (*id, (label.as_str(), comment.as_str())))
            .collect();
        let mut shown = 0;
        let mut total = 0;
        for variable in ctx.pool.iter() {
            let (label, comment) = notes.get(&variable.id).copied().unwrap_or(("", ""));
            if !(variable.name.to_lowercase().contains(&query)
                || label.to_lowercase().contains(&query)
                || comment.to_lowercase().contains(&query)
                || format!("0x{:08x}", variable.address).contains(&query))
            {
                continue;
            }
            total += 1;
            if shown >= MAX_SEARCH_RESULTS {
                continue;
            }
            shown += 1;
            ui.push_id(("search_result", variable.id), |ui| {
                ui.add(egui::Label::new(RichText::new(&variable.name).strong()).wrap())
                    .on_hover_text(format!("地址 0x{:08X}", variable.address));
                if !comment.is_empty() {
                    ui.add(
                        egui::Label::new(
                            RichText::new(comment)
                                .size(11.5)
                                .color(theme::muted_text(ui)),
                        )
                        .wrap(),
                    );
                }
                ui.horizontal(|ui| {
                    ui.monospace(self.value_text(ctx.pool, variable.id));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("复制")
                            .on_hover_text("复制完整变量路径")
                            .clicked()
                        {
                            ui.ctx().copy_text(variable.name.clone());
                        }
                        if ui.small_button("加入图表").clicked() {
                            let short = variable
                                .name
                                .rsplit('.')
                                .next()
                                .unwrap_or(&variable.name)
                                .to_owned();
                            output.actions.push(PluginAction::AddToChart {
                                variables: vec![(variable.id, short, comment.to_owned())],
                            });
                        }
                    });
                });
                ui.separator();
            });
        }
        if total == 0 {
            ui.label("没有匹配的已加载变量");
        } else if total > shown {
            ui.label(format!("共 {total} 项，只显示前 {shown} 项，请细化关键字"));
        }
    }

    fn expression_section(&mut self, ui: &mut Ui, ctx: &SideContext<'_>) {
        let mut remove = None;
        for (index, expression) in self.expressions.iter().enumerate() {
            ui.push_id(("watch_expression", index), |ui| {
                ui.horizontal(|ui| {
                    if ui.small_button("×").on_hover_text("删除此表达式").clicked() {
                        remove = Some(index);
                    }
                    ui.add(egui::Label::new(RichText::new(&expression.source).monospace()).wrap());
                });
                match self.evaluate(expression, ctx.pool) {
                    Ok(value) => {
                        ui.label(
                            RichText::new(format!("= {}", format_result(value)))
                                .monospace()
                                .strong()
                                .color(theme::palette(ui).accent_hover),
                        );
                    }
                    Err(error) => {
                        ui.add(
                            egui::Label::new(
                                RichText::new(error)
                                    .size(11.5)
                                    .color(theme::palette(ui).warning),
                            )
                            .wrap(),
                        );
                    }
                }
                ui.separator();
            });
        }
        if let Some(index) = remove {
            self.expressions.remove(index);
        }
        let mut add = false;
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.new_expression)
                    .hint_text("如 target - speed")
                    .desired_width((ui.available_width() - 48.0).max(80.0)),
            );
            add = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            add |= ui.button("添加").clicked();
        });
        if add && !self.new_expression.trim().is_empty() {
            if self.expressions.len() >= MAX_EXPRESSIONS {
                self.new_expression = format!("最多 {MAX_EXPRESSIONS} 条");
            } else {
                let source = std::mem::take(&mut self.new_expression);
                self.expressions
                    .push(WatchExpression::new(source.trim().to_owned()));
            }
        }
        ui.label(
            RichText::new(
                "用已加载变量做 + - * / 运算，支持 abs/sqrt/min/max；变量名可写完整路径或唯一的末尾字段。结果按主机收到的最新值计算，各变量不保证同一采样时刻。随“保存配置”一起保存。",
            )
            .size(11.0)
            .color(theme::muted_text(ui)),
        );
    }

    fn memory_section(&mut self, ui: &mut Ui, ctx: &SideContext<'_>, output: &mut SideOutput) {
        let memory = &mut self.memory;
        ui.horizontal(|ui| {
            ui.label("地址");
            ui.add(
                egui::TextEdit::singleline(&mut memory.address_text)
                    .hint_text("0x20000000 或变量名")
                    .desired_width(f32::INFINITY),
            );
        });
        ui.horizontal(|ui| {
            ui.label("长度");
            ui.add(
                egui::DragValue::new(&mut memory.bytes)
                    .range(4..=MEMORY_MAX_BYTES)
                    .speed(4)
                    .suffix(" 字节"),
            );
            ui.checkbox(&mut memory.auto_refresh, "自动刷新")
                .on_hover_text("连接后约每 0.5 秒读取一次");
        });
        memory.bytes = memory.bytes.clamp(4, MEMORY_MAX_BYTES).div_ceil(4) * 4;
        let ready = ctx.status.connected && !ctx.status.flashing;
        let clicked = ui
            .add_enabled(ready, egui::Button::new("读取一次"))
            .on_disabled_hover_text("连接目标后才能读取内存")
            .clicked();
        let auto_due = memory.auto_refresh
            && ready
            && memory
                .requested_at
                .is_none_or(|at| at.elapsed() >= MEMORY_AUTO_INTERVAL);
        if clicked || auto_due {
            match parse_address(&memory.address_text, ctx.pool) {
                Ok(address) => {
                    // 按 32 位字对齐读取。
                    let base = address & !3;
                    let count = (memory.bytes / 4) as usize;
                    if base != memory.base || count != memory.words.len() {
                        memory.epoch = memory.epoch.wrapping_add(1);
                        memory.base = base;
                        memory.words = vec![None; count];
                        memory.updated_at = None;
                    }
                    memory.error = None;
                    memory.requested_at = Some(Instant::now());
                    output.actions.push(PluginAction::ReadRegisters {
                        requests: (0..count)
                            .map(|index| RegisterReadRequest {
                                id: memory_id(memory.epoch, index),
                                address: base + index as u64 * 4,
                                size_bytes: 4,
                            })
                            .collect(),
                    });
                }
                Err(error) => {
                    memory.error = Some(error);
                    memory.auto_refresh = false;
                }
            }
        }
        if memory.auto_refresh && ready {
            ui.ctx().request_repaint_after(MEMORY_AUTO_INTERVAL);
        }
        if let Some(error) = &memory.error {
            ui.colored_label(theme::palette(ui).warning, error);
        }
        if !memory.words.is_empty() {
            match memory.updated_at {
                Some(at) => ui.label(
                    RichText::new(format!("{:.1} 秒前读取", at.elapsed().as_secs_f32()))
                        .size(11.0)
                        .color(theme::muted_text(ui)),
                ),
                None => ui.label(
                    RichText::new("已请求，等待结果…")
                        .size(11.0)
                        .color(theme::muted_text(ui)),
                ),
            };
            egui::ScrollArea::horizontal()
                .id_salt("memory_dump_scroll")
                .show(ui, |ui| {
                    for line in hex_dump_lines(memory.base, &memory.words) {
                        ui.label(RichText::new(line).monospace().size(12.0));
                    }
                });
            if let Some(error) = memory.words.iter().find_map(|word| match word {
                Some(Err(error)) => Some(error),
                _ => None,
            }) {
                ui.add(
                    egui::Label::new(RichText::new(error).color(theme::palette(ui).warning)).wrap(),
                );
            }
        }
        ui.label(
            RichText::new(
                "只读，按 32 位小端字读取后逐字节显示。WARNING：读外设寄存器地址可能有读副作用（如读数据寄存器清标志），RAM 和 Flash 地址没有这个问题。",
            )
            .size(11.0)
            .color(theme::muted_text(ui)),
        );
    }
}

fn format_result(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.0}")
    } else {
        format!("{value:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn frame_text(frame: &crate::model::StackFrameView) -> String {
    match &frame.source {
        Some(source) => format!(
            "{}  {}:{}",
            frame.function,
            file_name(&source.path),
            source.line.map(|l| l.to_string()).unwrap_or_default()
        ),
        None => format!("{}  0x{:08X}", frame.function, frame.pc),
    }
}

fn debug_section(ui: &mut Ui, ctx: &SideContext<'_>, output: &mut SideOutput) {
    let snapshot = ctx.snapshot;
    let colors = theme::palette(ui);
    if !snapshot.active {
        ui.add(
            egui::Label::new(
                RichText::new("调试器未启动。断点和调用栈要在调试面板点“启动调试”后才有内容。")
                    .size(11.5)
                    .color(colors.text_muted),
            )
            .wrap(),
        );
    }
    let capacity = snapshot
        .breakpoint_capacity
        .map(|n| format!(" / 硬件上限 {n}"))
        .unwrap_or_default();
    ui.label(RichText::new(format!("断点 {}{capacity}", snapshot.breakpoints.len())).strong());
    for breakpoint in &snapshot.breakpoints {
        let location = match &breakpoint.spec {
            BreakpointSpec::Source { path, line, .. } => format!("{}:{line}", file_name(path)),
            BreakpointSpec::Instruction { address } => format!("0x{address:08X}"),
        };
        let (mark, color) = if !breakpoint.enabled {
            ("停用", colors.text_muted)
        } else if breakpoint.verified {
            ("生效", colors.success)
        } else {
            ("未生效", colors.warning)
        };
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(color, mark);
            ui.monospace(location);
        })
        .response
        .on_hover_text(breakpoint.message.clone().unwrap_or_default());
    }
    ui.add_space(4.0);
    ui.label(RichText::new("调用栈").strong());
    if snapshot.frames.is_empty() {
        ui.label(
            RichText::new("目标暂停后显示")
                .size(11.5)
                .color(colors.text_muted),
        );
    }
    for frame in &snapshot.frames {
        let text = format!("#{} {}", frame.index, frame_text(frame));
        let text = if frame.index == snapshot.selected_frame {
            RichText::new(text).monospace().color(colors.accent_hover)
        } else {
            RichText::new(text).monospace()
        };
        ui.add(egui::Label::new(text.size(12.0)).wrap());
    }
    if ui
        .button("在调试面板中操作")
        .on_hover_text("增删断点、单步、切换栈帧都在调试面板完成")
        .clicked()
    {
        output.show_panel = Some("debug");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dwarf::types::{ExtendConfig, ExtendType};
    use crate::model::RegisterReadResult;

    fn status(
        connected: bool,
        running: bool,
        debug_active: bool,
        target_state: &TargetState,
    ) -> (String, String, StatusLevel) {
        target_status(&StatusInput {
            connected,
            connection_pending: false,
            flashing: false,
            acquisition_requested: running,
            running,
            sampling_hz: 1000.0,
            debug_active,
            target_state,
        })
    }

    #[test]
    fn target_status_distinguishes_disconnected_running_and_halted() {
        let halted = TargetState::Halted {
            reason: "断点".into(),
        };
        assert_eq!(
            status(false, false, false, &TargetState::Unknown).0,
            "未连接"
        );
        assert_eq!(
            status(true, false, false, &TargetState::Unknown).0,
            "已连接 · 未采集"
        );
        let running = status(true, true, false, &TargetState::Unknown);
        assert_eq!(running.2, StatusLevel::Good);
        assert!(running.1.contains("1000.0 Hz"));
        let stopped = status(true, false, true, &halted);
        assert_eq!(stopped.0, "已暂停（Halt）");
        assert!(stopped.1.contains("断点"));
        // 调试器未启动时的旧快照不能当作当前状态。
        assert_ne!(status(true, true, false, &halted).0, "已暂停（Halt）");
        assert_eq!(
            status(true, true, true, &TargetState::LockedUp).2,
            StatusLevel::Bad
        );
    }

    fn pool_with(names: &[(&str, u64, ExtendType)]) -> (VariablePool, Vec<usize>) {
        let mut pool = VariablePool::default();
        let ids = names
            .iter()
            .map(|(name, address, ext_type)| {
                pool.add(&ExtendConfig {
                    name: (*name).into(),
                    address: *address,
                    ext_type: ext_type.clone(),
                    size: 4,
                    array_index: None,
                    array_count: None,
                })
            })
            .collect();
        (pool, ids)
    }

    #[test]
    fn expressions_use_latest_and_stream_values_and_report_missing_samples() {
        let (pool, ids) = pool_with(&[
            ("axis.target", 0x2000_0000, ExtendType::Float),
            ("axis.speed", 0x2000_0004, ExtendType::I16),
        ]);
        let mut side = SidePanelState::default();
        let expression = WatchExpression::new("target - speed".into());
        assert!(
            side.evaluate(&expression, &pool)
                .unwrap_err()
                .contains("尚无采样")
        );
        pool.get(ids[0])
            .unwrap()
            .latest
            .store(u64::from(100.0f32.to_bits()).to_le_bytes());
        let mut frame = FrameData::default();
        frame.insert(ids[1], vec![(0.0, (-10i16 as u16 as u64).to_le_bytes())]);
        side.update(&pool, &frame, &RegisterData::default());
        assert_eq!(side.evaluate(&expression, &pool).unwrap(), 110.0);
        // 没有新数据时保留上次的值。
        side.update(&pool, &FrameData::default(), &RegisterData::default());
        assert_eq!(side.evaluate(&expression, &pool).unwrap(), 110.0);
        assert_eq!(side.value_text(&pool, ids[1]), "-10");
        assert!(
            side.evaluate(&WatchExpression::new("1 +".into()), &pool)
                .is_err()
        );
    }

    #[test]
    fn saved_side_panel_roundtrips_and_old_configs_default() {
        let mut side = SidePanelState::default();
        side.load_saved(serde_json::from_str("{}").unwrap());
        assert!(side.expressions.is_empty());
        assert_eq!(side.memory.bytes, 64);
        side.load_saved(SavedSidePanel {
            expressions: vec!["a - b".into(), "  ".into()],
            memory_address: "g_tune".into(),
            memory_bytes: 1000,
        });
        let saved = side.to_saved();
        assert_eq!(saved.expressions, ["a - b"]);
        assert_eq!(saved.memory_address, "g_tune");
        assert_eq!(saved.memory_bytes, 256);
    }

    #[test]
    fn memory_addresses_parse_and_results_map_to_hex_dump() {
        let (pool, _) = pool_with(&[("g_tune.enable", 0x2000_0102, ExtendType::U8)]);
        assert_eq!(parse_address("0x2000_0000", &pool), Ok(0x2000_0000));
        assert_eq!(parse_address("536870912", &pool), Ok(0x2000_0000));
        assert_eq!(parse_address("enable", &pool), Ok(0x2000_0102));
        assert!(parse_address("0xZZ", &pool).is_err());
        assert!(parse_address("nothing", &pool).is_err());

        let mut side = SidePanelState::default();
        side.memory.epoch = 7;
        side.memory.base = 0x2000_0100;
        side.memory.words = vec![None; 3];
        let mut registers = RegisterData::default();
        registers.insert(
            memory_id(7, 0),
            RegisterReadResult {
                sequence: 3,
                value: Ok(0x4443_4241),
            },
        );
        registers.insert(
            memory_id(7, 1),
            RegisterReadResult {
                sequence: 3,
                value: Err("读取失败".into()),
            },
        );
        // 旧代次的结果不能混入当前显示。
        registers.insert(
            memory_id(6, 2),
            RegisterReadResult {
                sequence: 2,
                value: Ok(1),
            },
        );
        side.update(&pool, &FrameData::default(), &registers);
        assert!(side.memory.updated_at.is_some());
        assert!(side.is_stale_memory_id(memory_id(6, 2)));
        assert!(!side.is_stale_memory_id(memory_id(7, 0)));
        assert!(!side.is_stale_memory_id(42), "SVD 寄存器 ID 不受影响");
        let lines = hex_dump_lines(side.memory.base, &side.memory.words);
        assert_eq!(lines[0], "20000100  41 42 43 44 ?? ?? ?? ??  ABCD????");
        assert_eq!(lines[1], "20000108  ?? ?? ?? ??  ????");
    }

    #[test]
    fn side_panel_renders_all_sections_without_hardware() {
        let (pool, ids) = pool_with(&[("axis.speed", 0x2000_0004, ExtendType::Float)]);
        let context = egui::Context::default();
        crate::ui::theme::install(&context);
        let mut side = SidePanelState::default();
        side.search = "速度".into();
        side.load_saved(SavedSidePanel {
            expressions: vec!["speed * 2".into()],
            ..Default::default()
        });
        side.search = "速度".into();
        let annotations = vec![(ids[0], "speed".to_owned(), "实际速度 rpm".to_owned())];
        let snapshot = DebugSnapshot::default();
        let mut actions = 0;
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(320.0, 900.0),
                )),
                ..Default::default()
            },
            |ui| {
                let result = side.show(
                    ui,
                    SideContext {
                        pool: &pool,
                        annotations: &annotations,
                        status: StatusInput {
                            connected: false,
                            connection_pending: false,
                            flashing: false,
                            acquisition_requested: false,
                            running: false,
                            sampling_hz: 0.0,
                            debug_active: false,
                            target_state: &snapshot.target_state,
                        },
                        snapshot: &snapshot,
                        elf_path: "C:/fw/app.axf",
                        chip: "STM32H723VGTx",
                    },
                );
                actions += result.actions.len();
            },
        );
        assert_eq!(actions, 0, "未操作时不应产生任何硬件动作");
        let texts: Vec<String> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.job.text.clone()),
                _ => None,
            })
            .collect();
        for expected in ["未连接", "axis.speed", "实际速度 rpm", "speed * 2"] {
            assert!(
                texts.iter().any(|text| text.contains(expected)),
                "侧栏缺少 {expected}：{texts:?}"
            );
        }
    }
}
