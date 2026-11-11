//! The Automation drawer: the control server's switches and the JavaScript
//! console of the current tab (the same entries `console.read` returns).

use super::Settings;
use super::console::{ConsoleLog, Level};
use crate::egui_chrome::{Colors, icon};

/// What the browser has to do after the panel changed something.
#[derive(Clone, Debug, PartialEq)]
pub enum AutomationEvent {
    /// Save these settings and apply them (start or stop the server).
    Settings(Settings),
    /// Empty the current tab's console.
    ClearConsole,
    /// Put this text on the clipboard.
    Copy(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Console,
    Server,
}

/// What the browser knows about the server, for display.
pub struct ServerState<'a> {
    pub running: bool,
    /// The socket path, or why it has none.
    pub socket: Result<String, String>,
    /// Why the server did not start, when it should have.
    pub error: Option<&'a str>,
    /// `--automation` was given: on for this run whatever the settings say.
    pub forced: bool,
}

pub struct AutomationPanel {
    pub view: View,
    /// Least severe level shown in the console.
    min_level: Option<Level>,
    hosts_draft: Option<String>,
}

impl Default for AutomationPanel {
    fn default() -> Self {
        Self::new()
    }
}

/// The `claude mcp add` command line for this binary.
pub fn mcp_command() -> String {
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "browsecraft".into());
    let quoted = if exe.contains(' ') { format!("\"{exe}\"") } else { exe };
    format!("claude mcp add browsecraft -- {quoted} mcp")
}

fn level_color(level: Level, c: &Colors) -> egui::Color32 {
    match level {
        Level::Error => c.danger,
        Level::Warn => c.warning,
        Level::Info => c.accent,
        Level::Log => c.text,
        Level::Debug => c.muted,
    }
}

impl AutomationPanel {
    pub fn new() -> Self {
        Self { view: View::Console, min_level: None, hosts_draft: None }
    }

    /// Draws the panel. `console` is the current tab's log, if it has one.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        c: &Colors,
        settings: &Settings,
        server: &ServerState,
        console: Option<&ConsoleLog>,
    ) -> Vec<AutomationEvent> {
        let mut events = Vec::new();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, View::Console, "Console");
            ui.selectable_value(&mut self.view, View::Server, "Control server");
        });
        ui.add_space(6.0);
        match self.view {
            View::Console => self.console_ui(ui, c, console, &mut events),
            View::Server => self.server_ui(ui, c, settings, server, &mut events),
        }
        events
    }

    fn console_ui(&mut self, ui: &mut egui::Ui, c: &Colors, log: Option<&ConsoleLog>, events: &mut Vec<AutomationEvent>) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Show").color(c.muted));
            egui::ComboBox::from_id_salt("bc-console-level")
                .selected_text(match self.min_level {
                    None | Some(Level::Debug) => "All levels",
                    Some(Level::Log) => "Log and up",
                    Some(Level::Info) => "Info and up",
                    Some(Level::Warn) => "Warnings and errors",
                    Some(Level::Error) => "Errors",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.min_level, None, "All levels");
                    ui.selectable_value(&mut self.min_level, Some(Level::Log), "Log and up");
                    ui.selectable_value(&mut self.min_level, Some(Level::Info), "Info and up");
                    ui.selectable_value(&mut self.min_level, Some(Level::Warn), "Warnings and errors");
                    ui.selectable_value(&mut self.min_level, Some(Level::Error), "Errors");
                });
            if ui.button("Clear").clicked() {
                events.push(AutomationEvent::ClearConsole);
            }
        });
        ui.add_space(4.0);
        let Some(log) = log.filter(|l| !l.is_empty()) else {
            ui.label(egui::RichText::new("No console messages from this tab yet.").color(c.muted));
            return;
        };
        let shown: Vec<_> = log.entries().filter(|e| self.min_level.is_none_or(|m| e.level >= m)).collect();
        if shown.is_empty() {
            ui.label(egui::RichText::new("No messages at this level.").color(c.muted));
        }
        for e in shown.iter().rev().take(300) {
            egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 3)).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let tag = match e.level {
                        Level::Error => "error",
                        Level::Warn => "warn",
                        Level::Info => "info",
                        Level::Log => "log",
                        Level::Debug => "debug",
                    };
                    ui.label(egui::RichText::new(tag).monospace().size(11.0).color(level_color(e.level, c)));
                    ui.label(egui::RichText::new(&e.text).monospace().size(12.0).color(c.text));
                });
                if let Some(src) = &e.source {
                    let at = match (e.line, e.column) {
                        (Some(l), Some(col)) => format!("{src}:{l}:{col}"),
                        (Some(l), None) => format!("{src}:{l}"),
                        _ => src.clone(),
                    };
                    ui.label(egui::RichText::new(at).size(11.0).color(c.muted));
                }
            });
            ui.separator();
        }
    }

    fn server_ui(&mut self, ui: &mut egui::Ui, c: &Colors, settings: &Settings, server: &ServerState, events: &mut Vec<AutomationEvent>) {
        let status = if server.running { "Running" } else { "Stopped" };
        ui.horizontal(|ui| {
            let color = if server.running { c.success } else { c.muted };
            ui.label(icon(egui_phosphor::regular::CIRCLE, 10.0).color(color));
            ui.label(egui::RichText::new(status).strong().color(c.text));
            if server.forced {
                ui.label(egui::RichText::new("(--automation)").color(c.muted));
            }
        });
        if let Some(e) = server.error {
            ui.label(egui::RichText::new(e).color(c.danger));
        }
        ui.add_space(6.0);
        let mut s = settings.clone();
        let mut changed = false;
        changed |= ui.checkbox(&mut s.enabled, "Enable the control server").changed();
        changed |= ui
            .checkbox(&mut s.allow_navigation, "Allow opening pages and navigating")
            .changed();
        changed |= ui
            .checkbox(&mut s.allow_eval, "Allow running JavaScript in pages (page.eval)")
            .changed();
        changed |= ui.checkbox(&mut s.allow_file_urls, "Allow file: URLs").changed();
        ui.add_space(6.0);
        ui.label(egui::RichText::new("Allowed sites (empty: any)").color(c.muted));
        let current = settings.allowed_hosts.as_ref().map(|h| h.join(", ")).unwrap_or_default();
        let draft = self.hosts_draft.get_or_insert_with(|| current.clone());
        ui.add(egui::TextEdit::singleline(draft).hint_text("example.com, docs.rs").desired_width(f32::INFINITY));
        if *draft != current && ui.button("Apply sites").clicked() {
            let hosts: Vec<String> = draft
                .split([',', ' ', '\n'])
                .map(|h| h.trim().to_ascii_lowercase())
                .filter(|h| !h.is_empty())
                .collect();
            s.allowed_hosts = (!hosts.is_empty()).then_some(hosts);
            self.hosts_draft = None;
            changed = true;
        }
        if changed {
            events.push(AutomationEvent::Settings(s));
        }
        ui.add_space(10.0);
        ui.label(egui::RichText::new("Socket").color(c.muted));
        match &server.socket {
            Ok(p) => {
                ui.label(egui::RichText::new(p).monospace().size(12.0).color(c.text));
            }
            Err(e) => {
                ui.label(egui::RichText::new(e).color(c.danger));
            }
        }
        ui.add_space(10.0);
        ui.label(egui::RichText::new("Use it from Claude Code (MCP over stdio)").strong().color(c.text));
        let cmd = mcp_command();
        ui.label(egui::RichText::new(&cmd).monospace().size(12.0).color(c.text));
        if ui.button("Copy command").clicked() {
            events.push(AutomationEvent::Copy(cmd));
        }
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "From a shell: browsecraft ctl tabs.list, browsecraft ctl page.text. \
                 Passwords and authenticator codes are never reachable.",
            )
            .size(12.0)
            .color(c.muted),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_panel_draws_and_reports_changes() {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        crate::icons::add_fonts(&mut fonts);
        ctx.set_fonts(fonts);
        let mut panel = AutomationPanel::new();
        let mut log = ConsoleLog::default();
        log.push(
            serde_json::from_str(r#"{"level":"warn","text":"careful","source":"https://a.test/x.js","line":3}"#).unwrap(),
            "https://a.test/",
        );
        let c = Colors::for_dark(true);
        let state = ServerState { running: true, socket: Ok("/run/x.sock".into()), error: None, forced: false };
        for view in [View::Console, View::Server] {
            panel.view = view;
            let mut out = ctx.run_ui(Default::default(), |ui| {
                let events = panel.ui(ui, &c, &Settings::default(), &state, Some(&log));
                assert!(events.is_empty());
            });
            out.textures_delta.clear();
        }
        assert!(mcp_command().starts_with("claude mcp add browsecraft -- "));
        assert!(mcp_command().ends_with(" mcp"));
    }
}
