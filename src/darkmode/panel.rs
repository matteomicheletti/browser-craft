//! The dark mode drawer panel (see `docs/CONTRACTS.md`).

use std::collections::HashMap;

use super::settings::{self, DarkSettings, MAX_CSS, Mode, SiteMode};
use crate::egui_chrome::Colors;

/// Things only the browser can do after the panel changed something.
#[derive(Clone, Debug, PartialEq)]
pub enum DarkEvent {
    /// The settings changed and are already saved: re-apply them on every
    /// tab (`darkmode::update_script`).
    SettingsChanged,
}

pub struct DarkModePanel {
    settings: DarkSettings,
    /// The custom CSS being typed for `css_host`, applied on demand.
    css_draft: String,
    css_host: String,
    new_exclusion: String,
    native: HashMap<String, bool>,
    error: Option<String>,
}

impl DarkModePanel {
    pub fn new() -> Self {
        Self::with_settings(settings::load())
    }

    pub fn with_settings(settings: DarkSettings) -> Self {
        Self {
            settings,
            css_draft: String::new(),
            css_host: String::new(),
            new_exclusion: String::new(),
            native: HashMap::new(),
            error: None,
        }
    }

    pub fn settings(&self) -> &DarkSettings {
        &self.settings
    }

    /// Changes the settings from outside the panel (toolbar button, command
    /// palette) and saves them. The caller re-applies them on the tabs.
    pub fn update(&mut self, change: impl FnOnce(&mut DarkSettings)) -> Result<(), String> {
        change(&mut self.settings);
        self.settings.normalize();
        let saved = settings::save(&self.settings);
        self.error = saved.clone().err();
        saved
    }

    /// Remembers a page's "natively dark" report to show it in the panel.
    pub fn note_native_dark(&mut self, host: &str, dark: bool) {
        self.native.insert(settings::normalize_host(host), dark);
    }

    fn commit(&mut self, events: &mut Vec<DarkEvent>) {
        self.settings.normalize();
        self.error = settings::save(&self.settings).err();
        events.push(DarkEvent::SettingsChanged);
    }

    /// Draws the panel body. `host` is the current tab's host, if it has one.
    pub fn ui(&mut self, ui: &mut egui::Ui, c: &Colors, host: Option<&str>) -> Vec<DarkEvent> {
        let mut events = Vec::new();
        let mut changed = false;
        let host = host.map(settings::normalize_host).filter(|h| !h.is_empty());

        ui.spacing_mut().item_spacing.y = 8.0;
        changed |= ui.checkbox(&mut self.settings.enabled, "Dark mode").changed();

        ui.add_enabled_ui(self.settings.enabled, |ui| {
            ui.horizontal(|ui| {
                ui.label("Mode");
                egui::ComboBox::from_id_salt("bc-dark-mode")
                    .selected_text(match self.settings.mode {
                        Mode::System => "Follow system",
                        Mode::Dark => "Always dark",
                        Mode::Light => "Always light",
                        Mode::Off => "Off",
                    })
                    .show_ui(ui, |ui| {
                        for (m, label) in [
                            (Mode::System, "Follow system"),
                            (Mode::Dark, "Always dark"),
                            (Mode::Light, "Always light"),
                            (Mode::Off, "Off"),
                        ] {
                            changed |= ui.selectable_value(&mut self.settings.mode, m, label).changed();
                        }
                    });
            });

            ui.separator();
            ui.label(egui::RichText::new("This site").strong());
            match &host {
                None => {
                    ui.label(egui::RichText::new("Open a web page to set rules for its site.").color(c.muted));
                }
                Some(host) => {
                    ui.label(egui::RichText::new(host).color(c.muted));
                    let current = if self.settings.is_excluded(host)
                        && self.settings.site_mode(host) == SiteMode::Default
                    {
                        None
                    } else {
                        Some(self.settings.site_mode(host))
                    };
                    let mut pick = current;
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut pick, Some(SiteMode::Default), "Default");
                        ui.selectable_value(&mut pick, Some(SiteMode::On), "Always dark");
                        ui.selectable_value(&mut pick, Some(SiteMode::Off), "Never");
                        ui.selectable_value(&mut pick, None, "Excluded");
                    });
                    if pick != current {
                        match pick {
                            None => {
                                self.settings.set_site_mode(host, SiteMode::Default);
                                self.settings.set_excluded(host, true);
                            }
                            Some(m) => {
                                self.settings.set_excluded(host, false);
                                self.settings.set_site_mode(host, m);
                            }
                        }
                        changed = true;
                    }
                    match self.native.get(host) {
                        Some(true) => {
                            ui.label(egui::RichText::new("This page is already dark; left unchanged.").color(c.muted));
                        }
                        Some(false) => {
                            ui.label(egui::RichText::new("This page is light.").color(c.muted));
                        }
                        None => {}
                    }

                    if self.css_host != *host {
                        self.css_host = host.clone();
                        self.css_draft = self.settings.sites.get(host).map(|s| s.css.clone()).unwrap_or_default();
                    }
                    ui.label("Custom CSS for this site");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.css_draft)
                            .code_editor()
                            .desired_rows(5)
                            .desired_width(f32::INFINITY)
                            .char_limit(MAX_CSS),
                    );
                    let stored = self.settings.sites.get(host).map(|s| s.css.clone()).unwrap_or_default();
                    ui.horizontal(|ui| {
                        if ui.add_enabled(self.css_draft != stored, egui::Button::new("Apply")).clicked() {
                            self.settings.set_site_css(host, &self.css_draft);
                            changed = true;
                        }
                        if ui.add_enabled(!stored.is_empty() || !self.css_draft.is_empty(), egui::Button::new("Clear")).clicked() {
                            self.css_draft.clear();
                            self.settings.set_site_css(host, "");
                            changed = true;
                        }
                    });
                }
            }

            ui.separator();
            ui.label(egui::RichText::new("Appearance").strong());
            let slider = |ui: &mut egui::Ui, value: &mut u16, range: std::ops::RangeInclusive<u16>, label: &str| {
                let r = ui.add(egui::Slider::new(value, range).text(label).suffix("%"));
                // One update per gesture, not one per frame of a drag.
                r.drag_stopped() || (r.changed() && !r.dragged())
            };
            changed |= slider(ui, &mut self.settings.brightness, 50..=150, "Brightness");
            changed |= slider(ui, &mut self.settings.contrast, 50..=150, "Contrast");
            changed |= slider(ui, &mut self.settings.sepia, 0..=100, "Sepia");
            changed |= slider(ui, &mut self.settings.grayscale, 0..=100, "Grayscale");
            changed |= ui.checkbox(&mut self.settings.use_font, "Use a custom font").changed();
            if self.settings.use_font {
                let r = ui.add(egui::TextEdit::singleline(&mut self.settings.font_family).hint_text("Font family"));
                changed |= r.lost_focus() && r.changed();
                let mut stroke = self.settings.text_stroke;
                let r = ui.add(egui::Slider::new(&mut stroke, 0..=10).text("Text stroke (1/10 px)"));
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    self.settings.text_stroke = stroke;
                    changed = true;
                }
            }
            changed |= ui
                .checkbox(&mut self.settings.respect_native, "Leave pages that are already dark")
                .changed();

            ui.separator();
            ui.label(egui::RichText::new("Schedule").strong());
            changed |= ui.checkbox(&mut self.settings.schedule.enabled, "Only between").changed();
            if self.settings.schedule.enabled {
                ui.horizontal(|ui| {
                    for (v, id) in [(&mut self.settings.schedule.from, "from"), (&mut self.settings.schedule.to, "to")] {
                        let mut h = f32::from(*v / 60);
                        let mut m = f32::from(*v % 60);
                        ui.push_id(id, |ui| {
                            let a = ui.add(egui::DragValue::new(&mut h).range(0..=23).suffix(" h"));
                            let b = ui.add(egui::DragValue::new(&mut m).range(0..=59).suffix(" m"));
                            if a.drag_stopped() || b.drag_stopped() || ((a.changed() || b.changed()) && !a.dragged() && !b.dragged()) {
                                *v = (h as u16) * 60 + m as u16;
                                changed = true;
                            }
                        });
                        if id == "from" {
                            ui.label("and");
                        }
                    }
                });
            }

            ui.separator();
            ui.label(egui::RichText::new("Excluded sites").strong());
            let mut remove = None;
            for (i, h) in self.settings.excluded.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(h);
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                self.settings.excluded.remove(i);
                changed = true;
            }
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut self.new_exclusion).hint_text("example.com"));
                let go = ui.button("Add").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if go && !settings::normalize_host(&self.new_exclusion).is_empty() {
                    let h = std::mem::take(&mut self.new_exclusion);
                    self.settings.set_excluded(&h, true);
                    changed = true;
                }
            });
        });

        if let Some(e) = &self.error {
            ui.colored_label(c.danger, format!("Could not save: {e}"));
        }
        if changed {
            self.commit(&mut events);
        }
        events
    }
}

impl Default for DarkModePanel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(panel: &mut DarkModePanel, host: Option<&str>) -> Vec<DarkEvent> {
        let ctx = egui::Context::default();
        let mut events = Vec::new();
        let c = Colors::for_dark(true);
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| events = panel.ui(ui, &c, host));
        out.textures_delta.clear();
        events
    }

    #[test]
    fn the_panel_draws_without_changing_anything() {
        let mut p = DarkModePanel::with_settings(DarkSettings::default());
        assert!(run(&mut p, Some("example.com")).is_empty());
        assert!(run(&mut p, None).is_empty());
        assert_eq!(p.settings(), &DarkSettings::default());
    }
}
