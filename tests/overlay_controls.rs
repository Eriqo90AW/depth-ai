#![cfg(feature = "tray")]

use depth::gui::OverlayWindow;
use slint::{
    ComponentHandle,
    platform::{PointerEventButton, WindowEvent},
};
use std::{cell::Cell, rc::Rc};

fn click(ui: &OverlayWindow, x: f32) {
    let position = slint::LogicalPosition::new(x, 24.0);
    let button = PointerEventButton::Left;
    ui.window()
        .dispatch_event(WindowEvent::PointerMoved { position });
    ui.window()
        .dispatch_event(WindowEvent::PointerPressed { position, button });
    ui.window()
        .dispatch_event(WindowEvent::PointerReleased { position, button });
}

#[test]
fn overlay_actions_are_separate_and_keyboard_accessible() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = OverlayWindow::new().unwrap();
    ui.window().set_size(slint::LogicalSize::new(156.0, 48.0));
    ui.show().unwrap();
    let stops = Rc::new(Cell::new(0));
    let opens = Rc::new(Cell::new(0));
    let observed = stops.clone();
    ui.on_stop(move || observed.set(observed.get() + 1));
    let observed = opens.clone();
    ui.on_open_app(move || observed.set(observed.get() + 1));

    click(&ui, 24.0);
    assert_eq!((stops.get(), opens.get()), (1, 0));
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: " ".into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: " ".into() });
    assert_eq!((stops.get(), opens.get()), (2, 0));
    click(&ui, 90.0);
    assert_eq!((stops.get(), opens.get()), (2, 1));
    ui.window()
        .dispatch_event(WindowEvent::KeyPressed { text: "\n".into() });
    ui.window()
        .dispatch_event(WindowEvent::KeyReleased { text: "\n".into() });
    assert_eq!((stops.get(), opens.get()), (2, 2));

    ui.set_finishing(true);
    click(&ui, 24.0);
    assert_eq!((stops.get(), opens.get()), (2, 2));
    click(&ui, 150.0);
    assert_eq!((stops.get(), opens.get()), (2, 3));
}
