use crate::dwarf::types::ExtendType;
use eframe::egui;
use std::io::Write;

#[derive(Clone, Copy, Default, PartialEq)]
pub enum NumberFormat {
    #[default]
    Decimal,
    Hex,
    Binary,
    Scientific,
}

pub fn format_number(raw: [u8; 8], kind: &ExtendType, format: NumberFormat) -> String {
    use ExtendType::*;
    let bits = u64::from_le_bytes(raw);
    let width = match kind {
        U8 | I8 => 8,
        U16 | I16 => 16,
        U32 | I32 | Float => 32,
        _ => 64,
    };
    let bits = if width == 64 {
        bits
    } else {
        bits & ((1u64 << width) - 1)
    };
    if matches!(kind, Float | Double) {
        let value = if *kind == Float {
            f32::from_bits(bits as u32) as f64
        } else {
            f64::from_bits(bits)
        };
        return if format == NumberFormat::Scientific {
            format!("{value:.6e}")
        } else {
            value.to_string()
        };
    }
    match format {
        NumberFormat::Hex => format!("0x{bits:0digits$X}", digits = width / 4),
        NumberFormat::Binary => format!("0b{bits:0digits$b}", digits = width),
        _ => match kind {
            I8 => (bits as i8).to_string(),
            I16 => (bits as i16).to_string(),
            I32 => (bits as i32).to_string(),
            I64 => (bits as i64).to_string(),
            Other => "不支持的类型".into(),
            _ => bits.to_string(),
        },
    }
}

/// 把采样原始字节按变量类型换算成数值；不支持的类型返回 None。
pub fn raw_to_f64(raw: [u8; 8], kind: &ExtendType) -> Option<f64> {
    use ExtendType::*;
    let bits = u64::from_le_bytes(raw);
    Some(match kind {
        U8 => bits as u8 as f64,
        I8 => bits as u8 as i8 as f64,
        U16 => bits as u16 as f64,
        I16 => bits as u16 as i16 as f64,
        U32 => bits as u32 as f64,
        I32 => bits as u32 as i32 as f64,
        U64 => bits as f64,
        I64 => bits as i64 as f64,
        Float => f32::from_bits(bits as u32) as f64,
        Double => f64::from_bits(bits),
        Other => return None,
    })
}

pub fn csv_field(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[derive(Default, Clone, Debug)]
pub struct IntervalStats {
    pub count: usize, // 区间有效样本数
    pub min: f64,     // 最小值，沿用变量单位
    pub max: f64,     // 最大值，沿用变量单位
    pub mean: f64,    // 均值，沿用变量单位
}
impl IntervalStats {
    pub fn calculate(points: impl Iterator<Item = (f64, f64)>, a: f64, b: f64) -> Self {
        let mut result = Self::default();
        if !a.is_finite() || !b.is_finite() {
            return result;
        }
        let (a, b) = (a.min(b), a.max(b));
        for (time, value) in points {
            if !time.is_finite() || !value.is_finite() || time < a || time > b {
                continue;
            }
            result.count += 1;
            if result.count == 1 {
                result.min = value;
                result.max = value;
                result.mean = value;
            } else {
                result.min = result.min.min(value);
                result.max = result.max.max(value);
                let weight = 1.0 / result.count as f64;
                result.mean = result.mean * (1.0 - weight) + value * weight;
            }
        }
        result
    }
}

#[derive(Clone, Debug)]
pub struct WriteRecord {
    pub id: u64,          // 底层写入请求编号
    pub time: u64,        // 提交时间，Unix 毫秒
    pub source: String,   // 手工或 AI 提交来源
    pub variable: String, // 提交时的完整变量路径
    pub old: String,      // 缓存或预期及实读旧值
    pub new: String,      // 提交的新值
    pub state: String,    // 真实提交或完成状态
    pub readback: String, // 回读验证状态及原始位
}

#[derive(Default)]
pub struct WriteJournal {
    pub records: Vec<WriteRecord>, // 本次运行的写入记录
    pub open: bool,                // 是否显示记录窗口
}
impl WriteJournal {
    pub fn submitted(&mut self, id: u64, source: &str, variable: String, old: String, new: String) {
        self.records.push(WriteRecord {
            id,
            time: crate::ai_readonly::now_ms(),
            source: source.into(),
            variable,
            old,
            new,
            state: "已提交".into(),
            readback: "待结果".into(),
        });
    }
    pub fn finished(&mut self, id: u64, result: Result<(), String>, verified: bool) {
        if let Some(record) = self.records.iter_mut().rev().find(|r| r.id == id) {
            record.state = match result {
                Ok(()) => "成功".into(),
                Err(e) => format!("失败或未确认：{e}"),
            };
            record.readback = if verified {
                "已回读验证"
            } else {
                "未回读验证"
            }
            .into();
        }
    }
    pub fn cancel_pending(&mut self, reason: &str) {
        for record in &mut self.records {
            if record.state == "已提交" {
                record.state = format!("结果未确认：{reason}");
                record.readback = "未回读验证".into();
            }
        }
    }
    pub fn export(&self, writer: &mut impl Write) -> std::io::Result<()> {
        writeln!(
            writer,
            "\u{feff}时间戳毫秒,请求编号,来源,变量路径,旧值,新值,结果,回读状态"
        )?;
        for r in &self.records {
            writeln!(
                writer,
                "{},{},{},{},{},{},{},{}",
                r.time,
                r.id,
                csv_field(&r.source),
                csv_field(&r.variable),
                csv_field(&r.old),
                csv_field(&r.new),
                csv_field(&r.state),
                csv_field(&r.readback)
            )?;
        }
        writer.flush()
    }
    pub fn show(&mut self, ctx: &egui::Context) {
        let mut open = self.open;
        egui::Window::new("参数修改记录").open(&mut open).default_size([850.0,400.0]).show(ctx, |ui| {
            ui.label("记录本次运行中通过变量表及 AI 接口提交的写入；手工旧值来自采样缓存；AI 保留预期值及可用的真实预读原始位。仅状态明确标注时表示回读验证。");
            if ui.button("导出修改记录 CSV").clicked() {
                if let Some(path) = crate::ui::file_dialog().add_filter("CSV", &["csv"]).set_file_name("参数修改记录.csv").save_file() {
                    let result = std::fs::File::create(path).and_then(|file| self.export(&mut std::io::BufWriter::new(file)));
                    ctx.data_mut(|d| d.insert_temp(egui::Id::new("journal_export_status"), result.map(|_| "导出成功".to_owned()).unwrap_or_else(|e| format!("导出失败：{e}"))));
                }
            }
            if let Some(status) = ctx.data_mut(|d| d.get_temp::<String>(egui::Id::new("journal_export_status"))) { ui.label(status); }
            egui::ScrollArea::both().show(ui, |ui| {
                egui::Grid::new("write_journal_grid").striped(true).show(ui, |ui| {
                    for title in ["时间戳(ms)","来源","变量","旧值","新值","结果","回读"] { ui.strong(title); } ui.end_row();
                    for r in self.records.iter().rev() {
                        for value in [r.time.to_string(),r.source.clone(),r.variable.clone(),r.old.clone(),r.new.clone(),r.state.clone(),r.readback.clone()] { ui.label(value); } ui.end_row();
                    }
                });
            });
        });
        self.open = open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn formats_signed_and_float_without_altering_raw() {
        assert_eq!(
            format_number([255; 8], &ExtendType::I8, NumberFormat::Decimal),
            "-1"
        );
        assert_eq!(
            format_number([255; 8], &ExtendType::I8, NumberFormat::Hex),
            "0xFF"
        );
        assert_eq!(
            format_number([255; 8], &ExtendType::I8, NumberFormat::Binary),
            "0b11111111"
        );
        assert!(
            format_number(
                1.25f64.to_le_bytes(),
                &ExtendType::Double,
                NumberFormat::Scientific
            )
            .contains("e0")
        );
    }
    #[test]
    fn raw_bytes_convert_by_type_for_expressions() {
        assert_eq!(
            raw_to_f64((-2i16 as u16 as u64).to_le_bytes(), &ExtendType::I16),
            Some(-2.0)
        );
        assert_eq!(
            raw_to_f64(0x1FFu64.to_le_bytes(), &ExtendType::U8),
            Some(255.0)
        );
        assert_eq!(
            raw_to_f64(
                u64::from(1.5f32.to_bits()).to_le_bytes(),
                &ExtendType::Float
            ),
            Some(1.5)
        );
        assert_eq!(raw_to_f64([0; 8], &ExtendType::Other), None);
    }

    #[test]
    fn stats_use_raw_finite_points_and_reversed_bounds() {
        let stats = IntervalStats::calculate(
            [
                (0.0, 99.0),
                (1.0, -2.0),
                (2.0, 8.0),
                (2.5, f64::NAN),
                (3.0, 3.0),
            ]
            .into_iter(),
            2.5,
            1.0,
        );
        assert_eq!(stats.count, 2);
        assert_eq!(stats.min, -2.0);
        assert_eq!(stats.max, 8.0);
        assert_eq!(stats.mean, 3.0);
        assert_eq!(
            IntervalStats::calculate(std::iter::empty(), 0.0, 1.0).count,
            0
        );
    }
    #[test]
    fn journal_only_marks_success_after_completion() {
        let mut journal = WriteJournal::default();
        journal.submitted(1, "手工", "kp".into(), "1".into(), "2".into());
        assert_eq!(journal.records[0].state, "已提交");
        journal.finished(1, Ok(()), false);
        assert_eq!(journal.records[0].state, "成功");
        assert_eq!(journal.records[0].readback, "未回读验证");
        journal.submitted(2, "AI", "ki".into(), "0".into(), "1".into());
        journal.finished(2, Err("读回不一致".into()), false);
        assert!(journal.records[1].state.starts_with("失败"));
        journal.submitted(3, "AI", "kd".into(), "0".into(), "1".into());
        journal.finished(3, Ok(()), true);
        assert_eq!(journal.records[2].readback, "已回读验证");
        let mut csv = Vec::new();
        journal.export(&mut csv).unwrap();
        assert!(String::from_utf8(csv).unwrap().contains("kp"));
    }
    #[test]
    fn csv_quotes_commas_and_line_breaks() {
        assert_eq!(csv_field("a,\"b\"\nc"), "\"a,\"\"b\"\"\nc\"");
    }
}
