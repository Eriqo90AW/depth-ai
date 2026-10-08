//! Tray integration uses Slint's existing Windows message loop.
use anyhow::Result;
use muda::{Menu, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
pub fn menu() -> Result<(Menu, MenuItem, MenuItem, MenuItem)> {
    let menu = Menu::new();
    let open = MenuItem::with_id("open", "Open Depth", true, None);
    let toggle = MenuItem::with_id("toggle", "Start recording", true, None);
    let quit = MenuItem::with_id("quit", "Quit", true, None);
    menu.append_items(&[&open, &toggle, &quit])?;
    Ok((menu, open, toggle, quit))
}
pub fn create(menu: Menu) -> Result<TrayIcon> {
    let mask = include_bytes!("../assets/icon/tray-32.mask");
    let mut rgba = vec![0u8; 32 * 32 * 4];
    for (i, c) in mask.chunks_exact(2).enumerate() {
        for (j, v) in [52u32, 103, 201].iter().enumerate() {
            rgba[i * 4 + j] = ((*v * (255 - c[1] as u32) + 255 * c[1] as u32) / 255) as u8;
        }
        rgba[i * 4 + 3] = c[0];
    }
    Ok(TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Depth")
        .with_icon(Icon::from_rgba(rgba, 32, 32)?)
        .build()?)
}
