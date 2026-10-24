//! Installer-only renderer probe. It never creates an application session or accesses a probe.
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub fn run_from_args() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--check-install")) {
        return None;
    }
    let Some(report) = args.next() else {
        return Some(64);
    };
    Some(run(Path::new(&report)))
}

fn run(report: &Path) -> i32 {
    let pending = "图形初始化未完成。请更新 Windows 和显卡驱动，并在本机桌面重新运行安装包。";
    if std::fs::write(report, pending).is_err() {
        return 74;
    }
    let rendered = Arc::new(AtomicBool::new(false));
    let observed = rendered.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eframe::run_native(
            "MemRW3 installation check",
            eframe::NativeOptions {
                viewport: eframe::egui::ViewportBuilder::default()
                    .with_visible(false)
                    .with_inner_size([64.0, 64.0]),
                ..Default::default()
            },
            Box::new(move |cc| {
                cc.egui_ctx.request_repaint();
                Ok(Box::new(RenderProbe(observed)))
            }),
        )
    }));
    let (code, message) = match result {
        Ok(Ok(())) if rendered.load(Ordering::Acquire) => {
            (0, "OK: 系统运行库与图形初始化检查通过。".to_owned())
        }
        Ok(Ok(())) => (21, pending.to_owned()),
        Ok(Err(error)) => (
            21,
            format!(
                "图形初始化失败：{error}\n请安装显卡厂商的正式驱动并更新 Windows；远程桌面或虚拟机请启用图形加速。"
            ),
        ),
        Err(_) => (21, format!("{pending}\n图形驱动初始化发生异常。")),
    };
    if std::fs::write(report, message).is_err() {
        return 74;
    }
    code
}

struct RenderProbe(Arc<AtomicBool>);

impl eframe::App for RenderProbe {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        self.0.store(true, Ordering::Release);
        ui.ctx()
            .send_viewport_cmd(eframe::egui::ViewportCommand::Close);
    }
}
