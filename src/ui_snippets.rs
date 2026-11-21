// One-off scripts with secrets injected: migrations, quick api pokes, etc.

use crate::app::{App, INTERPRETERS};
use crate::config::SnippetDef;
use crate::metrics::fmt_duration;
use crate::supervisor::Status;
use crate::theme::{self, Role, Scheme, chip, field_label};
use crate::ui_procs::{empty_state, log_view, pane_panel, secret_rows, sheet, status_look};
use eframe::egui::{self, CornerRadius, Margin, RichText};

const STARTER: &str = "# secrets you map below show up as env vars\necho \"hello from clawrun\"\nenv | grep -c . | xargs echo \"env vars:\"\n";

pub fn show(app: &mut App, ctx: &egui::Context) {
    let sc = theme::scheme(ctx);

    egui::SidePanel::left("snippets")
        .resizable(true)
        .show_separator_line(false)
        .default_width(250.0)
        .frame(sc.panel(sc.surface_container_low).inner_margin(Margin::symmetric(12, 12)))
        .show(ctx, |ui| {
            if ui.add(sc.filled("+   New snippet").min_size(egui::vec2(0.0, 48.0)).corner_radius(CornerRadius::same(16))).clicked() {
                let s = SnippetDef { code: STARTER.into(), ..Default::default() };
                app.selected_snippet = Some(s.id.clone());
                app.project.snippets.push(s);
                app.save_project();
            }
            ui.add_space(10.0);
            ui.label(RichText::new(format!("SNIPPETS · {}", app.project.snippets.len())).small().strong().color(sc.on_surface_variant));
            let mut clicked = None;
            egui::ScrollArea::vertical().show(ui, |ui| {
                for s in &app.project.snippets {
                    let running = app.rt.get(&s.id).is_some_and(|r| r.status.is_live());
                    let icon = if running { "⏳" } else { "📝" };
                    let sel = app.selected_snippet.as_deref() == Some(&s.id);
                    if theme::nav_item(ui, &sc, &format!("{icon}   {}", s.name), sel, 44.0).clicked() {
                        clicked = Some(s.id.clone());
                    }
                }
            });
            if let Some(id) = clicked {
                app.selected_snippet = Some(id);
            }
        });

    egui::CentralPanel::default().frame(pane_panel(&sc, 0, 12)).show(ctx, |ui| {
        sheet(ui, &sc, |ui| {
            let id = app.selected_snippet.clone().filter(|id| app.project.snippets.iter().any(|s| &s.id == id));
            match id {
                Some(id) => editor(app, ui, &sc, &id),
                None => empty_state(ui, &sc, "📝", "No snippets yet", "Scripts you run once in a while, with 1Claw secrets in their env."),
            }
        });
    });
}

fn editor(app: &mut App, ui: &mut egui::Ui, sc: &Scheme, id: &str) {
    let idx = app.project.snippets.iter().position(|s| s.id == id).unwrap();
    let mut s = app.project.snippets[idx].clone();
    let rt = app.rt.entry(id.to_string()).or_default();
    let status = rt.status.clone();
    let live = status.is_live();
    let took = match (rt.started_at, rt.ended_at) {
        (Some(a), Some(b)) if b > a && !live => Some(b - a),
        (Some(a), _) if live => Some(a.elapsed()),
        _ => None,
    };
    let logs = rt.logs.clone();

    // top row: name + run controls
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut s.name)
                .font(egui::FontId::proportional(20.0))
                .frame(false)
                .desired_width(260.0),
        );
        if !matches!(status, Status::Stopped) {
            let (_, role, label) = status_look(sc, &status);
            chip(ui, sc, &label, role);
        }
        if let Some(d) = took {
            chip(ui, sc, &format!("⏱ {}", if d.as_secs() == 0 { format!("{}ms", d.as_millis()) } else { fmt_duration(d) }), Role::Neutral);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(sc.text_button("🗑", sc.error)).on_hover_text("Delete snippet").clicked() {
                app.rt.remove(id);
                app.project.snippets.retain(|x| x.id != id);
                app.selected_snippet = app.project.snippets.first().map(|x| x.id.clone());
                app.save_project();
                return;
            }
            let run_key = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
            if live {
                if ui.add(sc.danger("■  Stop")).clicked() {
                    app.stop(id);
                }
            } else if ui.add(sc.filled("▶  Run").min_size(egui::vec2(96.0, 40.0))).on_hover_text("Ctrl+Enter").clicked() || run_key {
                app.project.snippets[idx] = s.clone();
                app.save_project();
                logs.lock().unwrap().clear();
                app.start(id);
            }
        });
    });
    if !app.project.snippets.iter().any(|x| x.id == id) {
        return; // just deleted
    }
    ui.add_space(8.0);

    ui.horizontal_wrapped(|ui| {
        field_label(ui, sc, "Run with");
        for (name, ..) in INTERPRETERS {
            let sel = s.interpreter == *name;
            let (fill, fg) = if sel { (sc.secondary_container, sc.on_secondary_container) } else { (sc.surface_container_high, sc.on_surface_variant) };
            let b = egui::Button::new(RichText::new(if sel { format!("✔ {name}") } else { name.to_string() }).color(fg))
                .fill(fill)
                .corner_radius(CornerRadius::same(8))
                .min_size(egui::vec2(0.0, 28.0));
            if ui.add(b).clicked() {
                s.interpreter = name.to_string();
            }
        }
        ui.add_space(12.0);
        field_label(ui, sc, "in");
        ui.add(
            egui::TextEdit::singleline(&mut s.cwd)
                .hint_text("working dir")
                .font(egui::TextStyle::Monospace)
                .margin(Margin::symmetric(10, 6))
                .desired_width(200.0),
        );
    });
    ui.add_space(6.0);

    egui::CollapsingHeader::new(RichText::new(format!("🔑 Secrets ({})", s.secrets.len())).color(sc.on_surface))
        .id_salt(("snip-secrets", id))
        .default_open(!s.secrets.is_empty())
        .show(ui, |ui| {
            let mut rows = std::mem::take(&mut s.secrets);
            secret_rows(app, ui, sc, &mut rows, id);
            s.secrets = rows;
        });
    ui.add_space(6.0);

    // code + output split the rest of the height
    let h = ui.available_height();
    egui::Frame::new()
        .fill(sc.surface_container)
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt(("code", id)).max_height(h * 0.42).show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut s.code)
                        .code_editor()
                        .frame(false)
                        .margin(Margin::same(6))
                        .desired_rows(10)
                        .desired_width(f32::INFINITY),
                );
            });
        });
    ui.add_space(10.0);
    log_view(ui, sc, &logs, &mut app.snippet_log, id);

    if s != app.project.snippets[idx] {
        app.project.snippets[idx] = s;
        app.snippets_dirty = Some(std::time::Instant::now());
    }
}
