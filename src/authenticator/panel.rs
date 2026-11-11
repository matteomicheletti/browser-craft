//! The authenticator drawer panel (see `docs/CONTRACTS.md`): unlock or
//! create the vault, the list of accounts with live codes, add / edit /
//! delete, import / export and security settings.
//!
//! Slow work (Argon2id, scrypt, QR decoding) runs on a worker thread so the
//! window keeps painting; the panel polls it every frame.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, RichText, Sense, Stroke, vec2};
use egui_phosphor::regular as ph;
use zeroize::Zeroizing;

use super::import::{self, Imported};
use super::otp::{self, Algorithm};
use super::vault::{self, AuthVault, VaultKey};
use super::{Account, Kind, Secret};
use crate::egui_chrome::{Colors, icon, rgb};

/// What the panel asks the browser to do.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthEvent {
    /// Put this one-time code on the clipboard. Codes expire within a
    /// minute; core may clear the clipboard after a while as it does for
    /// passwords.
    Copy(String),
    /// The vault was unlocked (for the toolbar button state).
    Unlocked,
    /// The vault was locked, by the user or by the idle timer.
    Locked,
}

const DANGER: u32 = 0xe5484d;
const WARN: u32 = 0xd99a1e;
const COPIED_FOR: Duration = Duration::from_millis(1500);
const AUTO_LOCK_CHOICES: [(u64, &str); 5] = [
    (0, "Never"),
    (60, "After 1 minute"),
    (5 * 60, "After 5 minutes"),
    (15 * 60, "After 15 minutes"),
    (60 * 60, "After 1 hour"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum View {
    List,
    Add(AddTab),
    Edit(String),
    Import,
    Export,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddTab {
    Manual,
    Link,
    Image,
}

enum JobResult {
    Opened(Result<AuthVault, String>),
    NewKey(Result<VaultKey, String>),
    Imported(Result<Imported, String>),
    Exported(Result<String, String>),
}

struct Job {
    what: &'static str,
    rx: Receiver<JobResult>,
}

fn spawn(what: &'static str, work: impl FnOnce() -> JobResult + Send + 'static) -> Job {
    let (tx, rx) = std::sync::mpsc::channel();
    // If the thread cannot start, the sender is dropped with the closure
    // and the poll reports the job as failed.
    let _ = std::thread::Builder::new()
        .name("authenticator".into())
        .spawn(move || {
            let _ = tx.send(work());
        });
    Job { what, rx }
}

/// The add / edit form. The secret field is wiped when dropped.
struct Form {
    issuer: String,
    label: String,
    secret: Zeroizing<String>,
    show_secret: bool,
    hotp: bool,
    algorithm: Algorithm,
    digits: u32,
    period: u32,
    counter: u64,
    tags: String,
    note: String,
}

impl Form {
    fn blank() -> Form {
        Form {
            issuer: String::new(),
            label: String::new(),
            secret: Zeroizing::new(String::new()),
            show_secret: false,
            hotp: false,
            algorithm: Algorithm::Sha1,
            digits: 6,
            period: 30,
            counter: 0,
            tags: String::new(),
            note: String::new(),
        }
    }

    /// Editing never shows the stored secret: an empty field keeps it.
    fn from_account(a: &Account) -> Form {
        let (hotp, period, counter) = match a.kind {
            Kind::Totp { period } => (false, period, 0),
            Kind::Hotp { counter } => (true, 30, counter),
        };
        Form {
            issuer: a.issuer.clone(),
            label: a.label.clone(),
            hotp,
            algorithm: a.algorithm,
            digits: a.digits,
            period,
            counter,
            tags: a.tags.join(", "),
            note: a.note.clone(),
            ..Form::blank()
        }
    }

    fn build(&self, base: Option<&Account>) -> Result<Account, String> {
        let secret = match (self.secret.trim().is_empty(), base) {
            (true, Some(base)) => base.secret.clone(),
            (true, None) => return Err("Enter the secret key the site shows.".into()),
            (false, _) => Secret::from_base32(&self.secret)
                .ok_or("The secret key may only contain letters A–Z and digits 2–7.")?,
        };
        let mut account = Account {
            id: base.map_or_else(super::new_id, |b| b.id.clone()),
            issuer: self.issuer.clone(),
            label: self.label.clone(),
            secret,
            algorithm: self.algorithm,
            digits: self.digits,
            kind: if self.hotp {
                Kind::Hotp { counter: self.counter }
            } else {
                Kind::Totp { period: self.period }
            },
            tags: self.tags.split(',').map(str::to_string).collect(),
            note: self.note.clone(),
        };
        account.tidy();
        account.validate()?;
        Ok(account)
    }
}

/// What a row asked for, applied after the list is drawn.
enum RowAction {
    Copy(String),
    NextHotp(String),
    Edit(String),
    Shift(String, isize),
    AskDelete(String),
    Delete(String),
    CancelDelete,
}

/// A row's display data, copied out of the vault so drawing can mutate
/// the panel. Codes are short-lived and not secret material.
struct Row {
    id: String,
    title: String,
    label: String,
    code: String,
    totp: Option<(u32, f32)>,
    tags: Vec<String>,
}

pub struct AuthenticatorPanel {
    path: Result<PathBuf, String>,
    vault: Option<AuthVault>,
    last_activity: Instant,
    view: View,
    password: Zeroizing<String>,
    confirm: Zeroizing<String>,
    search: String,
    tag: Option<String>,
    copied: Option<(String, Instant)>,
    confirm_delete: Option<String>,
    form: Form,
    link_text: Zeroizing<String>,
    file_path: String,
    file_password: Zeroizing<String>,
    preview: Option<Imported>,
    export_path: String,
    export_password: Zeroizing<String>,
    export_confirm: Zeroizing<String>,
    plain_path: String,
    plain_ack: bool,
    old_password: Zeroizing<String>,
    new_password: Zeroizing<String>,
    new_confirm: Zeroizing<String>,
    message: Option<(String, bool)>,
    job: Option<Job>,
    events: Vec<AuthEvent>,
}

impl Default for AuthenticatorPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthenticatorPanel {
    /// A locked panel on the default vault (`authenticator.bcotp` in the
    /// per-user config folder).
    pub fn new() -> Self {
        Self::with_path_result(vault::default_path())
    }

    /// A locked panel on the vault at `path`.
    pub fn with_path(path: PathBuf) -> Self {
        Self::with_path_result(Ok(path))
    }

    fn with_path_result(path: Result<PathBuf, String>) -> Self {
        AuthenticatorPanel {
            path,
            vault: None,
            last_activity: Instant::now(),
            view: View::List,
            password: Zeroizing::new(String::new()),
            confirm: Zeroizing::new(String::new()),
            search: String::new(),
            tag: None,
            copied: None,
            confirm_delete: None,
            form: Form::blank(),
            link_text: Zeroizing::new(String::new()),
            file_path: String::new(),
            file_password: Zeroizing::new(String::new()),
            preview: None,
            export_path: String::new(),
            export_password: Zeroizing::new(String::new()),
            export_confirm: Zeroizing::new(String::new()),
            plain_path: String::new(),
            plain_ack: false,
            old_password: Zeroizing::new(String::new()),
            new_password: Zeroizing::new(String::new()),
            new_confirm: Zeroizing::new(String::new()),
            message: None,
            job: None,
            events: Vec::new(),
        }
    }

    pub fn is_unlocked(&self) -> bool {
        self.vault.is_some()
    }

    /// Whether a vault file exists (else the panel offers to create one).
    pub fn vault_exists(&self) -> bool {
        self.path.as_ref().is_ok_and(|p| p.exists())
    }

    /// Locks the vault: the key, the seeds and every typed secret are
    /// dropped (and wiped). Pending imports are discarded.
    pub fn lock(&mut self) {
        let was_unlocked = self.vault.take().is_some();
        self.preview = None;
        self.copied = None;
        self.confirm_delete = None;
        self.form = Form::blank();
        for field in [
            &mut self.password,
            &mut self.confirm,
            &mut self.link_text,
            &mut self.file_password,
            &mut self.export_password,
            &mut self.export_confirm,
            &mut self.old_password,
            &mut self.new_password,
            &mut self.new_confirm,
        ] {
            *field = Zeroizing::new(String::new());
        }
        self.view = View::List;
        if was_unlocked {
            self.events.push(AuthEvent::Locked);
        }
    }

    /// Runs the idle timer and finishes background work. Core calls it
    /// every frame (or on a timer) so auto-lock works while the drawer is
    /// closed; [`Self::ui`] calls it too.
    pub fn tick(&mut self) -> Vec<AuthEvent> {
        self.poll_job();
        if let Some(v) = &self.vault {
            let secs = v.settings().auto_lock_secs;
            if secs > 0 && self.last_activity.elapsed() >= Duration::from_secs(secs) {
                self.lock();
                self.message = Some(("Locked after a period without use.".into(), false));
            }
        }
        std::mem::take(&mut self.events)
    }

    /// Resets the idle timer (core may call it when the user copies a code
    /// from a shortcut, for example).
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    fn info(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), false));
    }

    fn error(&mut self, text: impl Into<String>) {
        self.message = Some((text.into(), true));
    }

    fn busy(&self) -> bool {
        self.job.is_some()
    }

    fn poll_job(&mut self) {
        let Some(job) = &self.job else { return };
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                self.error("The background task failed.");
                return;
            }
        };
        self.job = None;
        match result {
            JobResult::Opened(Ok(v)) => {
                self.vault = Some(v);
                self.password = Zeroizing::new(String::new());
                self.confirm = Zeroizing::new(String::new());
                self.message = None;
                self.view = View::List;
                self.touch();
                self.events.push(AuthEvent::Unlocked);
            }
            JobResult::Opened(Err(e)) => {
                self.password = Zeroizing::new(String::new());
                self.error(e);
            }
            JobResult::NewKey(Ok(key)) => {
                let saved = self.vault.as_mut().map(|v| v.change_key(key));
                match saved {
                    Some(Ok(())) => {
                        self.old_password = Zeroizing::new(String::new());
                        self.new_password = Zeroizing::new(String::new());
                        self.new_confirm = Zeroizing::new(String::new());
                        self.info("Master password changed.");
                    }
                    Some(Err(e)) => self.error(e),
                    None => self.error("The vault was locked meanwhile; nothing changed."),
                }
            }
            JobResult::NewKey(Err(e)) => self.error(e),
            JobResult::Imported(Ok(found)) if found.accounts.is_empty() => {
                self.error(format!(
                    "Nothing to import ({} entr{} skipped).",
                    found.skipped,
                    if found.skipped == 1 { "y" } else { "ies" }
                ));
            }
            JobResult::Imported(Ok(found)) => {
                self.file_password = Zeroizing::new(String::new());
                self.message = None;
                self.preview = Some(found);
            }
            JobResult::Imported(Err(e)) => self.error(e),
            JobResult::Exported(Ok(path)) => {
                self.export_password = Zeroizing::new(String::new());
                self.export_confirm = Zeroizing::new(String::new());
                self.info(format!("Encrypted backup saved to {path}."));
            }
            JobResult::Exported(Err(e)) => self.error(e),
        }
    }

    /// Draws the panel body (the drawer draws its title and close button).
    pub fn ui(&mut self, ui: &mut egui::Ui, c: &Colors) -> Vec<AuthEvent> {
        self.poll_job();
        let active = ui.ui_contains_pointer()
            && ui.input(|i| !i.events.is_empty() || i.pointer.any_down() || i.pointer.delta() != egui::Vec2::ZERO);
        if active && self.vault.is_some() {
            self.touch();
        }
        let mut events = self.tick();
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        self.message_ui(ui, c);
        if self.vault.is_none() {
            self.locked_ui(ui, c);
        } else {
            match self.view.clone() {
                View::List => self.list_ui(ui, c),
                View::Add(tab) => self.add_ui(ui, c, tab),
                View::Edit(id) => self.edit_ui(ui, c, &id),
                View::Import => self.import_ui(ui, c),
                View::Export => self.export_ui(ui, c),
                View::Settings => self.settings_ui(ui, c),
            }
        }
        if self.busy() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        } else if self.vault.is_some() {
            // The countdown rings move; codes roll over on the second.
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
        events.append(&mut self.events);
        events
    }

    // ---- pieces ----

    fn message_ui(&mut self, ui: &mut egui::Ui, c: &Colors) {
        let Some((text, is_error)) = self.message.clone() else { return };
        let tint = if is_error { rgb(DANGER) } else { c.accent };
        let mut close = false;
        egui::Frame::NONE
            .fill(tint.gamma_multiply(0.14))
            .stroke(Stroke::new(1.0, tint.gamma_multiply(0.5)))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let glyph = if is_error { ph::WARNING } else { ph::CHECK };
                    ui.label(icon(glyph, 15.0).color(tint));
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        close = ui
                            .add(egui::Button::new(icon(ph::X, 12.0).color(c.muted)).frame(false))
                            .on_hover_text("Dismiss")
                            .clicked();
                        ui.with_layout(Layout::left_to_right(Align::Min), |ui| {
                            ui.add(egui::Label::new(RichText::new(text).size(12.5).color(c.text)).wrap());
                        });
                    });
                });
            });
        if close {
            self.message = None;
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, c: &Colors, title: &str) {
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new(icon(ph::ARROW_LEFT, 15.0).color(c.text)).frame(false))
                .on_hover_text("Back")
                .clicked()
            {
                self.view = View::List;
                self.preview = None;
                self.message = None;
            }
            ui.label(RichText::new(title).size(15.0).strong().color(c.text));
        });
        ui.add_space(2.0);
    }

    fn locked_ui(&mut self, ui: &mut egui::Ui, c: &Colors) {
        let path = match &self.path {
            Ok(p) => p.clone(),
            Err(e) => {
                let e = e.clone();
                ui.label(RichText::new(e).color(rgb(DANGER)));
                return;
            }
        };
        let exists = path.exists();
        ui.add_space(18.0);
        ui.vertical_centered(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(60.0, 60.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 30.0, c.accent.gamma_multiply(0.15));
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                ph::LOCK_KEY,
                FontId::new(28.0, crate::icons::family()),
                c.accent,
            );
            ui.add_space(4.0);
            ui.label(RichText::new("Authenticator").size(18.0).strong().color(c.text));
            let sub = if exists {
                "Unlock to see your two-factor codes."
            } else {
                "Keep the two-factor codes of your accounts here, encrypted with a master password and apart from your saved passwords."
            };
            ui.add(egui::Label::new(RichText::new(sub).size(12.5).color(c.muted)).wrap());
        });
        ui.add_space(8.0);
        let busy = self.busy();
        let mut submit = false;
        ui.add_enabled_ui(!busy, |ui| {
            let r = field(ui, c, &mut self.password, "Master password", true);
            // When creating, Enter in the first field moves on rather than
            // submitting a password that has not been repeated yet.
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if exists {
                    submit = true;
                } else {
                    ui.memory_mut(|m| m.request_focus(egui::Id::new("bc-auth-confirm")));
                }
            }
            if !exists {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut *self.confirm)
                        .id(egui::Id::new("bc-auth-confirm"))
                        .password(true)
                        .desired_width(f32::INFINITY)
                        .margin(Margin::symmetric(8, 6))
                        .hint_text(RichText::new("Repeat the password").color(c.muted)),
                );
                submit |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            }
            let label = if exists { "Unlock" } else { "Create vault" };
            submit |= primary(ui, c, label).clicked();
        });
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(self.job.as_ref().map_or("", |j| j.what)).color(c.muted));
            });
        } else if !exists {
            hint(
                ui,
                c,
                "If you forget this password the codes cannot be recovered. Keep an encrypted backup (Export) somewhere safe.",
            );
        }
        if submit && !busy {
            let password = self.password.clone();
            if exists {
                self.job = Some(spawn("Unlocking…", move || {
                    JobResult::Opened(AuthVault::open(&path, &password))
                }));
            } else if *self.password != *self.confirm {
                self.error("The two passwords differ.");
            } else if self.password.chars().count() < vault::MIN_PASSWORD {
                self.error(format!("Use a password of at least {} characters.", vault::MIN_PASSWORD));
            } else {
                self.job = Some(spawn("Creating the vault…", move || {
                    JobResult::Opened(AuthVault::create(&path, &password))
                }));
            }
        }
    }

    fn rows(&self, now_ms: u128) -> Vec<Row> {
        let Some(v) = &self.vault else { return Vec::new() };
        let now = (now_ms / 1000) as u64;
        v.accounts()
            .iter()
            .filter(|a| a.matches(&self.search))
            .filter(|a| self.tag.as_ref().is_none_or(|t| a.tags.iter().any(|x| x.eq_ignore_ascii_case(t))))
            .map(|a| Row {
                id: a.id.clone(),
                title: a.title().to_string(),
                label: if a.issuer.trim().is_empty() { String::new() } else { a.label.clone() },
                code: a.code_at(now),
                totp: match a.kind {
                    Kind::Totp { period } => {
                        let p = u128::from(period.max(1)) * 1000;
                        let left = p - now_ms % p;
                        Some((otp::seconds_left(now, period), left as f32 / p as f32))
                    }
                    Kind::Hotp { .. } => None,
                },
                tags: a.tags.clone(),
            })
            .collect()
    }

    fn list_ui(&mut self, ui: &mut egui::Ui, c: &Colors) {
        // Toolbar: search, add, more, lock.
        let mut open: Option<View> = None;
        let mut lock = false;
        let mut sort = false;
        ui.horizontal(|ui| {
            let buttons = 3.0 * 36.0;
            egui::Frame::NONE
                .fill(c.field_bg)
                .stroke(Stroke::new(1.0, c.line))
                .corner_radius(CornerRadius::same(16))
                .inner_margin(Margin::symmetric(10, 3))
                .show(ui, |ui| {
                    ui.set_width((ui.available_width() - buttons - 20.0).max(80.0));
                    ui.horizontal(|ui| {
                        ui.label(icon(ph::MAGNIFYING_GLASS, 13.0).color(c.muted));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.search)
                                .frame(egui::Frame::NONE)
                                .desired_width(f32::INFINITY)
                                .text_color(c.text)
                                .hint_text(RichText::new("Search").color(c.muted)),
                        );
                    });
                });
            if crate::egui_chrome::icon_button(ui, true, ph::PLUS, "Add account", 16.0).clicked() {
                open = Some(View::Add(AddTab::Manual));
            }
            ui.menu_button(icon(ph::DOTS_THREE_VERTICAL, 16.0), |ui| {
                ui.set_min_width(190.0);
                if crate::egui_chrome::menu_item(ui, c, ph::DOWNLOAD_SIMPLE, "Import…").clicked() {
                    open = Some(View::Import);
                    ui.close();
                }
                if crate::egui_chrome::menu_item(ui, c, ph::UPLOAD_SIMPLE, "Export…").clicked() {
                    open = Some(View::Export);
                    ui.close();
                }
                if crate::egui_chrome::menu_item(ui, c, ph::SORT_ASCENDING, "Sort A–Z").clicked() {
                    sort = true;
                    ui.close();
                }
                if crate::egui_chrome::menu_item(ui, c, ph::GEAR, "Security settings").clicked() {
                    open = Some(View::Settings);
                    ui.close();
                }
            });
            if crate::egui_chrome::icon_button(ui, true, ph::LOCK, "Lock", 16.0).clicked() {
                lock = true;
            }
        });
        if lock {
            self.lock();
            return;
        }
        if sort && let Some(Err(e)) = self.vault.as_mut().map(AuthVault::sort_by_name) {
            self.error(e);
        }
        if let Some(view) = open {
            if view == View::Add(AddTab::Manual) {
                self.form = Form::blank();
            }
            self.preview = None;
            self.message = None;
            self.view = view;
            return;
        }

        let tags = self.vault.as_ref().map(AuthVault::tags).unwrap_or_default();
        if !tags.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                if chip(ui, c, "All", self.tag.is_none()).clicked() {
                    self.tag = None;
                }
                for t in &tags {
                    let on = self.tag.as_ref().is_some_and(|x| x.eq_ignore_ascii_case(t));
                    if chip(ui, c, t, on).clicked() {
                        self.tag = if on { None } else { Some(t.clone()) };
                    }
                }
            });
        }

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let rows = self.rows(now_ms);
        let total = self.vault.as_ref().map_or(0, |v| v.accounts().len());
        let hide = self.vault.as_ref().is_some_and(|v| v.settings().hide_codes);
        if total == 0 {
            if empty_state(ui, c) {
                self.form = Form::blank();
                self.view = View::Add(AddTab::Manual);
            }
            return;
        }
        if rows.is_empty() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No accounts match.").color(c.muted));
            });
        }
        let mut actions = Vec::new();
        let busy = self.busy();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height((ui.available_height() - 26.0).max(120.0))
            .show(ui, |ui| {
                ui.add_enabled_ui(!busy, |ui| {
                    for (i, row) in rows.iter().enumerate() {
                        let copied = self
                            .copied
                            .as_ref()
                            .is_some_and(|(id, at)| *id == row.id && at.elapsed() < COPIED_FOR);
                        let confirming = self.confirm_delete.as_deref() == Some(row.id.as_str());
                        let first = i == 0;
                        let last = i + 1 == rows.len();
                        row_ui(ui, c, row, copied, confirming, hide, (first, last), &mut actions);
                    }
                });
            });
        let lock_text = self
            .vault
            .as_ref()
            .map(|v| v.settings().auto_lock_secs)
            .and_then(|s| AUTO_LOCK_CHOICES.iter().find(|(v, _)| *v == s).map(|(_, t)| *t))
            .map_or(String::new(), |t| {
                if t == "Never" {
                    " · auto-lock off".to_string()
                } else {
                    format!(" · locks {}", t.to_lowercase())
                }
            });
        ui.label(
            RichText::new(format!(
                "{total} account{}{lock_text}",
                if total == 1 { "" } else { "s" }
            ))
            .size(11.5)
            .color(c.muted),
        );
        for action in actions {
            self.apply(action);
        }
    }

    fn apply(&mut self, action: RowAction) {
        let Some(v) = self.vault.as_mut() else { return };
        let result = match action {
            RowAction::Copy(id) => {
                if let Some(a) = v.account(&id) {
                    self.events.push(AuthEvent::Copy(a.code_at(otp::now())));
                    self.copied = Some((id, Instant::now()));
                }
                Ok(())
            }
            RowAction::NextHotp(id) => v.next_hotp(&id).map(|code| {
                self.events.push(AuthEvent::Copy(code));
                self.copied = Some((id, Instant::now()));
            }),
            RowAction::Edit(id) => {
                if let Some(a) = v.account(&id) {
                    self.form = Form::from_account(a);
                    self.view = View::Edit(id);
                    self.message = None;
                }
                Ok(())
            }
            RowAction::Shift(id, d) => v.shift(&id, d),
            RowAction::AskDelete(id) => {
                self.confirm_delete = Some(id);
                Ok(())
            }
            RowAction::CancelDelete => {
                self.confirm_delete = None;
                Ok(())
            }
            RowAction::Delete(id) => {
                self.confirm_delete = None;
                let title = v.account(&id).map(|a| a.title().to_string()).unwrap_or_default();
                v.remove(&id).map(|()| self.message = Some((format!("Deleted {title}."), false)))
            }
        };
        if let Err(e) = result {
            self.error(e);
        }
    }

    fn add_ui(&mut self, ui: &mut egui::Ui, c: &Colors, tab: AddTab) {
        self.header(ui, c, "Add account");
        if self.preview.is_some() {
            self.preview_ui(ui, c);
            return;
        }
        ui.horizontal(|ui| {
            for (t, glyph, label) in [
                (AddTab::Manual, ph::KEYBOARD, "Setup key"),
                (AddTab::Link, ph::LINK, "Link"),
                (AddTab::Image, ph::QR_CODE, "QR image"),
            ] {
                if chip(ui, c, &format!("{glyph}  {label}"), tab == t).clicked() {
                    self.view = View::Add(t);
                    self.message = None;
                }
            }
        });
        ui.add_space(4.0);
        match tab {
            AddTab::Manual => {
                hint(ui, c, "Type the setup key the site shows next to its QR code (often under \"Can't scan it?\").");
                form_ui(ui, c, &mut self.form, false);
                if primary(ui, c, "Add account").clicked() {
                    let result = self.form.build(None).and_then(|a| match self.vault.as_mut() {
                        Some(v) => v.add(a.clone()).map(|()| a),
                        None => Err("The vault is locked.".into()),
                    });
                    match result {
                        Ok(a) => {
                            self.form = Form::blank();
                            self.view = View::List;
                            self.info(format!("Added {}.", a.title()));
                        }
                        Err(e) => self.error(e),
                    }
                }
            }
            AddTab::Link => {
                hint(
                    ui,
                    c,
                    "Paste an otpauth:// link, or a Google Authenticator export link (otpauth-migration://). Several links can go on separate lines.",
                );
                ui.add(
                    egui::TextEdit::multiline(&mut *self.link_text)
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .hint_text("otpauth://totp/Service:you@example.com?secret=…"),
                );
                if primary(ui, c, "Read link").clicked() {
                    let found = import::parse_uri_list(&self.link_text);
                    if found.accounts.is_empty() {
                        self.error("No valid authenticator link found.");
                    } else {
                        self.link_text = Zeroizing::new(String::new());
                        self.preview = Some(found);
                    }
                }
            }
            AddTab::Image => {
                hint(
                    ui,
                    c,
                    "Save or screenshot the QR code as an image (PNG, JPEG, GIF, WebP), then drop it here or type its path.",
                );
                self.path_field(ui, c);
                let busy = self.busy();
                if busy {
                    spinner(ui, c, self.job.as_ref().map_or("", |j| j.what));
                }
                if ui.add_enabled(!busy, primary_button(c, "Read QR code")).clicked() {
                    self.start_import(true);
                }
            }
        }
    }

    /// A path text field that also accepts a file dropped on the window.
    fn path_field(&mut self, ui: &mut egui::Ui, c: &Colors) {
        if let Some(path) = ui.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf())) {
            self.file_path = path.display().to_string();
        }
        let mut text = std::mem::take(&mut self.file_path);
        egui::Frame::NONE
            .fill(c.field_bg)
            .stroke(Stroke::new(1.0, c.line))
            .corner_radius(CornerRadius::same(9))
            .inner_margin(Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(icon(ph::FILE_ARROW_UP, 14.0).color(c.muted));
                    ui.add(
                        egui::TextEdit::singleline(&mut text)
                            .frame(egui::Frame::NONE)
                            .desired_width(f32::INFINITY)
                            .hint_text(RichText::new("/home/you/Downloads/file").color(c.muted)),
                    );
                });
            });
        self.file_path = text;
    }

    /// Reads the chosen file and imports it on the worker thread.
    fn start_import(&mut self, image_only: bool) {
        let path = match resolve_path(&self.file_path) {
            Ok(p) => p,
            Err(e) => return self.error(e),
        };
        let bytes = match import::read_file(&path) {
            Ok(b) => b,
            Err(e) => return self.error(e),
        };
        let kind = import::detect(&bytes);
        if image_only && kind != import::FileKind::Image {
            return self.error("That file is not a PNG, JPEG, GIF or WebP image.");
        }
        if kind == import::FileKind::Unknown {
            return self.error("This file is not a format the authenticator can import.");
        }
        if kind.needs_password() && self.file_password.is_empty() {
            return self.error(format!("This is an {}: enter its password.", kind.describe()));
        }
        let password = kind.needs_password().then(|| self.file_password.clone());
        self.message = None;
        self.job = Some(spawn(
            if kind.needs_password() { "Decrypting…" } else { "Reading…" },
            move || JobResult::Imported(import::import_bytes(&bytes, password.as_deref().map(|p| p.as_str()))),
        ));
    }

    fn preview_ui(&mut self, ui: &mut egui::Ui, c: &Colors) {
        let Some(found) = &self.preview else { return };
        let existing: Vec<bool> = found
            .accounts
            .iter()
            .map(|a| {
                self.vault
                    .as_ref()
                    .is_some_and(|v| v.accounts().iter().any(|x| x.same_as(a)))
            })
            .collect();
        let new = existing.iter().filter(|e| !**e).count();
        ui.label(
            RichText::new(format!(
                "Found {} account{}{}.",
                found.accounts.len(),
                if found.accounts.len() == 1 { "" } else { "s" },
                if found.skipped > 0 {
                    format!(" ({} unsupported entr{} skipped)", found.skipped, if found.skipped == 1 { "y" } else { "ies" })
                } else {
                    String::new()
                }
            ))
            .color(c.text),
        );
        egui::ScrollArea::vertical()
            .max_height((ui.available_height() - 60.0).max(100.0))
            .show(ui, |ui| {
                for (a, dup) in found.accounts.iter().zip(&existing) {
                    card(ui, c, |ui| {
                        ui.horizontal(|ui| {
                            avatar(ui, a.title(), 28.0);
                            ui.vertical(|ui| {
                                ui.label(RichText::new(a.title()).strong().color(c.text));
                                if !a.issuer.is_empty() && !a.label.is_empty() {
                                    ui.label(RichText::new(&a.label).size(12.0).color(c.muted));
                                }
                            });
                            if *dup {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(RichText::new("already added").size(11.5).color(c.muted));
                                });
                            }
                        });
                    });
                }
            });
        let mut add = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            let label = if new == 1 { "Add 1 account".to_string() } else { format!("Add {new} accounts") };
            add = ui.add_enabled(new > 0, primary_button(c, &label)).clicked();
            cancel = secondary(ui, c, "Cancel").clicked();
        });
        if cancel {
            self.preview = None;
        }
        if add && let Some(found) = self.preview.take() {
            let result = match self.vault.as_mut() {
                Some(v) => v.add_many(found.accounts),
                None => Err("The vault is locked.".into()),
            };
            match result {
                Ok(n) => {
                    self.view = View::List;
                    self.info(format!("Added {n} account{}.", if n == 1 { "" } else { "s" }));
                }
                Err(e) => self.error(e),
            }
        }
    }

    fn edit_ui(&mut self, ui: &mut egui::Ui, c: &Colors, id: &str) {
        self.header(ui, c, "Edit account");
        let Some(base) = self.vault.as_ref().and_then(|v| v.account(id)).cloned() else {
            self.view = View::List;
            return;
        };
        form_ui(ui, c, &mut self.form, true);
        let mut save = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            save = primary_button_ui(ui, c, "Save");
            cancel = secondary(ui, c, "Cancel").clicked();
        });
        if cancel {
            self.view = View::List;
        }
        if save {
            let result = self.form.build(Some(&base)).and_then(|a| match self.vault.as_mut() {
                Some(v) => v.update(a),
                None => Err("The vault is locked.".into()),
            });
            match result {
                Ok(()) => {
                    self.view = View::List;
                    self.form = Form::blank();
                    self.info("Saved.");
                }
                Err(e) => self.error(e),
            }
        }
    }

    fn import_ui(&mut self, ui: &mut egui::Ui, c: &Colors) {
        self.header(ui, c, "Import");
        if self.preview.is_some() {
            self.preview_ui(ui, c);
            return;
        }
        hint(
            ui,
            c,
            "Supported: BrowseCraft encrypted backups, Aegis exports (plain or encrypted), text files of otpauth:// or otpauth-migration:// links, and QR code images. Drop a file here or type its path.",
        );
        self.path_field(ui, c);
        field(ui, c, &mut self.file_password, "Password (encrypted files only)", true);
        let busy = self.busy();
        if busy {
            spinner(ui, c, self.job.as_ref().map_or("", |j| j.what));
        }
        if ui.add_enabled(!busy, primary_button(c, "Read file")).clicked() {
            self.start_import(false);
        }
    }

    fn export_ui(&mut self, ui: &mut egui::Ui, c: &Colors) {
        self.header(ui, c, "Export");
        let busy = self.busy();
        card(ui, c, |ui| {
            ui.horizontal(|ui| {
                ui.label(icon(ph::SHIELD_CHECK, 16.0).color(c.accent));
                ui.label(RichText::new("Encrypted backup").strong().color(c.text));
                ui.label(RichText::new("recommended").size(11.0).color(c.accent));
            });
            hint(ui, c, "Every account in one file, encrypted with its own password. Restore it with Import.");
            path_input(ui, c, &mut self.export_path, "/home/you/authenticator-backup.bcotp");
            field(ui, c, &mut self.export_password, "Backup password", true);
            field(ui, c, &mut self.export_confirm, "Repeat the password", true);
            if busy {
                spinner(ui, c, self.job.as_ref().map_or("", |j| j.what));
            }
            if ui.add_enabled(!busy, primary_button(c, "Save backup")).clicked() {
                self.start_backup();
            }
        });
        card(ui, c, |ui| {
            ui.horizontal(|ui| {
                ui.label(icon(ph::WARNING, 16.0).color(rgb(WARN)));
                ui.label(RichText::new("Plain text links").strong().color(c.text));
            });
            hint(
                ui,
                c,
                "One otpauth:// link per line, readable by most authenticator apps. The file is NOT encrypted: anyone who reads it can generate your codes. Delete it after use.",
            );
            path_input(ui, c, &mut self.plain_path, "/home/you/authenticator-links.txt");
            ui.checkbox(
                &mut self.plain_ack,
                RichText::new("I understand this file is not encrypted").size(12.5).color(c.text),
            );
            if ui
                .add_enabled(self.plain_ack && !busy, secondary_button(c, "Save plain file"))
                .clicked()
            {
                let result = resolve_path(&self.plain_path).and_then(|path| {
                    let accounts = self.vault.as_ref().map(AuthVault::snapshot).unwrap_or_default();
                    let text = import::uri_list(&accounts);
                    import::write_export(&path, text.as_bytes()).map(|()| path)
                });
                match result {
                    Ok(path) => {
                        self.plain_ack = false;
                        self.info(format!("Plain links saved to {}. Delete the file when done.", path.display()));
                    }
                    Err(e) => self.error(e),
                }
            }
        });
    }

    fn start_backup(&mut self) {
        if *self.export_password != *self.export_confirm {
            return self.error("The two passwords differ.");
        }
        if self.export_password.chars().count() < vault::MIN_PASSWORD {
            return self.error(format!("Use a password of at least {} characters.", vault::MIN_PASSWORD));
        }
        let path = match resolve_path(&self.export_path) {
            Ok(p) => p,
            Err(e) => return self.error(e),
        };
        if path.exists() {
            return self.error("A file with that name already exists; pick a new name.");
        }
        let accounts = self.vault.as_ref().map(AuthVault::snapshot).unwrap_or_default();
        let password = self.export_password.clone();
        self.job = Some(spawn("Encrypting…", move || {
            JobResult::Exported(
                vault::backup_bytes(&accounts, &password)
                    .and_then(|bytes| import::write_export(&path, &bytes))
                    .map(|()| path.display().to_string()),
            )
        }));
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui, c: &Colors) {
        self.header(ui, c, "Security settings");
        let Some(current) = self.vault.as_ref().map(|v| v.settings().clone()) else { return };
        let mut settings = current.clone();
        card(ui, c, |ui| {
            ui.horizontal(|ui| {
                ui.label(icon(ph::CLOCK, 15.0).color(c.accent));
                ui.label(RichText::new("Auto-lock").strong().color(c.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let text = AUTO_LOCK_CHOICES
                        .iter()
                        .find(|(s, _)| *s == settings.auto_lock_secs)
                        .map_or_else(|| format!("After {} s", settings.auto_lock_secs), |(_, t)| t.to_string());
                    egui::ComboBox::from_id_salt("bc-auth-autolock")
                        .selected_text(text)
                        .show_ui(ui, |ui| {
                            for (secs, label) in AUTO_LOCK_CHOICES {
                                ui.selectable_value(&mut settings.auto_lock_secs, secs, label);
                            }
                        });
                });
            });
            hint(ui, c, "Locks the vault when the authenticator has not been used for this long.");
            ui.checkbox(
                &mut settings.hide_codes,
                RichText::new("Hide codes until the pointer is over them").size(12.5).color(c.text),
            );
        });
        if settings != current
            && let Some(Err(e)) = self.vault.as_mut().map(|v| v.set_settings(settings))
        {
            self.error(e);
        }
        let busy = self.busy();
        card(ui, c, |ui| {
            ui.horizontal(|ui| {
                ui.label(icon(ph::KEY, 15.0).color(c.accent));
                ui.label(RichText::new("Change master password").strong().color(c.text));
            });
            field(ui, c, &mut self.old_password, "Current password", true);
            field(ui, c, &mut self.new_password, "New password", true);
            field(ui, c, &mut self.new_confirm, "Repeat the new password", true);
            if busy {
                spinner(ui, c, self.job.as_ref().map_or("", |j| j.what));
            }
            if ui.add_enabled(!busy, primary_button(c, "Change password")).clicked() {
                self.start_change_password();
            }
        });
        card(ui, c, |ui| {
            ui.horizontal(|ui| {
                ui.label(icon(ph::LOCK, 15.0).color(c.accent));
                ui.label(RichText::new("Vault").strong().color(c.text));
            });
            if let Ok(p) = &self.path {
                ui.add(egui::Label::new(RichText::new(p.display().to_string()).size(11.5).color(c.muted)).wrap());
            }
            hint(
                ui,
                c,
                "Encrypted with XChaCha20-Poly1305; the key is derived from your password with Argon2id (64 MiB, 3 passes). Codes are never shared with web pages, plugins or automation.",
            );
        });
        if secondary(ui, c, &format!("{}  Lock now", ph::LOCK)).clicked() {
            self.lock();
        }
    }

    fn start_change_password(&mut self) {
        if *self.new_password != *self.new_confirm {
            return self.error("The two new passwords differ.");
        }
        if self.new_password.chars().count() < vault::MIN_PASSWORD {
            return self.error(format!("Use a password of at least {} characters.", vault::MIN_PASSWORD));
        }
        let Some(key) = self.vault.as_ref().map(|v| v.key().clone()) else { return };
        let old = self.old_password.clone();
        let new = self.new_password.clone();
        self.job = Some(spawn("Changing the password…", move || {
            JobResult::NewKey(if key.matches(&old) {
                VaultKey::new(&new)
            } else {
                Err("The current password is wrong.".into())
            })
        }));
    }
}

// ---- widgets ----

/// Expands `~/` and refuses relative paths (they would land in whatever
/// the working directory happens to be).
fn resolve_path(text: &str) -> Result<PathBuf, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Enter the full path of the file.".into());
    }
    let path = match text.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|h| Path::new(&h).join(rest))
            .ok_or("Your home folder is unknown; enter the full path.")?,
        None => PathBuf::from(text),
    };
    if !path.is_absolute() {
        return Err("Enter the full path of the file (starting with / or ~/).".into());
    }
    Ok(path)
}

fn hint(ui: &mut egui::Ui, c: &Colors, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(c.muted)).wrap());
}

fn spinner(ui: &mut egui::Ui, c: &Colors, text: &str) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(RichText::new(text).size(12.5).color(c.muted));
    });
}

fn field(ui: &mut egui::Ui, c: &Colors, text: &mut Zeroizing<String>, hint: &str, password: bool) -> egui::Response {
    ui.add(
        egui::TextEdit::singleline(&mut **text)
            .password(password)
            .desired_width(f32::INFINITY)
            .margin(Margin::symmetric(8, 6))
            .hint_text(RichText::new(hint).color(c.muted)),
    )
}

fn path_input(ui: &mut egui::Ui, c: &Colors, text: &mut String, hint: &str) {
    ui.add(
        egui::TextEdit::singleline(text)
            .desired_width(f32::INFINITY)
            .margin(Margin::symmetric(8, 6))
            .hint_text(RichText::new(hint).color(c.muted)),
    );
}

fn primary_button<'a>(c: &Colors, text: &str) -> egui::Button<'a> {
    egui::Button::new(RichText::new(text).size(13.0).strong().color(Color32::WHITE))
        .fill(c.accent)
        .corner_radius(CornerRadius::same(9))
        .min_size(vec2(0.0, 32.0))
}

fn primary(ui: &mut egui::Ui, c: &Colors, text: &str) -> egui::Response {
    ui.add_sized([ui.available_width(), 32.0], primary_button(c, text))
}

fn primary_button_ui(ui: &mut egui::Ui, c: &Colors, text: &str) -> bool {
    ui.add(primary_button(c, text).min_size(vec2(90.0, 32.0))).clicked()
}

fn secondary_button<'a>(c: &Colors, text: &str) -> egui::Button<'a> {
    egui::Button::new(RichText::new(text).size(13.0).color(c.text))
        .fill(c.hover)
        .corner_radius(CornerRadius::same(9))
        .min_size(vec2(90.0, 32.0))
}

fn secondary(ui: &mut egui::Ui, c: &Colors, text: &str) -> egui::Response {
    ui.add(secondary_button(c, text))
}

fn chip(ui: &mut egui::Ui, c: &Colors, text: &str, on: bool) -> egui::Response {
    let (fill, fg) = if on { (c.accent, Color32::WHITE) } else { (c.hover, c.text) };
    ui.add(
        egui::Button::new(RichText::new(text).size(12.0).color(fg))
            .fill(fill)
            .corner_radius(CornerRadius::same(12))
            .min_size(vec2(0.0, 24.0)),
    )
}

fn card<R>(ui: &mut egui::Ui, c: &Colors, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(c.field_bg)
        .stroke(Stroke::new(1.0, c.line))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A round letter avatar in a colour picked from the name, so the same
/// service always gets the same colour.
fn avatar(ui: &mut egui::Ui, name: &str, size: f32) {
    const PALETTE: [u32; 8] = [
        0x5b8def, 0x2bb673, 0xe5484d, 0xd99a1e, 0x8e6cef, 0x12a4b4, 0xe0559b, 0x6b7a90,
    ];
    let hash = name
        .to_lowercase()
        .bytes()
        .fold(2166136261u32, |h, b| (h ^ u32::from(b)).wrapping_mul(16777619));
    let color = rgb(PALETTE[(hash % PALETTE.len() as u32) as usize]);
    let letter = name
        .chars()
        .find(|c| c.is_alphanumeric())
        .map_or("?".to_string(), |c| c.to_uppercase().to_string());
    let (rect, _) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    ui.painter().circle_filled(rect.center(), size / 2.0, color);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        letter,
        FontId::proportional(size * 0.46),
        Color32::WHITE,
    );
}

/// The countdown ring: the remaining share of the period, red in the last
/// five seconds.
fn ring(ui: &mut egui::Ui, c: &Colors, secs: u32, fraction: f32) {
    let (rect, response) = ui.allocate_exact_size(vec2(28.0, 28.0), Sense::hover());
    let center = rect.center();
    let radius = 11.0;
    let color = if secs <= 5 { rgb(DANGER) } else { c.accent };
    let painter = ui.painter();
    painter.circle_stroke(center, radius, Stroke::new(2.5, c.line));
    let steps = (64.0 * fraction).ceil().max(1.0) as usize;
    let points: Vec<egui::Pos2> = (0..=steps)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * fraction * i as f32 / steps as f32;
            center + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    painter.add(egui::Shape::line(points, Stroke::new(2.5, color)));
    painter.text(center, Align2::CENTER_CENTER, secs.to_string(), FontId::proportional(9.5), c.muted);
    response.on_hover_text(format!("New code in {secs} s"));
}

#[allow(clippy::too_many_arguments)]
fn row_ui(
    ui: &mut egui::Ui,
    c: &Colors,
    row: &Row,
    copied: bool,
    confirming: bool,
    hide: bool,
    (first, last): (bool, bool),
    actions: &mut Vec<RowAction>,
) {
    let id = row.id.clone();
    card(ui, c, |ui| {
        ui.horizontal(|ui| {
            avatar(ui, &row.title, 34.0);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&row.title).size(13.5).strong().color(c.text));
                    for t in &row.tags {
                        ui.label(RichText::new(t).size(10.5).color(c.muted).background_color(c.hover));
                    }
                });
                if !row.label.is_empty() {
                    ui.label(RichText::new(&row.label).size(12.0).color(c.muted));
                }
                let hovered = ui.ui_contains_pointer();
                let shown = if hide && !hovered {
                    "•".repeat(row.code.len())
                } else {
                    otp::group_code(&row.code)
                };
                let code_color = match row.totp {
                    Some((secs, _)) if secs <= 5 => rgb(DANGER),
                    _ => c.text,
                };
                let code = ui
                    .add(
                        egui::Label::new(RichText::new(shown).monospace().size(22.0).color(code_color))
                            .sense(Sense::click()),
                    )
                    .on_hover_text("Click to copy");
                if code.clicked() {
                    actions.push(RowAction::Copy(id.clone()));
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.menu_button(icon(ph::DOTS_THREE_VERTICAL, 15.0).color(c.muted), |ui| {
                    ui.set_min_width(160.0);
                    if crate::egui_chrome::menu_item(ui, c, ph::PENCIL_SIMPLE, "Edit").clicked() {
                        actions.push(RowAction::Edit(id.clone()));
                        ui.close();
                    }
                    if !first && crate::egui_chrome::menu_item(ui, c, ph::ARROW_UP, "Move up").clicked() {
                        actions.push(RowAction::Shift(id.clone(), -1));
                        ui.close();
                    }
                    if !last && crate::egui_chrome::menu_item(ui, c, ph::ARROW_DOWN, "Move down").clicked() {
                        actions.push(RowAction::Shift(id.clone(), 1));
                        ui.close();
                    }
                    if crate::egui_chrome::menu_item(ui, c, ph::TRASH, "Delete").clicked() {
                        actions.push(RowAction::AskDelete(id.clone()));
                        ui.close();
                    }
                });
                let copy_glyph = if copied { ph::CHECK } else { ph::COPY };
                let copy_color = if copied { rgb(0x2bb673) } else { c.muted };
                let copy = ui
                    .add(egui::Button::new(icon(copy_glyph, 15.0).color(copy_color)).frame(false))
                    .on_hover_text(if copied { "Copied" } else { "Copy code" });
                if copy.clicked() {
                    actions.push(RowAction::Copy(id.clone()));
                }
                match row.totp {
                    Some((secs, fraction)) => ring(ui, c, secs, fraction),
                    None => {
                        let next = ui
                            .add(egui::Button::new(icon(ph::ARROW_CLOCKWISE, 15.0).color(c.accent)).frame(false))
                            .on_hover_text("Use the next code (advances the counter and copies it)");
                        if next.clicked() {
                            actions.push(RowAction::NextHotp(id.clone()));
                        }
                    }
                }
            });
        });
        if confirming {
            ui.separator();
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "Delete {}? Without a backup, you will need the site's recovery codes to sign in.",
                        row.title
                    ))
                    .size(12.5)
                    .color(c.text),
                )
                .wrap(),
            );
            ui.horizontal(|ui| {
                let delete = egui::Button::new(RichText::new("Delete").size(12.5).strong().color(Color32::WHITE))
                    .fill(rgb(DANGER))
                    .corner_radius(CornerRadius::same(8));
                if ui.add(delete).clicked() {
                    actions.push(RowAction::Delete(id.clone()));
                }
                if secondary(ui, c, "Cancel").clicked() {
                    actions.push(RowAction::CancelDelete);
                }
            });
        }
    });
}

/// Shown when the vault has no accounts. Returns true for "Add account".
fn empty_state(ui: &mut egui::Ui, c: &Colors) -> bool {
    let mut add = false;
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.label(icon(ph::SHIELD_CHECK, 40.0).color(c.accent));
        ui.label(RichText::new("No accounts yet").size(15.0).strong().color(c.text));
        ui.add(
            egui::Label::new(
                RichText::new(
                    "When a site offers two-factor authentication, add the setup key, link or QR code it shows.",
                )
                .size(12.5)
                .color(c.muted),
            )
            .wrap(),
        );
        ui.add_space(6.0);
        add = ui.add(primary_button(c, &format!("{}  Add account", ph::PLUS))).clicked();
    });
    add
}

fn form_ui(ui: &mut egui::Ui, c: &Colors, form: &mut Form, editing: bool) {
    let label = |ui: &mut egui::Ui, text: &str| {
        ui.label(RichText::new(text).size(12.5).color(c.muted));
    };
    let text = |ui: &mut egui::Ui, value: &mut String, hint: &str| {
        ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(f32::INFINITY)
                .margin(Margin::symmetric(8, 5))
                .hint_text(RichText::new(hint).color(c.muted)),
        );
    };
    egui::Grid::new("bc-auth-form")
        .num_columns(2)
        .spacing([10.0, 8.0])
        .min_col_width(70.0)
        .show(ui, |ui| {
            label(ui, "Service");
            text(ui, &mut form.issuer, "GitHub");
            ui.end_row();
            label(ui, "Account");
            text(ui, &mut form.label, "you@example.com");
            ui.end_row();
            label(ui, "Secret key");
            ui.horizontal(|ui| {
                let hint = if editing { "Leave empty to keep the current key" } else { "JBSW Y3DP EHPK 3PXP" };
                ui.add(
                    egui::TextEdit::singleline(&mut *form.secret)
                        .password(!form.show_secret)
                        .desired_width((ui.available_width() - 30.0).max(60.0))
                        .margin(Margin::symmetric(8, 5))
                        .font(egui::TextStyle::Monospace)
                        .hint_text(RichText::new(hint).color(c.muted)),
                );
                let eye = if form.show_secret { ph::EYE_SLASH } else { ph::EYE };
                if ui
                    .add(egui::Button::new(icon(eye, 14.0).color(c.muted)).frame(false))
                    .on_hover_text(if form.show_secret { "Hide" } else { "Show" })
                    .clicked()
                {
                    form.show_secret = !form.show_secret;
                }
            });
            ui.end_row();
            label(ui, "Tags");
            text(ui, &mut form.tags, "work, personal");
            ui.end_row();
            label(ui, "Note");
            text(ui, &mut form.note, "Optional");
            ui.end_row();
        });
    egui::CollapsingHeader::new(RichText::new("Advanced").size(12.5).color(c.muted))
        .id_salt("bc-auth-advanced")
        .default_open(editing && (form.hotp || form.algorithm != Algorithm::Sha1 || form.digits != 6 || form.period != 30))
        .show(ui, |ui| {
            hint(ui, c, "Only change these if the site says so; most use time-based, SHA-1, 6 digits, 30 seconds.");
            egui::Grid::new("bc-auth-advanced-grid")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .min_col_width(70.0)
                .show(ui, |ui| {
                    label(ui, "Type");
                    ui.horizontal(|ui| {
                        if chip(ui, c, "Time-based", !form.hotp).clicked() {
                            form.hotp = false;
                        }
                        if chip(ui, c, "Counter-based", form.hotp).clicked() {
                            form.hotp = true;
                        }
                    });
                    ui.end_row();
                    label(ui, "Algorithm");
                    egui::ComboBox::from_id_salt("bc-auth-algo")
                        .selected_text(form.algorithm.name())
                        .show_ui(ui, |ui| {
                            for a in Algorithm::ALL {
                                ui.selectable_value(&mut form.algorithm, a, a.name());
                            }
                        });
                    ui.end_row();
                    label(ui, "Digits");
                    egui::ComboBox::from_id_salt("bc-auth-digits")
                        .selected_text(form.digits.to_string())
                        .show_ui(ui, |ui| {
                            for d in otp::MIN_DIGITS..=otp::MAX_DIGITS {
                                ui.selectable_value(&mut form.digits, d, d.to_string());
                            }
                        });
                    ui.end_row();
                    if form.hotp {
                        label(ui, "Counter");
                        ui.add(egui::DragValue::new(&mut form.counter).range(0..=u64::MAX / 2));
                    } else {
                        label(ui, "Period");
                        ui.add(
                            egui::DragValue::new(&mut form.period)
                                .range(otp::MIN_PERIOD..=otp::MAX_PERIOD)
                                .suffix(" s"),
                        );
                    }
                    ui.end_row();
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    const PW: &str = "panel password";

    fn render(panel: &mut AuthenticatorPanel, dark: bool) -> Vec<AuthEvent> {
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        crate::icons::add_fonts(&mut fonts);
        ctx.set_fonts(fonts);
        crate::egui_chrome::apply_style(&ctx, dark);
        let c = Colors::for_dark(dark);
        let mut events = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(360.0, 700.0))),
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            events = panel.ui(ui, &c);
        })
        .drop_without_applying_deltas();
        events
    }

    fn wait_job(panel: &mut AuthenticatorPanel) -> Vec<AuthEvent> {
        let mut events = Vec::new();
        for _ in 0..500 {
            events.extend(panel.tick());
            if !panel.busy() {
                return events;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("job did not finish");
    }

    fn unlocked() -> (tempfile::TempDir, AuthenticatorPanel) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.bcotp");
        let mut v = AuthVault::create(&path, PW).unwrap();
        let mut a = Account::totp("GitHub", "alice", Secret::from_base32("JBSWY3DPEHPK3PXP").unwrap());
        a.tags = vec!["work".into()];
        v.add(a).unwrap();
        let mut h = Account::totp("Bank", "", Secret::from_slice(b"12345678901234567890"));
        h.kind = Kind::Hotp { counter: 0 };
        v.add(h).unwrap();
        drop(v);
        let mut panel = AuthenticatorPanel::with_path(path);
        *panel.password = PW.into();
        let path = panel.path.clone().unwrap();
        let password = panel.password.clone();
        panel.job = Some(spawn("Unlocking…", move || JobResult::Opened(AuthVault::open(&path, &password))));
        assert_eq!(wait_job(&mut panel), vec![AuthEvent::Unlocked]);
        assert!(panel.password.is_empty(), "typed password cleared");
        (dir, panel)
    }

    #[test]
    fn renders_every_view_in_both_themes() {
        let dir = tempfile::tempdir().unwrap();
        let mut locked = AuthenticatorPanel::with_path(dir.path().join("none.bcotp"));
        for dark in [true, false] {
            assert!(render(&mut locked, dark).is_empty());
        }
        let (_dir, mut panel) = unlocked();
        for view in [
            View::List,
            View::Add(AddTab::Manual),
            View::Add(AddTab::Link),
            View::Add(AddTab::Image),
            View::Import,
            View::Export,
            View::Settings,
        ] {
            for dark in [true, false] {
                panel.view = view.clone();
                render(&mut panel, dark);
                assert_eq!(panel.view, view, "{view:?} stays open");
            }
        }
        let id = panel.vault.as_ref().unwrap().accounts()[0].id.clone();
        panel.apply(RowAction::Edit(id.clone()));
        assert_eq!(panel.view, View::Edit(id.clone()));
        render(&mut panel, true);
        panel.view = View::List;
        panel.confirm_delete = Some(id);
        panel.message = Some(("hello".into(), true));
        render(&mut panel, false);
        panel.search = "nothing matches".into();
        render(&mut panel, true);
    }

    #[test]
    fn copy_hotp_delete_and_lock() {
        let (_dir, mut panel) = unlocked();
        let (gid, hid) = {
            let v = panel.vault.as_ref().unwrap();
            (v.accounts()[0].id.clone(), v.accounts()[1].id.clone())
        };
        panel.apply(RowAction::Copy(hid.clone()));
        panel.apply(RowAction::NextHotp(hid.clone()));
        let events = panel.tick();
        assert_eq!(
            events,
            vec![AuthEvent::Copy("755224".into()), AuthEvent::Copy("287082".into())]
        );
        panel.apply(RowAction::AskDelete(gid.clone()));
        assert_eq!(panel.confirm_delete.as_deref(), Some(gid.as_str()));
        panel.apply(RowAction::Delete(gid));
        assert_eq!(panel.vault.as_ref().unwrap().accounts().len(), 1);
        panel.lock();
        assert!(!panel.is_unlocked());
        assert_eq!(panel.tick(), vec![AuthEvent::Locked]);
    }

    #[test]
    fn auto_lock_after_idle() {
        let (_dir, mut panel) = unlocked();
        let mut s = panel.vault.as_ref().unwrap().settings().clone();
        s.auto_lock_secs = 60;
        panel.vault.as_mut().unwrap().set_settings(s).unwrap();
        assert!(panel.tick().is_empty());
        panel.last_activity = Instant::now() - Duration::from_secs(61);
        assert_eq!(panel.tick(), vec![AuthEvent::Locked]);
        assert!(!panel.is_unlocked());
    }

    #[test]
    fn forms_build_accounts() {
        let mut form = Form::blank();
        form.issuer = "Svc".into();
        assert!(form.build(None).is_err(), "secret required");
        *form.secret = "jbsw y3dp ehpk 3pxp".into();
        form.tags = "a, b, a".into();
        let a = form.build(None).unwrap();
        assert_eq!(a.tags, vec!["a".to_string(), "b".to_string()]);
        let mut edit = Form::from_account(&a);
        assert!(edit.secret.is_empty(), "the stored secret is never put back in a field");
        edit.label = "renamed".into();
        let b = edit.build(Some(&a)).unwrap();
        assert_eq!((b.id.as_str(), b.secret == a.secret), (a.id.as_str(), true));
        *edit.secret = "not base32!".into();
        assert!(edit.build(Some(&a)).is_err());
    }

    #[test]
    fn link_and_file_import_flow() {
        let (dir, mut panel) = unlocked();
        // A backup of a new account, imported through the worker.
        let mut extra = Account::totp("GitLab", "bob", Secret::from_slice(b"another seed 123"));
        extra.tags = vec!["work".into()];
        let backup = dir.path().join("b.bcotp");
        import::write_export(&backup, &vault::backup_bytes(&[extra], "backup password").unwrap()).unwrap();
        panel.view = View::Import;
        panel.file_path = backup.display().to_string();
        panel.start_import(false);
        assert!(panel.message.as_ref().is_some_and(|(_, err)| *err), "password required");
        *panel.file_password = "backup password".into();
        panel.start_import(false);
        wait_job(&mut panel);
        assert_eq!(panel.preview.as_ref().map(|p| p.accounts.len()), Some(1));
        let found = panel.preview.take().unwrap();
        assert_eq!(panel.vault.as_mut().unwrap().add_many(found.accounts).unwrap(), 1);
        assert_eq!(panel.vault.as_ref().unwrap().accounts().len(), 3);
        // The image tab refuses a non-image.
        panel.start_import(true);
        assert!(panel.message.as_ref().is_some_and(|(m, _)| m.contains("not a PNG")));
        assert!(resolve_path("relative/file").is_err());
        assert!(resolve_path("/abs/file").is_ok());
    }

    #[test]
    fn backup_and_password_change_jobs() {
        let (dir, mut panel) = unlocked();
        let out = dir.path().join("backup.bcotp");
        panel.export_path = out.display().to_string();
        *panel.export_password = "export password".into();
        *panel.export_confirm = "export password".into();
        panel.start_backup();
        wait_job(&mut panel);
        assert!(panel.message.as_ref().is_some_and(|(m, err)| !err && m.contains("saved")), "{:?}", panel.message);
        assert_eq!(vault::restore_backup(&std::fs::read(&out).unwrap(), "export password").unwrap().len(), 2);

        *panel.old_password = "wrong password".into();
        *panel.new_password = "new master password".into();
        *panel.new_confirm = "new master password".into();
        panel.start_change_password();
        wait_job(&mut panel);
        assert!(panel.message.as_ref().is_some_and(|(m, _)| m.contains("wrong")));
        *panel.old_password = PW.into();
        panel.start_change_password();
        wait_job(&mut panel);
        assert!(panel.message.as_ref().is_some_and(|(m, _)| m.contains("changed")));
        let path = panel.path.clone().unwrap();
        assert!(AuthVault::open(&path, "new master password").is_ok());
    }
}
