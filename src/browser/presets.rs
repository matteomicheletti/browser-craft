//! Session presets in a window (`crate::presets`): the "Save session as
//! preset" and "Replace this window's tabs" dialogs, the presets page's
//! submissions, and opening a preset's tabs in this window. What involves
//! other windows (new windows, saving every window) goes to the
//! application as an [`AppRequest`](super::windows::AppRequest).

use super::windows::AppRequest;
use super::{Browser, HistoryEntry, Request, TabState};
use crate::presets::{self, OpenTarget, PresetWindow};
use crate::presets_page::{self, PageAction};

/// A dialog over the page.
pub(super) enum PresetDialog {
    /// "Save session as preset…": the name typed, every window or this one,
    /// and the last problem with the name.
    Save { name: String, all: bool, error: Option<String> },
    /// "Replace this window's tabs with …?"
    ConfirmReplace(u64),
}

/// What the user chose in a menu, the palette or a dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PresetEvent {
    /// Show the save dialog.
    AskSave,
    Save { name: String, all: bool },
    /// Open a preset (a replace asks first).
    Open(u64, OpenTarget),
    /// The replace was confirmed.
    ConfirmedReplace(u64),
}

/// Whether `name` can be a new preset's name: `Err` says why not.
fn check_new_name(name: &str) -> Result<(), String> {
    let name = presets::clean_name(name);
    if name.is_empty() {
        return Err("Give the preset a name".into());
    }
    if presets::find(&name).is_some() {
        return Err(format!("\u{201c}{name}\u{201d} exists: choose another name, or update it on browsecraft://sessions"));
    }
    Ok(())
}

impl Browser {
    pub(super) fn preset_event(&mut self, event: PresetEvent) {
        match event {
            PresetEvent::AskSave => {
                self.preset_dialog = Some(PresetDialog::Save { name: String::new(), all: false, error: None });
            }
            PresetEvent::Save { name, all } => self.app_requests.push(AppRequest::SavePreset { name, all }),
            PresetEvent::Open(id, OpenTarget::Replace) => self.preset_dialog = Some(PresetDialog::ConfirmReplace(id)),
            PresetEvent::Open(id, target) => self.app_requests.push(AppRequest::OpenPreset { id, target }),
            PresetEvent::ConfirmedReplace(id) => {
                self.app_requests.push(AppRequest::OpenPreset { id, target: OpenTarget::Replace });
            }
        }
        self.request_redraw();
    }

    /// The open preset dialog; returns the choice made in it.
    pub(super) fn preset_dialog_ui(&mut self, ctx: &egui::Context, c: &crate::egui_chrome::Colors) -> Option<PresetEvent> {
        let windows = self.shared.borrow().windows.len();
        let dialog = self.preset_dialog.as_mut()?;
        let mut out = None;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("bc-preset-dialog")).show(ctx, |ui| {
            ui.set_width(360.0);
            match dialog {
                PresetDialog::Save { name, all, error } => {
                    ui.label(egui::RichText::new("Save session as preset").size(15.0).strong().color(c.text));
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Keeps the tabs' addresses, containers, groups and split view. Tabs in temporary containers are left out.")
                            .size(12.0)
                            .color(c.muted),
                    );
                    ui.add_space(8.0);
                    let edit = ui.add(egui::TextEdit::singleline(name).hint_text("Work").desired_width(f32::INFINITY).char_limit(presets::MAX_NAME));
                    if !edit.has_focus() && !edit.lost_focus() {
                        edit.request_focus();
                    }
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_space(6.0);
                    ui.radio_value(all, false, "This window");
                    let label = format!("All windows ({windows})");
                    ui.add_enabled_ui(windows > 1, |ui| ui.radio_value(all, true, label));
                    if let Some(e) = error.as_deref() {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(e).size(12.0).color(c.danger));
                    }
                    ui.add_space(10.0);
                    let mut save = enter;
                    ui.horizontal(|ui| {
                        save |= ui.button("Save").clicked();
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                    if save {
                        match check_new_name(name) {
                            Ok(()) => {
                                out = Some(PresetEvent::Save { name: presets::clean_name(name), all: *all && windows > 1 });
                                close = true;
                            }
                            Err(e) => *error = Some(e),
                        }
                    }
                }
                PresetDialog::ConfirmReplace(id) => {
                    let id = *id;
                    let name = presets::get(id).map(|p| p.name).unwrap_or_default();
                    ui.label(egui::RichText::new("Replace this window's tabs?").size(15.0).strong().color(c.text));
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "Its tabs close and \u{201c}{}\u{201d} opens in their place. Ctrl+Shift+T reopens closed tabs.",
                            super::display_text(&name, 60)
                        ))
                        .size(12.0)
                        .color(c.muted),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Replace tabs").clicked() {
                            out = Some(PresetEvent::ConfirmedReplace(id));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
            }
        });
        if modal.should_close() || close {
            self.preset_dialog = None;
            ctx.request_repaint();
        }
        out
    }

    /// A link or form of `browsecraft://sessions` carrying its token: acts
    /// on it and returns the page to show instead. `None` for anything else.
    pub(super) fn sessions_page_submission(&mut self, request: &Request) -> Option<Request> {
        let query = presets_page::query_of(request.url())?;
        let body = match request {
            Request::Post(_, body) => Some(body.as_str()),
            Request::Get(_) => None,
        };
        if !presets_page::is_submission(query, body) {
            return None;
        }
        match presets_page::handle(query, body) {
            Some(PageAction::Open(id, target)) => self.app_requests.push(AppRequest::OpenPreset { id, target }),
            Some(PageAction::Save { name, all }) => self.app_requests.push(AppRequest::SavePreset { name, all }),
            Some(PageAction::Update { id, all }) => self.app_requests.push(AppRequest::UpdatePreset { id, all }),
            Some(PageAction::RestorePrevious) => self.app_requests.push(AppRequest::RestorePrevious),
            None => {}
        }
        Some(Request::Get(presets_page::page_url(query)))
    }

    /// Opens `windows`' tabs in this window: after its own tabs, or in their
    /// place (`replace`). Tabs load when first shown. Returns how many tabs
    /// opened and how many lost a container that no longer exists.
    pub(super) fn open_preset_here(&mut self, windows: &[PresetWindow], replace: bool) -> (usize, usize) {
        let plan = presets::plan_open(windows, replace, self.tabs.len(), self.split.is_some(), &|c| crate::containers::get(c).is_some());
        if plan.tabs.is_empty() {
            return (0, 0);
        }
        let old = self.tab_order();
        let ids: Vec<u64> = plan
            .tabs
            .iter()
            .map(|t| {
                let id = self.alloc_tab_id();
                self.tabs.push(TabState {
                    id,
                    container: t.container.clone(),
                    history: vec![HistoryEntry { request: Request::Get(t.url.clone()), scroll: 0.0 }],
                    title: t.title.clone(),
                    ..TabState::default()
                });
                id
            })
            .collect();
        let mut order = self.tab_order();
        for (g, members) in &plan.groups {
            let gid = self.groups.create(&g.name, Some(g.color));
            self.groups.set_collapsed(gid, g.collapsed);
            for m in members {
                order = self.groups.add_tab(ids[*m], gid, &order);
            }
        }
        self.apply_order(&order);
        let active = plan.active.map(|a| ids[a]);
        if let Some(j) = active.and_then(|id| self.tab_index(id)) {
            self.switch_tab(j);
        }
        if plan.close_existing {
            for id in old {
                if let Some(j) = self.tab_index(id) {
                    self.close_tab(j);
                }
            }
            if plan.name.is_some() {
                self.set_window_name(plan.name.as_deref().unwrap_or(""));
            }
        }
        if self.split.is_none()
            && let Some(s) = plan.split
            && let Some(mut split) = crate::split::Split::new(ids[s.left], ids[s.right])
        {
            split.set_ratio(s.ratio);
            self.split = Some(split);
            // The partner is on screen: load it, then come back.
            if let Some(a) = active
                && let Some(other) = split.other(a)
                && let (Some(j), Some(k)) = (self.tab_index(other), self.tab_index(a))
            {
                self.switch_tab(j);
                let k = self.tab_index(a).unwrap_or(k);
                self.switch_tab(k);
            }
        }
        self.request_redraw();
        (plan.tabs.len(), plan.missing_containers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_new_names_are_checked() {
        assert!(check_new_name("   ").is_err());
        assert!(check_new_name("A name nobody used").is_ok());
    }
}
