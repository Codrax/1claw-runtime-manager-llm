use crate::app::{App, LogView, ProcTab};
use crate::config::{EnvVar, ProcDef, SecretRef};
use crate::metrics::{fmt_bytes, fmt_duration};
use crate::supervisor::{SharedLogs, Status, Stream};
use crate::theme::{self, Role, Scheme, chip, dialog, field_label, search_bar, segmented};
use eframe::egui::{self, Color32, CornerRadius, Margin, RichText};
use egui_plot::{Line, Plot, PlotPoints};

// --- bits shared with the snippets tab ---

pub fn pane_panel(sc: &Scheme, left: i8, right: i8) -> egui::Frame {
    egui::Frame::new().fill(sc.surface_container_low).inner_margin(Margin { left, right, top: 0, bottom: 12 })
}

pub fn sheet(ui: &mut egui::Ui, sc: &Scheme, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(sc.surface)
        .corner_radius(CornerRadius::same(24))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            add(ui)
        });
}

pub fn empty_state(ui: &mut egui::Ui, sc: &Scheme, icon: &str, title: &str, sub: &str) {
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.28).max(10.0));
        ui.label(RichText::new(icon).size(42.0).color(sc.outline));
        ui.add_space(6.0);
        ui.label(RichText::new(title).size(16.0).color(sc.on_surface));
        ui.label(RichText::new(sub).color(sc.on_surface_variant));
    });
}

pub fn status_look(sc: &Scheme, s: &Status) -> (Color32, Role, String) {
    match s {
        Status::Running => (sc.success, Role::Success, "running".into()),
        Status::Starting => (sc.tertiary, Role::Tertiary, "starting".into()),
        Status::Stopping => (sc.tertiary, Role::Tertiary, "stopping".into()),
        Status::Backoff => (sc.tertiary, Role::Tertiary, "waiting to restart".into()),
        Status::Stopped => (sc.outline, Role::Neutral, "stopped".into()),
        Status::Exited(c) => (sc.outline, Role::Neutral, format!("exited ({c})")),
        Status::Crashed(c) => (sc.error, Role::Error, format!("crashed ({c})")),
        Status::Failed(_) => (sc.error, Role::Error, "failed".into()),
    }
}

/// The log pane: toolbar + virtualized list of lines.
pub fn log_view(ui: &mut egui::Ui, sc: &Scheme, logs: &SharedLogs, v: &mut LogView, id: &str) {
    ui.horizontal(|ui| {
        ui.scope(|ui| {
            ui.set_max_width(280.0);
            search_bar(ui, &mut v.filter, "Search output");
        });
        for (on, label) in [(&mut v.out, "stdout"), (&mut v.err, "stderr"), (&mut v.sys, "clawrun")] {
            filter_chip(ui, sc, on, label);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(sc.text_button("Clear", sc.primary)).clicked() {
                logs.lock().unwrap().clear();
            }
            if ui.add(sc.text_button("🗐 Copy", sc.primary)).on_hover_text("Copy visible lines (already masked)").clicked() {
                let l = logs.lock().unwrap();
                let text: Vec<String> = l.lines.iter().map(|x| format!("{} {}", x.time, x.text)).collect();
                ui.ctx().copy_text(text.join("\n"));
            }
            ui.checkbox(&mut v.follow, "Follow");
        });
    });
    ui.add_space(6.0);

    let l = logs.lock().unwrap();
    let q = v.filter.to_lowercase();
    let rows: Vec<usize> = l
        .lines
        .iter()
        .enumerate()
        .filter(|(_, x)| match x.stream {
            Stream::Out => v.out,
            Stream::Err => v.err,
            Stream::Sys => v.sys,
        })
        .filter(|(_, x)| q.is_empty() || x.text.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect();

    egui::Frame::new()
        .fill(if sc.dark { theme::mix(sc.surface, Color32::BLACK, 0.35) } else { sc.surface_container_low })
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            if rows.is_empty() {
                ui.label(RichText::new(if l.lines.is_empty() { "No output yet." } else { "Nothing matches." }).monospace().color(sc.outline));
                return;
            }
            let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 4.0;
            // the theme's roomy interact size makes each horizontal row way too tall here
            ui.spacing_mut().interact_size.y = row_h - 4.0;
            ui.spacing_mut().item_spacing.y = 4.0;
            egui::ScrollArea::both()
                .id_salt(("log", id))
                .auto_shrink(false)
                .stick_to_bottom(v.follow)
                .show_rows(ui, row_h, rows.len(), |ui, range| {
                    for &i in &rows[range] {
                        let line = &l.lines[i];
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.0;
                            ui.label(RichText::new(&line.time).monospace().color(sc.outline));
                            let (color, prefix) = match line.stream {
                                Stream::Out => (sc.on_surface, ""),
                                Stream::Err => (sc.error, ""),
                                Stream::Sys => (sc.tertiary, "▸ "),
                            };
                            ui.add(egui::Label::new(RichText::new(format!("{prefix}{}", line.text)).monospace().color(color)).extend());
                        });
                    }
                });
        });
}

fn filter_chip(ui: &mut egui::Ui, sc: &Scheme, on: &mut bool, label: &str) {
    let (fill, fg) = if *on { (sc.secondary_container, sc.on_secondary_container) } else { (Color32::TRANSPARENT, sc.on_surface_variant) };
    let text = if *on { format!("✔ {label}") } else { label.to_string() };
    let b = egui::Button::new(RichText::new(text).small().color(fg))
        .fill(fill)
        .stroke(egui::Stroke::new(1.0f32, if *on { fill } else { sc.outline_variant }))
        .corner_radius(CornerRadius::same(8))
        .min_size(egui::vec2(0.0, 28.0));
    if ui.add(b).clicked() {
        *on = !*on;
    }
}

/// Editable `ENV <- vault / path` rows. Used by both editors.
pub fn secret_rows(app: &mut App, ui: &mut egui::Ui, sc: &Scheme, rows: &mut Vec<SecretRef>, salt: &str) {
    let mut remove = None;
    let mut want_paths = None;
    for (i, r) in rows.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut r.env)
                    .hint_text("ENV_NAME")
                    .font(egui::TextStyle::Monospace)
                    .margin(Margin::symmetric(10, 8))
                    .desired_width(150.0),
            );
            ui.label(RichText::new("from").small().color(sc.outline));
            let vault_label = if r.vault.is_empty() { "vault".to_string() } else { r.vault.clone() };
            egui::ComboBox::from_id_salt((salt, "vault", i)).selected_text(vault_label).width(130.0).show_ui(ui, |ui| {
                if app.vault_names.is_empty() {
                    ui.label(RichText::new("connect to 1Claw to pick").small());
                }
                for v in &app.vault_names {
                    ui.selectable_value(&mut r.vault, v.clone(), v);
                }
            });
            ui.add(
                egui::TextEdit::singleline(&mut r.path)
                    .hint_text("path/to/secret")
                    .font(egui::TextStyle::Monospace)
                    .margin(Margin::symmetric(10, 8))
                    .desired_width(ui.available_width() - 80.0),
            );
            let menu = ui.menu_button("🔍", |ui| {
                ui.set_max_height(280.0);
                match app.paths.get(&r.vault) {
                    Some(list) if !list.is_empty() => {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for p in list {
                                if ui.selectable_label(r.path == *p, RichText::new(p).monospace()).clicked() {
                                    r.path = p.clone();
                                    if r.env.is_empty() {
                                        r.env = guess_env(p);
                                    }
                                    ui.close_menu();
                                }
                            }
                        });
                    }
                    _ => {
                        ui.label(RichText::new(if r.vault.is_empty() { "pick a vault first" } else { "loading..." }).small());
                    }
                }
            });
            if menu.response.clicked() && !r.vault.is_empty() {
                want_paths = Some(r.vault.clone());
            }
            if ui.add(sc.text_button("🗑", sc.on_surface_variant)).on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        rows.remove(i);
    }
    if let Some(v) = want_paths {
        app.fetch_paths(&v);
    }
    if ui.add(sc.text_button("+ Add secret", sc.primary)).clicked() {
        let vault = app.vault_names.first().cloned().unwrap_or_default();
        rows.push(SecretRef { vault, ..Default::default() });
    }
}

/// "stripe/secret_key" -> "STRIPE_SECRET_KEY"
pub fn guess_env(path: &str) -> String {
    path.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect()
}

// --- the processes tab ---

pub fn show(app: &mut App, ctx: &egui::Context) {
    let sc = theme::scheme(ctx);
    drawer(app, ctx, &sc);

    egui::CentralPanel::default().frame(pane_panel(&sc, 0, 12)).show(ctx, |ui| {
        sheet(ui, &sc, |ui| {
            let id = app.selected.clone().filter(|id| app.project.processes.iter().any(|p| &p.id == id));
            match id {
                Some(id) => detail(app, ui, &sc, &id),
                None => empty_state(
                    ui,
                    &sc,
                    "⚡",
                    "No process selected",
                    "Add one on the left. Its secrets come from 1Claw, not a .env file.",
                ),
            }
        });
    });

    editor(app, ctx, &sc);
    delete_dialog(app, ctx, &sc);
}

fn drawer(app: &mut App, ctx: &egui::Context, sc: &Scheme) {
    egui::SidePanel::left("procs")
        .resizable(true)
        .show_separator_line(false)
        .default_width(250.0)
        .frame(sc.panel(sc.surface_container_low).inner_margin(Margin::symmetric(12, 12)))
        .show(ctx, |ui| {
            if ui.add(sc.filled("+   New process").min_size(egui::vec2(0.0, 48.0)).corner_radius(CornerRadius::same(16))).clicked() {
                app.editor = Some((ProcDef { name: "my-app".into(), ..Default::default() }, true));
            }
            ui.add_space(10.0);
            search_bar(ui, &mut app.proc_filter, "Search processes");
            ui.add_space(8.0);
            let running = app.rt.values().filter(|r| r.child.is_some()).count();
            ui.label(
                RichText::new(format!("PROCESSES · {} · {running} running", app.project.processes.len()))
                    .small()
                    .strong()
                    .color(sc.on_surface_variant),
            );

            let q = app.proc_filter.to_lowercase();
            let mut clicked = None;
            egui::ScrollArea::vertical().show(ui, |ui| {
                for p in app.project.processes.iter().filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q)) {
                    let rt = app.rt.get(&p.id);
                    let status = rt.map(|r| r.status.clone()).unwrap_or(Status::Stopped);
                    let cpu = rt.filter(|r| r.child.is_some()).and_then(|r| r.cpu.back()).map(|v| format!("{:.0}%", v[1]));
                    let sel = app.selected.as_deref() == Some(&p.id);
                    if proc_item(ui, sc, &p.name, &status, cpu.as_deref(), sel).clicked() {
                        clicked = Some(p.id.clone());
                    }
                }
            });
            if let Some(id) = clicked {
                app.selected = Some(id);
            }
        });
}

fn proc_item(ui: &mut egui::Ui, sc: &Scheme, name: &str, status: &Status, cpu: Option<&str>, selected: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 48.0), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let bg = if selected {
            sc.secondary_container
        } else if resp.hovered() {
            theme::mix(sc.surface_container_low, sc.on_surface, 0.08)
        } else {
            Color32::TRANSPARENT
        };
        p.rect_filled(rect, CornerRadius::same(24), bg);
        let (dot, _, label) = status_look(sc, status);
        let c = egui::pos2(rect.left() + 22.0, rect.center().y);
        if *status == Status::Running {
            // soft halo so running ones pop
            p.circle_filled(c, 8.0, dot.gamma_multiply(0.25));
        }
        p.circle_filled(c, 4.5, dot);
        let fg = if selected { sc.on_secondary_container } else { sc.on_surface };
        let name_g = p.layout_no_wrap(name.to_string(), egui::FontId::proportional(14.5), fg);
        let sub_g = p.layout_no_wrap(label, egui::FontId::proportional(11.5), sc.on_surface_variant);
        let x = rect.left() + 40.0;
        p.with_clip_rect(rect.shrink(4.0)).galley(egui::pos2(x, rect.center().y - 17.0), name_g, fg);
        p.galley(egui::pos2(x, rect.center().y + 2.0), sub_g, sc.on_surface_variant);
        if let Some(cpu) = cpu {
            p.text(
                egui::pos2(rect.right() - 16.0, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                cpu,
                egui::FontId::monospace(12.0),
                sc.on_surface_variant,
            );
        }
    }
    resp
}

fn stat(ui: &mut egui::Ui, sc: &Scheme, label: &str, value: &str, w: f32) {
    sc.card(1).corner_radius(CornerRadius::same(16)).inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
        ui.set_width(w);
        ui.label(RichText::new(label).small().color(sc.on_surface_variant));
        ui.label(RichText::new(value).size(18.0).color(sc.on_surface));
    });
}

fn detail(app: &mut App, ui: &mut egui::Ui, sc: &Scheme, id: &str) {
    let def = app.project.processes.iter().find(|p| p.id == id).cloned().unwrap();
    let rt = app.rt.entry(id.to_string()).or_default();
    let status = rt.status.clone();
    let live = rt.status.is_live();
    let (_, role, label) = status_look(sc, &status);

    // header
    ui.horizontal(|ui| {
        theme::icon_tile(ui, sc, "▶", 48.0);
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&def.name).size(22.0).color(sc.on_surface));
                chip(ui, sc, &label, role);
                if !def.secrets.is_empty() {
                    chip(ui, sc, &format!("🔑 {} secrets", def.secrets.len()), Role::Secondary);
                }
            });
            ui.label(RichText::new(&def.command).monospace().color(sc.on_surface_variant));
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.menu_button(RichText::new("…").size(18.0), |ui| {
                if ui.button("✏  Edit").clicked() {
                    app.editor = Some((def.clone(), false));
                    ui.close_menu();
                }
                if ui.button("⧉  Duplicate").clicked() {
                    let mut d = def.clone();
                    d.id = crate::config::new_id();
                    d.name = format!("{} copy", d.name);
                    app.editor = Some((d, true));
                    ui.close_menu();
                }
                if ui.button(RichText::new("🗑  Delete").color(sc.error)).clicked() {
                    app.confirm_delete = Some(id.to_string());
                    ui.close_menu();
                }
            });
            if ui.add(sc.text_button("✏ Edit", sc.primary)).clicked() {
                app.editor = Some((def.clone(), false));
            }
            if live {
                if ui.add(sc.tonal("⟳  Restart")).clicked() {
                    app.restart(id);
                }
                if ui.add(sc.danger("■  Stop")).clicked() {
                    app.stop(id);
                }
            } else if ui.add(sc.filled("▶  Start")).clicked() {
                app.start(id);
            }
        });
    });
    if let Status::Failed(e) = &status {
        ui.add_space(6.0);
        egui::Frame::new()
            .fill(sc.error_container)
            .corner_radius(CornerRadius::same(12))
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(format!("⚠ {e}")).color(sc.on_error_container));
            });
    }
    ui.add_space(12.0);

    // stat tiles
    let rt = app.rt.get(id).unwrap();
    let cpu = rt.cpu.back().filter(|_| rt.child.is_some()).map(|v| format!("{:.1}%", v[1])).unwrap_or("-".into());
    let mem = rt.mem.back().filter(|_| rt.child.is_some()).map(|v| fmt_bytes(v[1])).unwrap_or("-".into());
    let up = rt.uptime().map(fmt_duration).unwrap_or("-".into());
    let pid = rt.pid().map(|p| p.to_string()).unwrap_or("-".into());
    let restarts = rt.restarts.to_string();
    ui.horizontal_wrapped(|ui| {
        let w = ((ui.available_width() - 5.0 * 10.0) / 5.0 - 28.0).clamp(70.0, 160.0);
        stat(ui, sc, "CPU", &cpu, w);
        stat(ui, sc, "Memory", &mem, w);
        stat(ui, sc, "Uptime", &up, w);
        stat(ui, sc, "Restarts", &restarts, w);
        stat(ui, sc, "PID", &pid, w);
    });
    ui.add_space(12.0);

    let before = app.proc_tab;
    segmented(
        ui,
        sc,
        &mut app.proc_tab,
        &[(ProcTab::Logs, "Logs"), (ProcTab::Performance, "Performance"), (ProcTab::Secrets, "Secrets"), (ProcTab::Activity, "Activity")],
    );
    // refetch when the tab opens, or when the process restarted with (maybe) different secrets
    let runs = app.rt[id].runs;
    if app.proc_tab == ProcTab::Activity && (before != ProcTab::Activity || app.audit_gen.get(id) != Some(&runs)) {
        app.audit_gen.insert(id.to_string(), runs);
        app.fetch_audit(id);
    }
    ui.add_space(10.0);

    match app.proc_tab {
        ProcTab::Logs => {
            let logs = app.rt[id].logs.clone();
            log_view(ui, sc, &logs, &mut app.log_view, id);
        }
        ProcTab::Performance => performance(app, ui, sc, id),
        ProcTab::Secrets => secrets_tab(app, ui, sc, id, &def),
        ProcTab::Activity => activity_tab(app, ui, sc, id),
    }
}

fn performance(app: &mut App, ui: &mut egui::Ui, sc: &Scheme, id: &str) {
    let now = app.now();
    let rt = &app.rt[id];
    if rt.cpu.is_empty() {
        empty_state(ui, sc, "📈", "No samples yet", "Start the process and the graphs fill in, one point a second.");
        return;
    }
    let rel = |q: &std::collections::VecDeque<[f64; 2]>| -> Vec<[f64; 2]> { q.iter().map(|p| [p[0] - now, p[1]]).collect() };
    let h = ((ui.available_height() - 70.0) / 2.0).max(120.0);
    let peak_cpu = rt.cpu.iter().map(|p| p[1]).fold(0.0, f64::max);
    let peak_mem = rt.mem.iter().map(|p| p[1]).fold(0.0, f64::max);

    // window grows with the data (min a minute) and y follows the peak, otherwise
    // a 3% process is just a flat line at the bottom of a 0-100 chart
    let span = rt.cpu.front().map(|p| now - p[0]).unwrap_or(0.0).max(60.0);
    for (title, data, color, max, unit, peak) in [
        ("CPU", rel(&rt.cpu), sc.primary, (peak_cpu * 1.3).max(5.0), "%", format!("peak {peak_cpu:.1}%")),
        ("Memory", rel(&rt.mem), sc.tertiary, (peak_mem * 1.3).max(8.0), " MB", format!("peak {}", fmt_bytes(peak_mem))),
    ] {
        sc.card(1).corner_radius(CornerRadius::same(16)).inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(title).strong().color(sc.on_surface));
                ui.label(RichText::new(peak).small().color(sc.on_surface_variant));
            });
            Plot::new((id, title))
                .height(h - 40.0)
                .allow_drag(false)
                .allow_zoom(false)
                .allow_scroll(false)
                .allow_boxed_zoom(false)
                .allow_double_click_reset(false)
                .show_background(false)
                .include_x(-span)
                .include_x(0.0)
                .include_y(0.0)
                .include_y(max)
                .x_axis_formatter(|m, _| format!("{:.0}s", m.value))
                .y_axis_formatter(move |m, _| format!("{:.0}{unit}", m.value))
                .show(ui, |p| {
                    p.line(Line::new(PlotPoints::from(data)).color(color).width(2.0f32).fill(0.0f32).fill_alpha(0.18f32));
                });
        });
        ui.add_space(8.0);
    }
    ui.label(RichText::new("Summed over the whole process tree (children included). 100% = one core.").small().color(sc.on_surface_variant));
}

fn secrets_tab(app: &mut App, ui: &mut egui::Ui, sc: &Scheme, id: &str, def: &ProcDef) {
    if def.secrets.is_empty() {
        empty_state(ui, sc, "🔑", "No secrets mapped", "Edit the process to pull env vars from a 1Claw vault.");
        return;
    }
    let rt = &app.rt[id];
    ui.label(
        RichText::new("Fetched when the process starts, passed only to its environment, and masked as •••• anywhere they show up in the logs.")
            .color(sc.on_surface_variant),
    );
    ui.add_space(8.0);
    for r in &def.secrets {
        let inj = rt.injected.iter().find(|i| i.env == r.env.trim()).filter(|_| rt.child.is_some());
        sc.card(1).corner_radius(CornerRadius::same(16)).inner_margin(Margin::symmetric(14, 10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(&r.env).monospace().strong().color(sc.on_surface));
                ui.label(RichText::new("from").small().color(sc.outline));
                ui.label(RichText::new(format!("{} / {}", r.vault, r.path)).monospace().color(sc.on_surface_variant));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match inj {
                    Some(i) => {
                        chip(ui, sc, "injected", Role::Success);
                        chip(ui, sc, &format!("v{}", i.version), Role::Secondary);
                    }
                    None => chip(ui, sc, "not loaded", Role::Neutral),
                });
            });
        });
        ui.add_space(6.0);
    }
    ui.add_space(6.0);
    let watch = if def.restart_on_rotation {
        format!("🔄 Restarts automatically when one of these gets a new version (checked every {}s).", app.settings.rotation_check_secs)
    } else {
        "Rotation watch is off for this process.".into()
    };
    ui.label(RichText::new(watch).small().color(sc.on_surface_variant));
}

fn activity_tab(app: &mut App, ui: &mut egui::Ui, sc: &Scheme, id: &str) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("1Claw audit events for the secrets this process uses").color(sc.on_surface_variant));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(sc.text_button("⟳ Refresh", sc.primary)).clicked() {
                app.fetch_audit(id);
            }
        });
    });
    ui.add_space(6.0);
    match app.audit.get(id) {
        None | Some(None) => {
            ui.add(egui::Spinner::new().color(sc.primary));
        }
        Some(Some(list)) if list.is_empty() => {
            let sub = if app.client.is_none() { "Connect to 1Claw first." } else { "Start the process so Clawrun knows which secrets to look up." };
            empty_state(ui, sc, "📜", "No events", sub);
        }
        Some(Some(list)) => {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                egui::Grid::new(("audit", id)).striped(true).num_columns(4).spacing([24.0, 10.0]).show(ui, |ui| {
                    for h in ["When", "Action", "Actor", "IP"] {
                        ui.label(RichText::new(h).small().strong().color(sc.on_surface_variant));
                    }
                    ui.end_row();
                    for e in list {
                        ui.label(RichText::new(e.created_at.get(..16).unwrap_or(&e.created_at).replace('T', " ")).color(sc.on_surface_variant));
                        let role = if e.action.contains("delete") {
                            Role::Error
                        } else if e.action.contains("read") {
                            Role::Secondary
                        } else if e.action.contains("create") {
                            Role::Primary
                        } else {
                            Role::Tertiary
                        };
                        chip(ui, sc, &e.action, role);
                        ui.label(format!("{} {}", e.actor_type, e.actor_id.get(..8).unwrap_or(&e.actor_id))).on_hover_text(&e.actor_id);
                        ui.label(RichText::new(e.ip_address.as_deref().unwrap_or("-")).monospace().color(sc.on_surface_variant));
                        ui.end_row();
                    }
                });
            });
        }
    }
}

fn editor(app: &mut App, ctx: &egui::Context, sc: &Scheme) {
    let Some((mut d, is_new)) = app.editor.take() else { return };
    let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let mut save = false;
    let title = if is_new { "New process" } else { "Edit process" };
    dialog(ctx, title, |ui| {
        ui.set_width(620.0);
        egui::ScrollArea::vertical().max_height(ctx.screen_rect().height() - 170.0).show(ui, |ui| {
            field_label(ui, sc, "Name");
            ui.add(egui::TextEdit::singleline(&mut d.name).margin(Margin::symmetric(14, 10)).desired_width(f32::INFINITY));
            field_label(ui, sc, "Command");
            ui.add(
                egui::TextEdit::singleline(&mut d.command)
                    .hint_text("npm start, python app.py, ./server --port 8080 ...")
                    .font(egui::TextStyle::Monospace)
                    .margin(Margin::symmetric(14, 10))
                    .desired_width(f32::INFINITY),
            );
            field_label(ui, sc, "Working directory");
            ui.add(
                egui::TextEdit::singleline(&mut d.cwd)
                    .hint_text("~/code/my-app (empty = where clawrun was started)")
                    .font(egui::TextStyle::Monospace)
                    .margin(Margin::symmetric(14, 10))
                    .desired_width(f32::INFINITY),
            );

            ui.add_space(8.0);
            ui.label(RichText::new("Secrets from 1Claw").strong().color(sc.on_surface));
            ui.label(RichText::new("Injected as env vars at start. Only the path is saved, never the value.").small().color(sc.on_surface_variant));
            let mut rows = std::mem::take(&mut d.secrets);
            secret_rows(app, ui, sc, &mut rows, &d.id);
            d.secrets = rows;

            ui.add_space(8.0);
            ui.label(RichText::new("Plain env vars").strong().color(sc.on_surface));
            let mut remove = None;
            for (i, e) in d.env.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut e.key).hint_text("KEY").font(egui::TextStyle::Monospace).margin(Margin::symmetric(10, 8)).desired_width(150.0));
                    ui.label(RichText::new("=").color(sc.outline));
                    ui.add(
                        egui::TextEdit::singleline(&mut e.value)
                            .hint_text("value")
                            .font(egui::TextStyle::Monospace)
                            .margin(Margin::symmetric(10, 8))
                            .desired_width(ui.available_width() - 40.0),
                    );
                    if ui.add(sc.text_button("🗑", sc.on_surface_variant)).clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                d.env.remove(i);
            }
            if ui.add(sc.text_button("+ Add variable", sc.primary)).clicked() {
                d.env.push(EnvVar::default());
            }

            ui.add_space(8.0);
            ui.checkbox(&mut d.autorestart, "Restart if it crashes (with backoff)");
            ui.checkbox(&mut d.restart_on_rotation, "Restart when one of its secrets is rotated in 1Claw");
            ui.checkbox(&mut d.autostart, "Start when Clawrun opens");
        });

        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ok = !d.name.trim().is_empty() && !d.command.trim().is_empty();
            save = ui.add_enabled(ok, sc.filled("Save")).clicked();
            if ui.add(sc.text_button("Cancel", sc.primary)).clicked() {
                close = true;
            }
        });
    });

    if save {
        d.name = d.name.trim().to_string();
        let id = d.id.clone();
        match app.project.processes.iter_mut().find(|p| p.id == d.id) {
            Some(p) => {
                let changed = p.command != d.command || p.cwd != d.cwd || p.env != d.env || p.secrets != d.secrets;
                *p = d;
                if changed && app.rt.get(&id).is_some_and(|r| r.child.is_some()) {
                    app.info("Saved. Restart the process to pick up the changes");
                }
            }
            None => app.project.processes.push(d),
        }
        app.selected = Some(id);
        app.save_project();
    } else if !close {
        app.editor = Some((d, is_new));
    }
}

fn delete_dialog(app: &mut App, ctx: &egui::Context, sc: &Scheme) {
    let Some(id) = app.confirm_delete.clone() else { return };
    let name = app.project.processes.iter().find(|p| p.id == id).map(|p| p.name.clone()).unwrap_or_default();
    let mut keep = !ctx.input(|i| i.key_pressed(egui::Key::Escape));
    dialog(ctx, "Delete process?", |ui| {
        ui.label(RichText::new(format!("\"{name}\" will be stopped and removed from Clawrun. Nothing in 1Claw is touched.")).color(sc.on_surface_variant));
        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(sc.danger("Delete")).clicked() {
                app.rt.remove(&id); // Drop kills it
                app.project.processes.retain(|p| p.id != id);
                app.selected = app.project.processes.first().map(|p| p.id.clone());
                app.save_project();
                keep = false;
            }
            if ui.add(sc.text_button("Cancel", sc.primary)).clicked() {
                keep = false;
            }
        });
    });
    if !keep {
        app.confirm_delete = None;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn env_names() {
        assert_eq!(super::guess_env("stripe/secret-key"), "STRIPE_SECRET_KEY");
    }
}
