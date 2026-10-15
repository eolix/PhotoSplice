//! Layers panel right-click menu in Photoshop's order. Items are command ids from the menu catalog;
//! unavailable ones are greyed using the same enablement as the main menus. Items marked as menu
//! invocations (`Value::Null` params) go through `menus::invoke`, so dialogs open like in the menu bar.

use photosplice_doc::{LabelColor, Layer, LayerContent};
use serde_json::{Value, json};

/// One entry: label, command id. `None` = separator.
pub type Entry = Option<(&'static str, &'static str)>;

/// The command behind "Add Layer Mask" (the panel button and this menu). Like Photoshop, an active
/// selection becomes the mask; ⌥ inverts it (Hide Selection, or Hide All without a selection).
pub fn add_mask_command(has_selection: bool, alt: bool) -> &'static str {
    match (has_selection, alt) {
        (true, false) => "layer.layerMask.revealSelection",
        (true, true) => "layer.layerMask.hideSelection",
        (false, false) => "layer.layerMask.revealAll",
        (false, true) => "layer.layerMask.hideAll",
    }
}

/// The context menu entries for a layer (Photoshop 2026 order, trimmed to the layer kind).
pub fn entries(l: &Layer, multi: bool, has_selection: bool) -> Vec<Entry> {
    let mut v: Vec<Entry> = vec![Some((tl!("Blending Options…"), "layer.layerStyle.blendingOptions"))];
    v.push(None);
    v.push(Some((if multi { tl!("Duplicate Layers…") } else { tl!("Duplicate Layer…") }, "layer.duplicate")));
    v.push(Some((if multi { tl!("Delete Layers") } else { tl!("Delete Layer") }, "layer.delete")));
    if !multi {
        v.push(Some((tl!("Group from Layers…"), "layer.groupLayers")));
    }
    v.push(None);
    v.push(Some((tl!("Quick Export As PNG"), "layer.quickExportAsPng")));
    v.push(Some((tl!("Export As…"), "layer.exportAs")));
    v.push(None);
    v.push(Some((tl!("Artboard from Layers…"), "layer.new.artboardFromLayers")));
    v.push(Some((tl!("Frame from Layers…"), "layer.new.frameFromLayers")));
    v.push(None);
    v.push(Some((tl!("Convert to Smart Object"), "layer.smartObjects.convertToSmartObject")));
    match &l.content {
        LayerContent::Text(_) => v.push(Some(("Rasterize Type", "layer.rasterize.type"))),
        LayerContent::Shape(_) => v.push(Some(("Rasterize Layer", "layer.rasterize.shape"))),
        LayerContent::Smart(_) => {
            v.push(Some((tl!("Edit Contents"), "layer.smartObjects.editContents")));
            v.push(Some((tl!("Convert to Layers"), "layer.smartObjects.convertToLayers")));
            v.push(Some(("Rasterize Layer", "layer.rasterize.smartObject")));
        }
        LayerContent::Fill(_) => v.push(Some(("Rasterize Layer", "layer.rasterize.fillContent"))),
        _ => v.push(Some(("Rasterize Layer", "layer.rasterize.layer"))),
    }
    v.push(None);
    if l.mask.is_some() {
        v.push(Some((tl!("Disable Layer Mask"), "layer.layerMask.enabled")));
        v.push(Some((tl!("Apply Layer Mask"), "layer.layerMask.apply")));
        v.push(Some((tl!("Delete Layer Mask"), "layer.layerMask.delete")));
    } else {
        v.push(Some((tl!("Add Layer Mask"), add_mask_command(has_selection, false))));
    }
    v.push(Some((
        if l.clipped { tl!("Release Clipping Mask") } else { tl!("Create Clipping Mask") },
        if l.clipped { "layer.releaseClippingMask" } else { "layer.createClippingMask" },
    )));
    v.push(None);
    v.push(Some((tl!("Link Layers"), "layer.linkLayers")));
    v.push(Some((tl!("Select Linked Layers"), "layer.selectLinkedLayers")));
    v.push(None);
    v.push(Some((tl!("Copy Layer Style"), "layer.layerStyle.copyLayerStyle")));
    v.push(Some((tl!("Paste Layer Style"), "layer.layerStyle.pasteLayerStyle")));
    v.push(Some((tl!("Clear Layer Style"), "layer.layerStyle.clear")));
    v.push(None);
    if multi {
        v.push(Some((tl!("Merge Layers"), "layer.mergeLayers")));
    } else {
        v.push(Some((tl!("Merge Down"), "layer.mergeDown")));
    }
    v.push(Some((tl!("Merge Visible"), "layer.mergeVisible")));
    v.push(Some((tl!("Flatten Image"), "layer.flattenImage")));
    v
}

/// Render the menu. Pushes `(command, params)` actions; `Value::Null` params mean "invoke like the
/// menu item" (opens the command's dialog when it has one).
pub fn show(app: &crate::PhotospliceApp, ui: &mut egui::Ui, l: &Layer, on_set: bool, actions: &mut Vec<(String, Value)>) -> bool {
    ui.set_min_width(220.0);
    let mut rename = false;
    let mut last_sep = true;
    let has_selection = app.session.active().is_some_and(|s| s.doc.selection.is_some());
    for e in entries(l, on_set, has_selection) {
        match e {
            None => {
                if !last_sep {
                    ui.separator();
                }
                last_sep = true;
            }
            Some((label, id)) => {
                // Skip commands this build doesn't have rather than showing dead items.
                if photosplice_engine::commands::find(id).is_none() && !crate::menu_catalog::CATALOG.iter().any(|m| m.3 == id) {
                    continue;
                }
                last_sep = false;
                // Enablement is exact for the active layer (or the selection); another row is
                // selected first when clicked, so its items stay available.
                let is_active = app.session.active().is_some_and(|s| s.active_layer == Some(l.id));
                let enabled = if on_set || is_active { crate::menus::is_enabled(app, id) } else { true };
                if ui.add_enabled(enabled, egui::Button::new(tl!(&label))).clicked() {
                    if !on_set {
                        actions.push(("layer.select".into(), json!({"layer": l.id.0})));
                    }
                    actions.push((id.into(), Value::Null));
                    ui.close();
                }
            }
        }
    }
    ui.separator();
    if ui.button(tl!("Rename Layer…")).clicked() {
        rename = true;
        ui.close();
    }
    // Colour labels (Photoshop's last block), as one row of swatches so the menu stays short:
    // applied to every selected layer when the menu opened on the selection, as one history step.
    ui.separator();
    let t = crate::theme::Tokens::get(ui.ctx());
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for label in LabelColor::ALL {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
            let name = tl!(label.label());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
            let swatch = rect.shrink(3.0);
            match t.label_color(label) {
                Some(c) => {
                    ui.painter().rect_filled(swatch, 3.0, c);
                }
                None => {
                    // No Color: an empty swatch struck through.
                    let stroke = egui::Stroke::new(1.0, t.text_faint);
                    ui.painter().rect_stroke(swatch, 3.0, stroke, egui::StrokeKind::Inside);
                    ui.painter().line_segment([swatch.left_bottom(), swatch.right_top()], stroke);
                }
            }
            if (l.label == label && !on_set) || resp.hovered() {
                ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.5, if resp.hovered() { t.text } else { t.accent }), egui::StrokeKind::Inside);
            }
            if resp.on_hover_text(name).clicked() {
                picked = Some(label);
            }
        }
    });
    if let Some(label) = picked {
        let targets: Vec<u64> = match app.session.active() {
            Some(s) if on_set => s.selected_layers().iter().map(|id| id.0).collect(),
            _ => vec![l.id.0],
        };
        actions.extend(color_actions(&targets, label, ui.ctx().cumulative_pass_nr()));
        ui.close();
    }
    rename
}

/// The `layer.setProps` calls that give `layers` the colour `label`, coalesced into one history
/// step under `key`.
pub fn color_actions(layers: &[u64], label: LabelColor, key: u64) -> Vec<(String, Value)> {
    layers.iter().map(|id| ("layer.setProps".to_string(), json!({"layer": id, "color": label.id(), "coalesce": format!("layer-color:{key}")}))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_object_context_menu_edits_the_clicked_layer() {
        use crate::PhotospliceApp;
        use egui::{Event, Modifiers, PointerButton, Pos2, pos2, vec2};
        use egui_kittest::{Harness, kittest::Queryable};

        fn click(h: &mut Harness<'_, PhotospliceApp>, at: Pos2, button: PointerButton) {
            h.hover_at(at);
            h.step();
            for pressed in [true, false] {
                h.event(Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE });
                h.step();
            }
            h.run_steps(3);
        }

        for ppp in [1.0, 2.0] {
            let mut s = photosplice_engine::Session::new();
            s.execute("file.new", json!({"width": 640, "height": 480})).unwrap();
            s.execute("layer.new.layer", json!({"name": "Smart source"})).unwrap();
            s.execute("paint.stroke", json!({"points": [[100, 100]], "size": 20})).unwrap();
            let smart = s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap()["layer"].as_u64().unwrap();
            s.execute("layer.new.layer", json!({"name": "Other active layer"})).unwrap();
            let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).with_pixels_per_point(ppp).with_step_dt(1.0 / 60.0).with_max_steps(64).build_eframe(
                move |cc| {
                    PhotospliceApp::setup_context(&cc.egui_ctx, Default::default());
                    PhotospliceApp::new(s, crate::Services::default())
                },
            );
            let ctx = h.ctx.clone();
            let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"dock": {"collapsed": ["color", "properties", "history", "navigator"]}}));
            crate::control::handle(h.state_mut(), &ctx, &req);
            h.run_steps(8);
            let row = crate::layer_row_ui::recorded(&h.ctx).into_iter().find(|r| r.layer == smart).expect("layer row drawn").row;
            // Top-level Pro row: 6 pt padding, 28 pt eye column, 24 pt layer thumbnail.
            let at = pos2(row.left() + 6.0 + 28.0 + 12.0, row.center().y);
            click(&mut h, at, PointerButton::Secondary);
            let at = h.get_by_label("Edit Contents").rect().center();
            click(&mut h, at, PointerButton::Primary);
            assert_eq!(h.state().session.active_index(), Some(1), "@{ppp}x: contents tab opens");
            assert!(h.state().session.is_enabled("layer.smartObjects.saveContents"));
            assert!(h.state().session.active().unwrap().doc.walk().iter().any(|(_, _, l)| l.name == "Smart source"));
            h.state_mut().session.set_active(0);
            assert_eq!(h.state().session.active().unwrap().active_layer, Some(photosplice_doc::LayerId(smart)));
        }
    }

    #[test]
    fn color_label_from_the_context_menu() {
        use crate::PhotospliceApp;
        use egui::{Event, Modifiers, PointerButton, Pos2, pos2, vec2};
        use egui_kittest::{Harness, kittest::Queryable};

        fn click(h: &mut Harness<'_, PhotospliceApp>, at: Pos2, button: PointerButton) {
            h.hover_at(at);
            h.step();
            for pressed in [true, false] {
                h.event(Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE });
                h.step();
            }
            h.run_steps(3);
        }

        let mut s = photosplice_engine::Session::new();
        s.execute("file.new", json!({"width": 64, "height": 48})).unwrap();
        let a = s.execute("layer.new.layer", json!({"name": "A"})).unwrap()["layer"].as_u64().unwrap();
        let b = s.execute("layer.new.layer", json!({"name": "B"})).unwrap()["layer"].as_u64().unwrap();
        s.execute("layer.select", json!({"layer": a, "mode": "add"})).unwrap();
        let mut h = Harness::builder().with_size(vec2(1440.0, 900.0)).with_step_dt(1.0 / 60.0).with_max_steps(64).build_eframe(move |cc| {
            PhotospliceApp::setup_context(&cc.egui_ctx, Default::default());
            PhotospliceApp::new(s, crate::Services::default())
        });
        let ctx = h.ctx.clone();
        let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"dock": {"collapsed": ["color", "properties", "history", "navigator"]}}));
        crate::control::handle(h.state_mut(), &ctx, &req);
        h.run_steps(8);
        let row = crate::layer_row_ui::recorded(&h.ctx).into_iter().find(|r| r.layer == b).expect("layer row drawn").row;
        click(&mut h, pos2(row.left() + 6.0 + 28.0 + 12.0, row.center().y), PointerButton::Secondary);
        let at = h.get_by_label("Violet").rect().center();
        click(&mut h, at, PointerButton::Primary);
        let doc = &h.state().session.active().unwrap().doc;
        for id in [a, b] {
            assert_eq!(doc.layer(photosplice_doc::LayerId(id)).unwrap().label, LabelColor::Violet, "both selected layers");
        }
        // One history step for the whole selection.
        h.state_mut().session.execute("edit.undo", json!({})).unwrap();
        let doc = &h.state().session.active().unwrap().doc;
        assert!([a, b].iter().all(|id| doc.layer(photosplice_doc::LayerId(*id)).unwrap().label == LabelColor::None));
    }

    #[test]
    fn edit_contents_is_available_only_for_smart_objects() {
        let mut s = photosplice_engine::Session::new();
        s.execute("file.new", json!({"width": 32, "height": 24})).unwrap();
        s.execute("layer.new.layer", json!({"name": "Source"})).unwrap();
        let layer = |s: &photosplice_engine::Session| {
            let st = s.active().unwrap();
            st.doc.layer(st.active_layer.unwrap()).unwrap().clone()
        };
        assert!(!entries(&layer(&s), false, false).iter().flatten().any(|e| e.1 == "layer.smartObjects.editContents"));
        s.execute("paint.stroke", json!({"points": [[12, 12]], "size": 8})).unwrap();
        s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
        let ids: Vec<_> = entries(&layer(&s), false, false).into_iter().flatten().map(|e| e.1).collect();
        let edit = ids.iter().position(|id| *id == "layer.smartObjects.editContents").expect("Edit Contents entry");
        assert_eq!(ids[edit + 1..edit + 3], ["layer.smartObjects.convertToLayers", "layer.rasterize.smartObject"]);
    }

    #[test]
    fn entries_follow_layer_state() {
        let mut s = photosplice_engine::Session::new();
        s.execute("file.new", json!({"width": 10, "height": 10})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        let st = s.active().unwrap();
        let l = st.doc.layer(st.active_layer.unwrap()).unwrap().clone();
        let ids: Vec<&str> = entries(&l, false, false).into_iter().flatten().map(|e| e.1).collect();
        assert_eq!(ids.first(), Some(&"layer.layerStyle.blendingOptions"));
        assert!(ids.contains(&"layer.mergeDown") && !ids.contains(&"layer.mergeLayers"));
        assert!(ids.contains(&"layer.layerMask.revealAll"));
        let ids: Vec<&str> = entries(&l, false, true).into_iter().flatten().map(|e| e.1).collect();
        assert!(ids.contains(&"layer.layerMask.revealSelection") && !ids.contains(&"layer.layerMask.revealAll"));
        let ids: Vec<&str> = entries(&l, true, false).into_iter().flatten().map(|e| e.1).collect();
        assert!(ids.contains(&"layer.mergeLayers"));
    }

    #[test]
    fn add_mask_uses_the_selection() {
        assert_eq!(add_mask_command(false, false), "layer.layerMask.revealAll");
        assert_eq!(add_mask_command(false, true), "layer.layerMask.hideAll");
        assert_eq!(add_mask_command(true, true), "layer.layerMask.hideSelection");
        // With a selection the new mask is the selection: inside revealed, outside hidden.
        let mut s = photosplice_engine::Session::new();
        s.execute("file.new", json!({"width": 10, "height": 10})).unwrap();
        s.execute("layer.new.layer", json!({})).unwrap();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 4, "height": 10})).unwrap();
        s.execute(add_mask_command(true, false), json!({})).unwrap();
        let st = s.active().unwrap();
        let mask = &st.doc.layer(st.active_layer.unwrap()).unwrap().mask.as_ref().unwrap().surface;
        assert!(mask.pixel(1, 5)[0] > 0.99 && mask.pixel(8, 5)[0] < 0.01);
    }

    #[test]
    fn every_entry_is_a_known_command() {
        let mut s = photosplice_engine::Session::new();
        s.execute("file.new", json!({"width": 10, "height": 10})).unwrap();
        let l = s.active().unwrap().doc.layers[0].clone();
        for (_, id) in entries(&l, false, true).into_iter().chain(entries(&l, false, false)).flatten() {
            assert!(crate::menu_catalog::CATALOG.iter().any(|m| m.3 == id) || photosplice_engine::commands::find(id).is_some(), "{id}");
        }
    }
}
