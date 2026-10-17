//! With no document open, double-clicking the empty workspace shows File › Open, as in Reference
//! (#5367), with Auto Show the Home Screen off (the plain workspace); a single click does nothing.
//! PhotoSplice's own Home screen keeps its clicks for its own widgets.

use std::cell::RefCell;
use std::rc::Rc;

use egui::{Event, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::Harness;

use crate::file_dialog::{FileDialogReply, FileDialogRequest};
use crate::{PhotospliceApp, Services};

type Requests = Rc<RefCell<Vec<(FileDialogRequest, FileDialogReply)>>>;

fn harness(home: bool) -> (Harness<'static, PhotospliceApp>, Requests) {
    let requests: Requests = Default::default();
    let held = requests.clone();
    let services = Services { file_dialog: Some(Box::new(move |request, _parent, reply| held.borrow_mut().push((request, reply)))), ..Default::default() };
    let mut app = PhotospliceApp::new(photosplice_engine::Session::new(), services);
    app.session.edit_prefs(|p| p.general.auto_show_home_screen = home);
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui, app: &mut PhotospliceApp| {
            let ctx = ui.ctx().clone();
            // Fonts set up after the first frame only apply from the next one.
            if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
            app.poll_file_dialog(&ctx, None);
        },
        app,
    );
    PhotospliceApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(4);
    (h, requests)
}

fn button(h: &mut Harness<'static, PhotospliceApp>, at: Pos2, pressed: bool) {
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
}

/// `count` clicks two frames apart at `at`, after letting the clock move past earlier clicks.
fn clicks(h: &mut Harness<'static, PhotospliceApp>, at: Pos2, count: usize) {
    h.hover_at(at);
    for _ in 0..40 {
        h.step();
    }
    for _ in 0..count {
        button(h, at, true);
        h.step();
        button(h, at, false);
        h.step();
    }
    h.run_steps(3);
}

fn opens(requests: &Requests) -> usize {
    requests.borrow().iter().filter(|(r, _)| matches!(r, FileDialogRequest::Open { multiple: true, .. })).count()
}

#[test]
fn double_clicking_the_empty_workspace_shows_file_open() {
    let (mut h, requests) = harness(false);
    assert!(h.state().session.documents().is_empty());
    let blank = pos2(40.0, 760.0);
    clicks(&mut h, blank, 1);
    assert_eq!(opens(&requests), 0, "a single click opens nothing");
    clicks(&mut h, blank, 2);
    assert_eq!(opens(&requests), 1, "a double-click shows File › Open");
}
