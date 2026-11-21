use crate::api::{AuditEvent, Client, Credentials, KeyKind};
use crate::config::{ProcDef, Project, Settings};
use crate::metrics::Sampler;
use crate::supervisor::{self, Injected, Program, Runtime, Spec, Tick};
use crate::theme::{self, Role, chip, dialog, field_label, segmented};
use eframe::egui::{self, Color32, CornerRadius, Margin, RichText};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(PartialEq, Clone, Copy)]
pub enum Tab {
    Processes,
    Snippets,
}

#[derive(PartialEq, Clone, Copy)]
pub enum ProcTab {
    Logs,
    Performance,
    Secrets,
    Activity,
}

pub enum Msg {
    Connected(Result<(Client, Vec<String>), String>),
    Started { id: String, generation: u64, res: Result<(Child, Vec<Injected>), String> },
    Versions { vault_id: String, versions: HashMap<String, i64> },
    Audit { id: String, res: Result<Vec<AuditEvent>, String> },
    Paths { vault: String, paths: Vec<String> },
    Error(String),
}

pub struct Toast {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

/// Which bits of the log to show + the search box.
pub struct LogView {
    pub filter: String,
    pub out: bool,
    pub err: bool,
    pub sys: bool,
    pub follow: bool,
}

impl Default for LogView {
    fn default() -> Self {
        Self { filter: String::new(), out: true, err: true, sys: true, follow: true }
    }
}

pub struct Account {
    pub open: bool,
    pub key: String,
    pub error: Option<String>,
    pub busy: bool,
}

pub struct App {
    pub settings: Settings,
    pub project: Project,
    pub project_path: PathBuf,
    pub client: Option<Arc<Mutex<Client>>>,
    pub vault_names: Vec<String>,
    pub paths: HashMap<String, Vec<String>>,
    pub account: Account,

    pub rt: HashMap<String, Runtime>,
    pub tx: Sender<Msg>,
    rx: Receiver<Msg>,
    pub ctx: egui::Context,
    sampler: Sampler,
    t0: Instant,
    last_rotation_check: Instant,
    autostarted: bool,
    sized: bool,

    pub tab: Tab,
    pub selected: Option<String>,
    pub proc_tab: ProcTab,
    pub proc_filter: String,
    pub log_view: LogView,
    pub snippet_log: LogView,
    pub editor: Option<(ProcDef, bool)>,
    pub confirm_delete: Option<String>,
    pub audit: HashMap<String, Option<Vec<AuditEvent>>>,
    pub selected_snippet: Option<String>,
    pub snippets_dirty: Option<Instant>,

    pub toast: Option<Toast>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        let settings = Settings::load();
        theme::apply(&cc.egui_ctx, settings.seed, settings.dark);
        // `clawrun path/to/clawrun.json` opens a project file instead of the default one
        let project_path = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(crate::config::default_project_path);
        let project = Project::load(&project_path);
        let (tx, rx) = channel();
        let mut app = Self {
            settings,
            selected: project.processes.first().map(|p| p.id.clone()),
            selected_snippet: project.snippets.first().map(|p| p.id.clone()),
            project,
            project_path,
            client: None,
            vault_names: vec![],
            paths: HashMap::new(),
            account: Account { open: false, key: String::new(), error: None, busy: false },
            rt: HashMap::new(),
            tx,
            rx,
            ctx: cc.egui_ctx.clone(),
            sampler: Sampler::new(),
            t0: Instant::now(),
            last_rotation_check: Instant::now(),
            autostarted: false,
            sized: false,
            tab: Tab::Processes,
            proc_tab: ProcTab::Logs,
            proc_filter: String::new(),
            log_view: LogView::default(),
            snippet_log: LogView::default(),
            editor: None,
            confirm_delete: None,
            audit: HashMap::new(),
            snippets_dirty: None,
            toast: None,
        };

        let env_key = std::env::var("ONECLAW_API_KEY").ok().filter(|k| !k.trim().is_empty());
        if let Some(k) = env_key.or_else(|| (app.settings.remember && !app.settings.api_key.is_empty()).then(|| app.settings.api_key.clone())) {
            if k.starts_with("ocv_") {
                app.settings.kind = KeyKind::Agent;
            }
            app.connect(k, false);
        } else {
            // nothing to wait for, kick off autostart processes that don't need secrets
            app.autostart(false);
        }
        app
    }

    pub fn info(&mut self, t: impl Into<String>) {
        self.toast = Some(Toast { text: t.into(), error: false, at: Instant::now() });
    }

    pub fn error(&mut self, t: impl Into<String>) {
        self.toast = Some(Toast { text: t.into(), error: true, at: Instant::now() });
    }

    pub fn save_project(&self) {
        self.project.save(&self.project_path);
    }

    pub fn now(&self) -> f64 {
        self.t0.elapsed().as_secs_f64()
    }

    // --- 1Claw account ---

    pub fn connect(&mut self, key: String, from_form: bool) {
        if from_form {
            self.settings.api_key = if self.settings.remember { key.trim().to_string() } else { String::new() };
            self.settings.save();
        }
        self.account.busy = true;
        self.account.error = None;
        let creds = Credentials {
            base_url: std::env::var("ONECLAW_BASE_URL").unwrap_or_else(|_| self.settings.base_url.trim().to_string()),
            kind: self.settings.kind,
            api_key: key.trim().to_string(),
            agent_id: Some(self.settings.agent_id.trim().to_string()),
        };
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let mut c = Client::new(creds);
            let res = c.login().and_then(|_| c.vaults()).map(|v| (c, v.into_iter().map(|v| v.name).collect()));
            let _ = tx.send(Msg::Connected(res));
            ctx.request_repaint();
        });
    }

    pub fn disconnect(&mut self) {
        self.client = None;
        self.vault_names.clear();
        self.paths.clear();
        self.settings.api_key.clear();
        self.settings.save();
        self.info("Disconnected from 1Claw");
    }

    /// Run something against the api on a worker thread.
    pub fn bg(&self, f: impl FnOnce(&mut Client) -> Msg + Send + 'static) {
        let Some(c) = self.client.clone() else { return };
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let m = f(&mut c.lock().unwrap());
            let _ = tx.send(m);
            ctx.request_repaint();
        });
    }

    pub fn fetch_paths(&mut self, vault: &str) {
        if self.paths.contains_key(vault) || self.client.is_none() {
            return;
        }
        self.paths.insert(vault.to_string(), vec![]); // mark as requested
        let vault = vault.to_string();
        self.bg(move |c| {
            let r = c.vaults().and_then(|vs| {
                let v = vs.into_iter().find(|v| v.name == vault).ok_or("vault not found")?;
                c.secrets(&v.id)
            });
            match r {
                Ok(list) => Msg::Paths { vault, paths: list.into_iter().map(|s| s.path).collect() },
                Err(e) => Msg::Error(e),
            }
        });
    }

    pub fn fetch_audit(&mut self, id: &str) {
        let ids: Vec<String> = self.rt.get(id).map(|r| r.injected.iter().map(|i| i.secret_id.clone()).collect()).unwrap_or_default();
        self.audit.insert(id.to_string(), None);
        let id = id.to_string();
        if ids.is_empty() || self.client.is_none() {
            self.audit.insert(id, Some(vec![]));
            return;
        }
        self.bg(move |c| {
            let mut all = vec![];
            for sid in ids {
                match c.audit(Some(&sid), 25) {
                    Ok(mut ev) => all.append(&mut ev),
                    Err(e) => return Msg::Audit { id, res: Err(e) },
                }
            }
            all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            Msg::Audit { id, res: Ok(all) }
        });
    }

    // --- process control ---

    pub fn start(&mut self, id: &str) {
        let (spec, refs, temp) = if let Some(p) = self.project.processes.iter().find(|p| p.id == id) {
            let env = p.env.iter().filter(|e| !e.key.trim().is_empty()).map(|e| (e.key.trim().to_string(), e.value.clone())).collect();
            (Spec { program: Program::Shell(p.command.clone()), cwd: p.cwd.clone(), env }, p.secrets.clone(), None)
        } else if let Some(s) = self.project.snippets.iter().find(|s| s.id == id) {
            let (args, ext) = interpreter(&s.interpreter);
            let file = std::env::temp_dir().join(format!("clawrun-{}.{}", s.id, ext));
            if let Err(e) = std::fs::write(&file, &s.code) {
                self.error(format!("couldn't write temp file: {e}"));
                return;
            }
            let mut args = args;
            args.push(file.to_string_lossy().into_owned());
            (Spec { program: Program::Args(args), cwd: s.cwd.clone(), env: vec![] }, s.secrets.clone(), Some(file))
        } else {
            return;
        };

        let rt = self.rt.entry(id.to_string()).or_default();
        if rt.child.is_some() {
            return;
        }
        let generation = rt.begin_start();
        rt.temp_file = temp;
        let n = refs.iter().filter(|r| !r.env.trim().is_empty()).count();
        if n > 0 {
            rt.log(format!("fetching {n} secret{} from 1Claw...", if n == 1 { "" } else { "s" }));
        } else {
            rt.log("starting...");
        }

        let logs = rt.logs.clone();
        let client = self.client.clone();
        let (tx, ctx, id) = (self.tx.clone(), self.ctx.clone(), id.to_string());
        std::thread::spawn(move || {
            let res = supervisor::resolve(client.as_ref(), &refs).and_then(|r| {
                let values: Vec<String> = r.vars.iter().map(|(_, v)| v.clone()).collect();
                logs.lock().unwrap().set_redact(&values);
                let mut spec = spec;
                spec.env.extend(r.vars);
                supervisor::spawn(&spec, &logs, &ctx).map(|c| (c, r.injected))
            });
            let _ = tx.send(Msg::Started { id, generation, res });
            ctx.request_repaint();
        });
    }

    pub fn stop(&mut self, id: &str) {
        if let Some(rt) = self.rt.get_mut(id) {
            rt.stop(false);
        }
    }

    pub fn restart(&mut self, id: &str) {
        match self.rt.get_mut(id) {
            Some(rt) if rt.child.is_some() => rt.stop(true),
            _ => self.start(id),
        }
    }

    fn autostart(&mut self, connected: bool) {
        if self.autostarted && connected {
            return;
        }
        let ids: Vec<String> = self
            .project
            .processes
            .iter()
            .filter(|p| p.autostart && (connected || p.secrets.is_empty()))
            .filter(|p| !self.rt.get(&p.id).is_some_and(|r| r.status.is_live()))
            .map(|p| p.id.clone())
            .collect();
        for id in ids {
            self.start(&id);
        }
        if connected {
            self.autostarted = true;
        }
    }

    fn check_rotation(&mut self) {
        self.last_rotation_check = Instant::now();
        let mut vaults: Vec<String> = vec![];
        for p in self.project.processes.iter().filter(|p| p.restart_on_rotation) {
            if let Some(rt) = self.rt.get(&p.id).filter(|r| r.child.is_some()) {
                for i in &rt.injected {
                    if !vaults.contains(&i.vault_id) {
                        vaults.push(i.vault_id.clone());
                    }
                }
            }
        }
        // listing secrets only returns metadata, so this doesn't count as a read in the audit log
        for v in vaults {
            self.bg(move |c| match c.secrets(&v) {
                Ok(list) => Msg::Versions { vault_id: v, versions: list.into_iter().map(|s| (s.path, s.version)).collect() },
                Err(e) => Msg::Error(format!("rotation check: {e}")),
            });
        }
    }

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::Connected(Ok((client, vaults))) => {
                self.client = Some(Arc::new(Mutex::new(client)));
                self.vault_names = vaults;
                self.account.busy = false;
                self.account.open = false;
                self.account.key.clear();
                self.info(format!("Connected to 1Claw, {} vaults", self.vault_names.len()));
                self.autostart(true);
            }
            Msg::Connected(Err(e)) => {
                self.account.busy = false;
                self.account.error = Some(e.clone());
                if !self.account.open {
                    // came from a saved key, let the user see why
                    self.account.open = true;
                }
                self.autostart(false);
            }
            Msg::Started { id, generation, res } => {
                let Some(rt) = self.rt.get_mut(&id) else { return };
                if rt.generation != generation {
                    // stopped while we were still fetching secrets
                    if let Ok((mut child, _)) = res {
                        supervisor::terminate(&mut child, true);
                        std::thread::spawn(move || child.wait());
                    }
                    return;
                }
                match res {
                    Ok((child, injected)) => rt.started(child, injected),
                    Err(e) => {
                        rt.failed(e.clone());
                        if let Some(f) = rt.temp_file.take() {
                            let _ = std::fs::remove_file(f);
                        }
                        self.error(e);
                    }
                }
            }
            Msg::Versions { vault_id, versions } => {
                let mut restart = vec![];
                for p in self.project.processes.iter().filter(|p| p.restart_on_rotation) {
                    let Some(rt) = self.rt.get(&p.id).filter(|r| r.child.is_some()) else { continue };
                    for i in rt.injected.iter().filter(|i| i.vault_id == vault_id) {
                        if let Some(&v) = versions.get(&i.path).filter(|v| **v > i.version) {
                            rt.log(format!("🔄 {} ({}/{}) rotated v{} -> v{}, restarting with the new value", i.env, i.vault, i.path, i.version, v));
                            restart.push(p.id.clone());
                            break;
                        }
                    }
                }
                for id in restart {
                    if let Some(name) = self.project.processes.iter().find(|p| p.id == id).map(|p| p.name.clone()) {
                        self.info(format!("Secret rotated, restarting {name}"));
                    }
                    self.restart(&id);
                }
            }
            Msg::Audit { id, res } => match res {
                Ok(ev) => {
                    self.audit.insert(id, Some(ev));
                }
                Err(e) => {
                    self.audit.insert(id, Some(vec![]));
                    self.error(e);
                }
            },
            Msg::Paths { vault, paths } => {
                self.paths.insert(vault, paths);
            }
            Msg::Error(e) => self.error(e),
        }
    }

    fn tick(&mut self) {
        let mut to_start = vec![];
        let mut any_live = false;
        for (id, rt) in self.rt.iter_mut() {
            let auto = self.project.processes.iter().find(|p| &p.id == id).is_some_and(|p| p.autorestart);
            if let Tick::Start = rt.tick(auto) {
                to_start.push(id.clone());
            }
            any_live |= rt.status.is_live();
        }
        for id in to_start {
            self.start(&id);
        }

        if self.sampler.due() {
            let pids: Vec<u32> = self.rt.values().filter_map(|r| r.pid()).collect();
            if !pids.is_empty() {
                let usage = self.sampler.sample(&pids);
                let t = self.now();
                for rt in self.rt.values_mut() {
                    if let Some(u) = rt.pid().and_then(|p| usage.get(&p)) {
                        rt.push_sample(t, u.cpu, u.mem);
                    }
                }
            }
        }

        if self.client.is_some() && self.last_rotation_check.elapsed() >= Duration::from_secs(self.settings.rotation_check_secs.max(5)) {
            self.check_rotation();
        }

        if let Some(t) = self.snippets_dirty {
            if t.elapsed() > Duration::from_millis(800) {
                self.snippets_dirty = None;
                self.save_project();
            }
        }

        // keep polling child status + graphs moving while anything runs
        self.ctx.request_repaint_after(if any_live { Duration::from_millis(250) } else { Duration::from_secs(1) });
    }

    // --- chrome ---

    fn top_bar(&mut self, ctx: &egui::Context) {
        let sc = theme::scheme(ctx);
        egui::TopBottomPanel::top("top")
            .show_separator_line(false)
            .exact_height(60.0)
            .frame(sc.panel(sc.surface_container_low).inner_margin(Margin::symmetric(16, 0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    theme::icon_tile(ui, &sc, "⚡", 36.0);
                    ui.label(RichText::new("Clawrun").size(19.0).color(sc.on_surface));
                    ui.add_space(18.0);
                    let running = self.rt.values().filter(|r| r.child.is_some()).count();
                    let procs = format!("▶  Processes{}", if running > 0 { format!(" · {running}") } else { String::new() });
                    segmented(ui, &sc, &mut self.tab, &[(Tab::Processes, &procs), (Tab::Snippets, "📝  Snippets")]);

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button(RichText::new("🎨").size(16.0).color(sc.on_surface_variant), |ui| self.palette_menu(ui, ctx))
                            .response
                            .on_hover_text("Theme");
                        if self.client.is_some() {
                            let b = egui::Button::new(RichText::new("●  1Claw connected").color(sc.on_success_container))
                                .fill(sc.success_container)
                                .corner_radius(CornerRadius::same(18))
                                .min_size(egui::vec2(0.0, 34.0));
                            if ui.add(b).on_hover_text("Click to manage").clicked() {
                                self.account.open = true;
                            }
                        } else if ui.add(sc.filled("🔑  Connect 1Claw").min_size(egui::vec2(0.0, 36.0))).clicked() {
                            self.account.open = true;
                        }
                        if self.account.busy {
                            ui.add(egui::Spinner::new().color(sc.primary));
                        }
                    });
                });
            });
    }

    fn palette_menu(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let sc = theme::scheme(ctx);
        ui.label(RichText::new("Color").small().color(sc.on_surface_variant));
        ui.horizontal(|ui| {
            for (seed, name) in theme::SEEDS {
                let c = Color32::from_rgb(seed[0], seed[1], seed[2]);
                if theme::swatch(ui, c, *seed == self.settings.seed, sc.on_surface).on_hover_text(*name).clicked() {
                    self.settings.seed = *seed;
                    self.retheme(ctx);
                }
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.selectable_label(!self.settings.dark, "☀ Light").clicked() {
                self.settings.dark = false;
                self.retheme(ctx);
            }
            if ui.selectable_label(self.settings.dark, "🌙 Dark").clicked() {
                self.settings.dark = true;
                self.retheme(ctx);
            }
        });
    }

    fn retheme(&mut self, ctx: &egui::Context) {
        theme::apply(ctx, self.settings.seed, self.settings.dark);
        self.settings.save();
    }

    fn account_dialog(&mut self, ctx: &egui::Context) {
        if !self.account.open {
            return;
        }
        let sc = theme::scheme(ctx);
        let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let mut go = None;
        let mut disconnect = false;
        dialog(ctx, "1Claw account", |ui| {
            ui.set_width(420.0);
            if self.client.is_some() {
                ui.horizontal(|ui| {
                    chip(ui, &sc, "connected", Role::Success);
                    ui.label(RichText::new(format!("{} vaults available", self.vault_names.len())).color(sc.on_surface_variant));
                });
                ui.add_space(6.0);
                ui.label(RichText::new("Secrets are fetched when a process starts and only live in that process's environment.").color(sc.on_surface_variant));
                ui.add_space(14.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(sc.filled("Done")).clicked() {
                        close = true;
                    }
                    if ui.add(sc.text_button("Disconnect", sc.error)).clicked() {
                        disconnect = true;
                    }
                });
                return;
            }

            ui.label(RichText::new("Clawrun pulls each process's secrets from your vaults when it starts.").color(sc.on_surface_variant));
            ui.add_space(10.0);
            segmented(ui, &sc, &mut self.settings.kind, &[(KeyKind::User, "Personal key"), (KeyKind::Agent, "Agent key")]);
            ui.add_space(10.0);
            field_label(ui, &sc, "API key");
            let hint = if self.settings.kind == KeyKind::User { "1ck_..." } else { "ocv_..." };
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.account.key)
                    .password(true)
                    .hint_text(hint)
                    .margin(Margin::symmetric(14, 10))
                    .desired_width(f32::INFINITY),
            );
            if self.settings.kind == KeyKind::Agent {
                field_label(ui, &sc, "Agent ID (optional)");
                ui.add(egui::TextEdit::singleline(&mut self.settings.agent_id).margin(Margin::symmetric(14, 10)).desired_width(f32::INFINITY));
            }
            ui.add_space(4.0);
            ui.checkbox(&mut self.settings.remember, "Remember me")
                .on_hover_text("Keeps the key in the settings file (plain text, readable only by you) so you're connected next time.");
            egui::CollapsingHeader::new(RichText::new("Advanced").color(sc.on_surface_variant)).show(ui, |ui| {
                field_label(ui, &sc, "API base URL");
                ui.add(egui::TextEdit::singleline(&mut self.settings.base_url).margin(Margin::symmetric(14, 10)).desired_width(f32::INFINITY));
                ui.horizontal(|ui| {
                    field_label(ui, &sc, "Check for rotated secrets every");
                    ui.add(egui::DragValue::new(&mut self.settings.rotation_check_secs).range(5..=3600).suffix(" s"));
                });
            });
            if let Some(e) = &self.account.error {
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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ok = !self.account.key.trim().is_empty() && !self.account.busy;
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let label = if self.account.busy { "Connecting..." } else { "Connect" };
                if (ui.add_enabled(ok, sc.filled(label)).clicked() || enter) && ok {
                    go = Some(self.account.key.clone());
                }
                if ui.add(sc.text_button("Cancel", sc.primary)).clicked() {
                    close = true;
                }
            });
        });
        if let Some(k) = go {
            self.connect(k, true);
        }
        if disconnect {
            self.disconnect();
            close = true;
        }
        if close {
            self.account.open = false;
            self.account.key.clear();
            self.settings.save();
        }
    }

    fn snackbar(&mut self, ctx: &egui::Context) {
        let Some(t) = &self.toast else { return };
        let ttl = Duration::from_secs(if t.error { 8 } else { 4 });
        if t.at.elapsed() >= ttl {
            self.toast = None;
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
        let sc = theme::scheme(ctx);
        let (bg, fg) = if t.error { (sc.error_container, sc.on_error_container) } else { (sc.on_surface, sc.surface) };
        let mut dismiss = false;
        egui::Area::new(egui::Id::new("snackbar"))
            .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -20.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(bg)
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(16, 12))
                    .shadow(ctx.style().visuals.popup_shadow)
                    .show(ui, |ui| {
                        ui.set_max_width(560.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&t.text).color(fg));
                            ui.add_space(12.0);
                            let accent = if t.error { sc.error } else { sc.primary_container };
                            if ui.add(sc.text_button("Dismiss", accent)).clicked() {
                                dismiss = true;
                            }
                        });
                    });
            });
        if dismiss {
            self.toast = None;
        }
    }
}

/// interpreter name -> (command, file extension)
pub const INTERPRETERS: &[(&str, &str, &str)] = &[
    ("sh", "sh", "sh"),
    ("bash", "bash", "sh"),
    ("python", "python3", "py"),
    ("node", "node", "js"),
    ("deno", "deno run -A", "ts"),
    ("ruby", "ruby", "rb"),
    ("pwsh", "pwsh -NoProfile -File", "ps1"),
];

pub fn interpreter(name: &str) -> (Vec<String>, &'static str) {
    let (_, cmd, ext) = INTERPRETERS.iter().find(|(n, ..)| *n == name).unwrap_or(&INTERPRETERS[0]);
    (cmd.split_whitespace().map(str::to_string).collect(), ext)
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(m) = self.rx.try_recv() {
            self.handle(m);
        }
        self.tick();

        // KWin likes to ignore the initial size, ask again once we're on screen
        if !self.sized {
            self.sized = true;
            let r = ctx.screen_rect();
            if r.width() < 800.0 || r.height() < 500.0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(1240.0, 780.0)));
            }
        }

        self.top_bar(ctx);
        match self.tab {
            Tab::Processes => crate::ui_procs::show(self, ctx),
            Tab::Snippets => crate::ui_snippets::show(self, ctx),
        }
        self.account_dialog(ctx);
        self.snackbar(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // be nice first, then Runtime's Drop kills whatever is left
        let mut any = false;
        for rt in self.rt.values_mut() {
            if let Some(c) = rt.child.as_mut() {
                supervisor::terminate(c, false);
                any = true;
            }
        }
        if any {
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(2) && self.rt.values_mut().any(|r| r.child.as_mut().is_some_and(|c| matches!(c.try_wait(), Ok(None)))) {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        self.save_project();
    }
}
