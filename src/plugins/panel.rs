//! The plugin manager, drawn in the browser's right-hand drawer.

use super::manifest::{Manifest, OptionKind};
use super::permissions::{self, Requested, Risk};
use super::registry::{Registry, Source};
use super::Shared;
use crate::egui_chrome::Colors;
use serde_json::Value;
use std::time::{Duration, Instant};

/// Things only the browser can do for the panel.
#[derive(Debug, PartialEq)]
pub enum PluginsEvent {
    /// Enabled plugins, settings or files changed: rebuild the web views'
    /// init scripts and offer to reload the open tabs.
    ScriptsChanged,
    OpenUrl(String),
}

enum Pending {
    Ok { path: String, manifest: Box<Manifest> },
    Failed(Vec<String>),
}

enum Action {
    Toggle(String, bool),
    Setting(String, String, Value),
    ConfirmRemove(String),
    Remove(String),
    CancelRemove,
    Install(String),
    Reload,
    AddDev(String),
    CancelInstall,
    OpenUrl(String),
}

pub struct PluginsPanel {
    reg: Shared,
    install_path: String,
    dev_path: String,
    status: Option<(bool, String)>,
    pending: Option<Pending>,
    confirm_remove: Option<String>,
    last_poll: Instant,
    seen_generation: u64,
}

impl Default for PluginsPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginsPanel {
    pub fn new() -> Self {
        Self::with_registry(super::shared())
    }

    pub fn with_registry(reg: Shared) -> Self {
        let seen = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner).generation();
        Self {
            reg,
            install_path: String::new(),
            dev_path: String::new(),
            status: None,
            pending: None,
            confirm_remove: None,
            last_poll: Instant::now(),
            seen_generation: seen,
        }
    }

    /// Draws the panel body (the drawer draws title and close button).
    pub fn ui(&mut self, ui: &mut egui::Ui, c: &Colors) -> Vec<PluginsEvent> {
        let reg = self.reg.clone();
        let mut reg = reg.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.last_poll.elapsed() >= Duration::from_secs(1) {
            self.last_poll = Instant::now();
            if reg.poll_changes() {
                self.status = Some((true, "Plugin folder changed: reloaded.".into()));
            }
            // Check again soon even if the pointer is idle.
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        let mut actions = Vec::new();
        self.draw(ui, c, &reg, &mut actions);
        let mut events = Vec::new();
        for a in actions {
            events.extend(self.apply(a, &mut reg));
        }
        if reg.generation() != self.seen_generation {
            self.seen_generation = reg.generation();
            events.push(PluginsEvent::ScriptsChanged);
        }
        events
    }

    fn apply(&mut self, a: Action, reg: &mut Registry) -> Option<PluginsEvent> {
        let say = |s: &mut Self, r: Result<String, String>| s.status = Some(match r { Ok(m) => (true, m), Err(m) => (false, m) });
        match a {
            Action::Toggle(id, on) => {
                let r = reg.set_enabled(&id, on).map(|()| String::new());
                if r.is_err() {
                    say(self, r);
                }
            }
            Action::Setting(id, k, v) => {
                if let Err(e) = reg.set_setting(&id, &k, v) {
                    say(self, Err(e));
                }
            }
            Action::ConfirmRemove(id) => self.confirm_remove = Some(id),
            Action::CancelRemove => self.confirm_remove = None,
            Action::Remove(id) => {
                self.confirm_remove = None;
                say(self, reg.remove(&id).map(|()| format!("Removed {id}.")));
            }
            Action::AddDev(path) => {
                let r = reg.add_dev_folder(std::path::Path::new(&path)).map(|n| format!("Added folder with {n} plugin(s)."));
                if r.is_ok() {
                    self.dev_path.clear();
                }
                say(self, r);
            }
            Action::CancelInstall => self.pending = None,
            Action::OpenUrl(u) => return Some(PluginsEvent::OpenUrl(u)),
            Action::Reload => {
                reg.reload();
                self.status = Some((true, "Reloaded all plugins.".into()));
            }
            Action::Install(path) => {
                self.pending = None;
                let r = reg.install_from_folder(std::path::Path::new(&path)).map(|id| format!("Installed {id}.")).map_err(|e| e.join("\n"));
                if r.is_ok() {
                    self.install_path.clear();
                }
                say(self, r);
            }
        }
        None
    }

    fn draw(&mut self, ui: &mut egui::Ui, c: &Colors, reg: &Registry, actions: &mut Vec<Action>) {
        let muted = |t: &str| egui::RichText::new(t).size(12.0).color(c.muted);
        let danger = c.danger;

        // ---- add ----
        ui.label(muted("Install a plugin folder (copied into your settings), or add a folder of plugins you are developing (used in place, reloaded when files change)."));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.install_path).hint_text("Plugin folder to install").desired_width(ui.available_width() - 70.0));
            if ui.button("Install").clicked() {
                let path = self.install_path.trim().trim_matches('"').to_string();
                self.pending = Some(match Registry::preview(std::path::Path::new(&path)) {
                    Ok(manifest) => Pending::Ok { path, manifest: Box::new(manifest) },
                    Err(e) => Pending::Failed(e),
                });
            }
        });
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.dev_path).hint_text("Development folder").desired_width(ui.available_width() - 70.0));
            if ui.button("Add").clicked() {
                let p = self.dev_path.trim().trim_matches('"').to_string();
                actions.push(Action::AddDev(p));
            }
        });
        if ui.button("Reload all").clicked() {
            actions.push(Action::Reload);
        }
        if let Some((ok, msg)) = &self.status
            && !msg.is_empty()
        {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(msg).size(12.5).color(if *ok { c.accent } else { danger }));
        }

        // ---- install confirmation ----
        match &self.pending {
            Some(Pending::Ok { path, manifest }) => {
                ui.add_space(6.0);
                egui::Frame::group(ui.style()).fill(c.field_bg).show(ui, |ui| {
                    ui.label(egui::RichText::new(format!("Install {} {}?", manifest.name, manifest.version)).strong().color(c.text));
                    ui.label(muted(&format!("{} - API {}.{}", manifest.id, manifest.api.0, manifest.api.1)));
                    permission_list(ui, c, manifest);
                    ui.horizontal(|ui| {
                        if ui.button("Install").clicked() {
                            actions.push(Action::Install(path.clone()));
                        }
                        if ui.button("Cancel").clicked() {
                            actions.push(Action::CancelInstall);
                        }
                    });
                });
            }
            Some(Pending::Failed(errors)) => {
                ui.add_space(6.0);
                ui.label(egui::RichText::new("This folder cannot be installed:").color(danger));
                for e in errors {
                    ui.label(egui::RichText::new(format!("- {e}")).size(12.0).color(danger));
                }
            }
            None => {}
        }

        ui.add_space(8.0);
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (source, title) in [(Source::Builtin, "Built-in"), (Source::Installed, "Installed"), (Source::Dev, "Development")] {
                let list: Vec<_> = reg.entries().iter().filter(|e| e.source == source).collect();
                if list.is_empty() {
                    continue;
                }
                ui.add_space(6.0);
                ui.label(egui::RichText::new(title).size(13.0).strong().color(c.text));
                for e in list {
                    self.entry(ui, c, reg, e, actions);
                }
            }
            ui.add_space(10.0);
            api_reference(ui, c);
        });
    }

    fn entry(&mut self, ui: &mut egui::Ui, c: &Colors, reg: &Registry, e: &super::registry::Entry, actions: &mut Vec<Action>) {
        let danger = c.danger;
        let Some(m) = e.manifest() else {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(egui::RichText::new(e.dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default()).size(12.0).color(c.text));
                if let Err(errors) = &e.loaded {
                    for err in errors {
                        ui.label(egui::RichText::new(format!("- {err}")).size(12.0).color(danger));
                    }
                }
                if ui.small_button("Forget this folder entry").clicked() {
                    actions.push(Action::Remove(e.id.clone()));
                }
            });
            return;
        };
        let mut on = reg.is_enabled(&e.id);
        ui.horizontal(|ui| {
            if ui.checkbox(&mut on, "").on_hover_text("Enable or disable").changed() {
                actions.push(Action::Toggle(e.id.clone(), on));
            }
            ui.label(egui::RichText::new(&m.name).size(13.5).color(c.text));
            ui.label(egui::RichText::new(format!("v{}", m.version)).size(11.5).color(c.muted));
        });
        // What a plugin takes over is shown without opening its details.
        if m.has(super::permissions::Permission::ClipboardHistory) {
            let state = if on { "Records everything you copy, here and in other apps (not passwords)." } else { "Would record everything you copy (turned off)." };
            ui.label(egui::RichText::new(format!("Clipboard history: {state}")).size(11.5).strong().color(danger));
        }
        if on {
            let (active, conflicts) = super::shortcuts::resolve(reg);
            for a in active.iter().filter(|a| a.plugin == m.id) {
                if let Some(what) = a.overrides {
                    let label = format!("Takes {} (the browser's {what})", a.combo.label());
                    ui.label(egui::RichText::new(label).size(11.5).color(c.warning));
                }
            }
            for x in conflicts.iter().filter(|x| x.plugin == m.id) {
                let label = format!("{} is not active: {} already uses it", x.combo.label(), x.winner);
                ui.label(egui::RichText::new(label).size(11.5).color(danger));
            }
        }
        egui::CollapsingHeader::new(egui::RichText::new("Details").size(12.0).color(c.muted)).id_salt(&e.id).show(ui, |ui| {
            if !m.description.is_empty() {
                ui.label(egui::RichText::new(&m.description).size(12.5).color(c.text));
            }
            ui.label(egui::RichText::new(format!("{}  -  API {}.{}  -  {}", m.id, m.api.0, m.api.1, if m.author.is_empty() { "unknown author" } else { &m.author })).size(11.5).color(c.muted));
            if let Some(d) = &e.dir {
                ui.label(egui::RichText::new(d.display().to_string()).size(11.0).color(c.muted));
            }
            if !m.homepage.is_empty() && ui.link(&m.homepage).clicked() {
                actions.push(Action::OpenUrl(m.homepage.clone()));
            }
            permission_list(ui, c, m);
            if !m.content_scripts.is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Runs on").size(12.0).strong().color(c.text));
                for cs in &m.content_scripts {
                    let pats: Vec<&str> = cs.matches.iter().map(|p| p.as_str()).collect();
                    ui.label(egui::RichText::new(format!("{} ({})", pats.join(", "), cs.run_at.as_str())).size(11.5).color(c.muted));
                }
            }
            if !m.pages.is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Pages").size(12.0).strong().color(c.text));
                for p in &m.pages {
                    let how = match p.open_as {
                        super::manifest::OpenAs::Tab => "tab".to_string(),
                        super::manifest::OpenAs::Modal => format!("modal {}x{}", p.width, p.height),
                    };
                    let editor = if m.screenshot_editor.as_deref() == Some(p.id.as_str()) { ", screenshot editor" } else { "" };
                    ui.label(egui::RichText::new(format!("{} ({how}{editor})", p.title)).size(11.5).color(c.muted));
                }
            }
            if !m.commands.is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Commands").size(12.0).strong().color(c.text));
                for cmd in &m.commands {
                    let keys = cmd.shortcut.as_ref().map(|k| format!("  {}", k.label())).unwrap_or_default();
                    let when = cmd.when.as_ref().map(|w| format!(" (when {w} is on)")).unwrap_or_default();
                    ui.label(egui::RichText::new(format!("{}{keys}{when}", cmd.title)).size(11.5).color(c.muted));
                }
            }
            if m.wasm.is_some() {
                ui.label(egui::RichText::new("Has a sandboxed WASM module.").size(11.5).color(c.muted));
            }
            settings_form(ui, c, reg, m, actions);
            ui.add_space(4.0);
            if e.source != Source::Builtin {
                if self.confirm_remove.as_deref() == Some(&e.id) {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Remove this plugin and its data?").color(danger));
                        if ui.button("Remove").clicked() {
                            actions.push(Action::Remove(e.id.clone()));
                        }
                        if ui.button("Keep").clicked() {
                            actions.push(Action::CancelRemove);
                        }
                    });
                } else if ui.button(if e.source == Source::Dev { "Remove folder" } else { "Remove" }).clicked() {
                    actions.push(Action::ConfirmRemove(e.id.clone()));
                }
            } else {
                ui.label(egui::RichText::new("Built-in components cannot be removed.").size(11.5).color(c.muted));
            }
        });
    }
}

fn risk_color(r: Risk, c: &Colors) -> egui::Color32 {
    match r {
        Risk::High => c.danger,
        Risk::Medium => c.warning,
        Risk::Low => c.success,
    }
}

fn permission_list(ui: &mut egui::Ui, c: &Colors, m: &Manifest) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new("Permissions").size(12.0).strong().color(c.text));
    if m.permissions.is_empty() {
        ui.label(egui::RichText::new("None: it can only run its own page code.").size(11.5).color(c.muted));
    }
    for r in &m.permissions {
        match r {
            Requested::Api(p) => {
                let i = p.info();
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(i.name).size(12.0).strong().color(risk_color(i.risk, c)));
                    ui.label(egui::RichText::new(i.description).size(11.5).color(c.muted));
                });
            }
            Requested::Host(h) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(h.as_str()).size(12.0).strong().color(risk_color(Risk::Medium, c)));
                    ui.label(egui::RichText::new("Host access (reserved; no API uses it yet).").size(11.5).color(c.muted));
                });
            }
        }
    }
    let allowed: Vec<&str> = permissions::API.iter().filter(|a| permissions::check(&m.permissions, a.wire).is_ok()).map(|a| a.js).collect();
    ui.label(egui::RichText::new(format!("API available: {}", allowed.join("  "))).size(11.0).color(c.muted));
}

fn settings_form(ui: &mut egui::Ui, c: &Colors, reg: &Registry, m: &Manifest, actions: &mut Vec<Action>) {
    if m.options.is_empty() {
        return;
    }
    ui.add_space(4.0);
    ui.label(egui::RichText::new("Settings").size(12.0).strong().color(c.text));
    let current = reg.settings(&m.id);
    for o in &m.options {
        let cur = current.get(&o.key).cloned().unwrap_or(Value::Null);
        let mut new = None;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&o.label).size(12.0).color(c.text));
            match o.kind {
                OptionKind::Bool => {
                    let mut b = cur.as_bool().unwrap_or(false);
                    if ui.checkbox(&mut b, "").changed() {
                        new = Some(Value::Bool(b));
                    }
                }
                OptionKind::String => {
                    let mut s = cur.as_str().unwrap_or("").to_string();
                    if ui.add(egui::TextEdit::singleline(&mut s).desired_width(160.0)).changed() {
                        new = Some(Value::String(s));
                    }
                }
                OptionKind::Number => {
                    let mut x = cur.as_f64().unwrap_or(0.0);
                    let mut d = egui::DragValue::new(&mut x);
                    if let (Some(lo), Some(hi)) = (o.min, o.max) {
                        d = d.range(lo..=hi);
                    }
                    if ui.add(d).changed() {
                        new = serde_json::Number::from_f64(x).map(Value::Number);
                    }
                }
                OptionKind::Choice => {
                    let mut s = cur.as_str().unwrap_or("").to_string();
                    egui::ComboBox::from_id_salt((&m.id, &o.key)).selected_text(&s).show_ui(ui, |ui| {
                        for ch in &o.choices {
                            if ui.selectable_value(&mut s, ch.clone(), ch).clicked() {
                                new = Some(Value::String(ch.clone()));
                            }
                        }
                    });
                }
            }
        });
        if let Some(v) = new {
            actions.push(Action::Setting(m.id.clone(), o.key.clone(), v));
        }
    }
}

/// The page API every plugin can see, with what each call needs.
fn api_reference(ui: &mut egui::Ui, c: &Colors) {
    egui::CollapsingHeader::new(egui::RichText::new(format!("Plugin API {}.{}: methods and permissions", permissions::API_MAJOR, permissions::API_MINOR)).size(12.5).color(c.text)).show(ui, |ui| {
        for a in permissions::API {
            ui.label(egui::RichText::new(a.js).size(12.0).strong().color(c.text));
            let need = a.needs.map(|p| format!("needs {}", p.name())).unwrap_or_else(|| "no permission".into());
            ui.label(egui::RichText::new(format!("{} ({need})", a.description)).size(11.5).color(c.muted));
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Plugin scripts run in the page's JavaScript world: they are not sandboxed from the page. See docs/EXTENSIONS.md.").size(11.5).color(c.muted));
    });
}
