#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod app;
mod dwarf;
#[cfg(target_os = "windows")]
mod install_check;
mod model;
mod probe;
mod svd;
mod ui;

use anyhow::{Result, anyhow};

use crate::app::MemRW3App;

fn main() -> Result<()> {
    #[cfg(target_os = "windows")]
    if let Some(code) = install_check::run_from_args() {
        std::process::exit(code);
    }

    let dwarf_state = dwarf::types::DwarfState::new(Vec::new());
    let window_icon =
        eframe::icon_data::from_png_bytes(include_bytes!("../assets/MemRW3-pink.png"))?;

    eframe::run_native(
        "MemRW3 - Memory Read/Write Monitor",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_icon(window_icon)
                .with_inner_size([1280.0, 720.0])
                .with_min_inner_size([800.0, 500.0]),
            ..Default::default()
        },
        Box::new(move |cc| {
            app::setup_fonts(&cc.egui_ctx);
            Ok(Box::new(MemRW3App::new(dwarf_state, cc.egui_ctx.clone())))
        }),
    )
    .map_err(|e| anyhow!("{}", e))?;
    Ok(())
}
