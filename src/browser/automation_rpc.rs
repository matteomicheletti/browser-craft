//! Answers control API requests (docs/AUTOMATION.md) on the UI thread.
//!
//! The server thread has already validated the parameters and checked the
//! permissions that do not depend on the page; here the browser checks
//! `allowed_hosts` against the tab's address, acts, and answers each
//! request exactly once.

use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use super::features::{NavWait, PageJob};
use super::{Browser, ContainerChoice, Request, UserMessage, WebPane};
use crate::automation::protocol::{self as p, TabId};
use crate::automation::scripts::{self, Step};
use crate::automation::{Method, RpcError};
use crate::{bookmarks, containers, history};

type AutoRequest = crate::automation::Request;

fn to_value<T: Serialize>(v: &T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::new(RpcError::INTERNAL, e.to_string()))
}

impl Browser {
    pub(super) fn automation_request(&mut self, req: AutoRequest) {
        // A request that arrives after automation was switched off.
        if self.feat.automation.is_none() {
            req.respond(Err(RpcError::denied("automation is disabled")));
            return;
        }
        match self.answer(&req) {
            // Answered now.
            Ok(Some(value)) => req.respond(Ok(value)),
            // Kept: answered later (page script, snapshot, load).
            Ok(None) => self.defer(req),
            Err(e) => req.respond(Err(e)),
        }
        self.request_redraw();
    }

    /// The tab a request names (`tab`), or the active one.
    fn target_tab(&self, tab: Option<TabId>) -> Result<(usize, u64), RpcError> {
        match tab {
            None => Ok((self.active, self.tab_id)),
            Some(id) => self
                .tab_index(id)
                .map(|i| (i, id))
                .ok_or_else(|| RpcError::browser(format!("no tab with id {id} (see tabs.list)"))),
        }
    }

    fn tab_info(&self, i: usize) -> p::TabInfo {
        let loading = if i == self.active { self.loading } else { self.tabs.get(i).is_some_and(|t| t.loading) };
        p::TabInfo {
            id: self.tab_id_at(i),
            index: i,
            url: self.tab_url(i),
            title: self.tab_title(i),
            active: i == self.active,
            loading,
            container: self.tab_container(i),
        }
    }

    fn page_info(&self, id: u64) -> p::PageInfo {
        let i = self.tab_index(id).unwrap_or(self.active);
        let mut info = self.tab_info(i);
        // The title event can come after the load event: ask the page.
        #[cfg(target_os = "linux")]
        if let Some(WebPane::Wry(wv)) = self.view_of(id) {
            use webkit2gtk::WebViewExt as _;
            use wry::WebViewExtUnix as _;
            if let Some(title) = wv.webview().title().filter(|t| !t.trim().is_empty()) {
                info.title = super::clean_title(&title);
            }
        }
        p::PageInfo { tab: info.id, url: info.url, title: info.title, loading: info.loading }
    }

    fn page_allowed(&self, i: usize) -> Result<(), RpcError> {
        self.feat.automation_settings.page_allowed(&self.tab_url(i))
    }

    /// Answers `req` now (`Some`), or says it is answered later (`None`).
    fn answer(&mut self, req: &AutoRequest) -> Result<Option<Value>, RpcError> {
        match req.method {
            Method::BrowserVersion => to_value(&p::VersionResult::current()).map(Some),
            Method::TabsList => {
                let tabs = (0..self.tabs.len()).map(|i| self.tab_info(i)).collect();
                to_value(&p::TabsListResult { tabs }).map(Some)
            }
            Method::TabsOpen => {
                let params: p::TabsOpenParams = req.params()?;
                let choice = match &params.container {
                    None => ContainerChoice::None,
                    Some(id) => match containers::get(id) {
                        Some(c) => ContainerChoice::Id(c.id),
                        None => return Err(RpcError::browser(format!("no container with id {id} (see containers.list)"))),
                    },
                };
                let previous = self.tab_id;
                self.new_tab_in(choice, Some(params.url));
                let opened = self.tab_id;
                if params.background
                    && let Some(i) = self.tab_index(previous)
                {
                    self.switch_tab(i);
                }
                let i = self.tab_index(opened).unwrap_or(self.active);
                to_value(&self.tab_info(i)).map(Some)
            }
            Method::TabsActivate => {
                let params: p::TabParams = req.params()?;
                let (i, _) = self.target_tab(Some(params.tab))?;
                self.switch_tab(i);
                to_value(&self.tab_info(i)).map(Some)
            }
            Method::TabsClose => {
                let params: p::OptTabParams = req.params()?;
                let (i, _) = self.target_tab(params.tab)?;
                if self.tabs.len() == 1 {
                    // Closing the last tab would quit the browser.
                    self.new_tab();
                }
                let i = if self.tabs.len() > 1 && i < self.tabs.len() { i } else { 0 };
                self.close_tab(i);
                to_value(&p::ActionResult { ok: true, description: None }).map(Some)
            }
            Method::PageNavigate => {
                let params: p::NavigateParams = req.params()?;
                let (i, _) = self.target_tab(params.tab)?;
                self.switch_tab(i);
                self.load(Request::Get(params.url), true);
                self.after_navigation(params.wait)
            }
            Method::PageBack | Method::PageForward | Method::PageReload => {
                let params: p::HistoryParams = req.params()?;
                let (i, _) = self.target_tab(params.tab)?;
                self.switch_tab(i);
                self.page_allowed(self.active)?;
                match req.method {
                    Method::PageBack if self.index == 0 => return Err(RpcError::browser("no previous page in this tab")),
                    Method::PageBack => self.go(-1),
                    Method::PageForward if self.index + 1 >= self.history.len() => {
                        return Err(RpcError::browser("no next page in this tab"));
                    }
                    Method::PageForward => self.go(1),
                    _ => self.reload(),
                }
                self.after_navigation(params.wait)
            }
            Method::PageScreenshot | Method::PagePdf => {
                let tab = req.params.get("tab").and_then(Value::as_u64);
                let (i, _) = self.target_tab(tab)?;
                self.page_allowed(i)?;
                Ok(None)
            }
            Method::ConsoleRead => {
                let params: p::ConsoleReadParams = req.params()?;
                let (_, id) = self.target_tab(params.tab)?;
                let result = match self.feat.console.get(&id) {
                    Some(log) => log.read_result(&params),
                    None => crate::automation::console::ConsoleLog::default().read_result(&params),
                };
                to_value(&result).map(Some)
            }
            Method::ContainersList => {
                let mut list = containers::all();
                for t in containers::temporary() {
                    if !list.iter().any(|c| c.id == t.id) {
                        list.push(t);
                    }
                }
                let containers = list
                    .into_iter()
                    .map(|c| p::ContainerInfo { id: c.id, name: c.name, color: c.color, icon: c.icon, temporary: c.temporary })
                    .collect();
                to_value(&p::ContainersListResult { containers }).map(Some)
            }
            Method::BookmarksList => {
                let params: p::BookmarksParams = req.params()?;
                let q = params.query.unwrap_or_default().to_lowercase();
                let bookmarks = bookmarks::all()
                    .into_iter()
                    .filter(|b| q.is_empty() || b.title.to_lowercase().contains(&q) || b.url.to_lowercase().contains(&q))
                    .take(params.limit.unwrap_or(500).clamp(1, 5000))
                    .map(|b| p::BookmarkInfo { title: b.title, url: b.url, folder: None })
                    .collect();
                to_value(&p::BookmarksListResult { bookmarks }).map(Some)
            }
            Method::HistorySearch => {
                let params: p::HistorySearchParams = req.params()?;
                let entries = history::search(&params.query, params.limit.unwrap_or(100).clamp(1, 1000))
                    .into_iter()
                    .map(|(url, title)| p::HistoryEntry { title, url, visited: None })
                    .collect();
                to_value(&p::HistorySearchResult { entries }).map(Some)
            }
            // Page scripts: checked here, run in `defer`.
            Method::PageText
            | Method::PageHtml
            | Method::PageSnapshot
            | Method::PageClick
            | Method::PageType
            | Method::PagePress
            | Method::PageScroll
            | Method::PageEval
            | Method::PageWait => {
                let tab = req.params.get("tab").and_then(Value::as_u64);
                let (i, _) = self.target_tab(tab)?;
                self.page_allowed(i)?;
                Ok(None)
            }
        }
    }

    /// After starting a navigation in the active tab: answer now, or wait
    /// for the load (engine pages load at once).
    fn after_navigation(&mut self, wait: bool) -> Result<Option<Value>, RpcError> {
        if !wait || !self.web_page {
            return to_value(&self.page_info(self.tab_id)).map(Some);
        }
        Ok(None)
    }

    /// Keeps a request that is answered asynchronously.
    fn defer(&mut self, req: AutoRequest) {
        match req.method {
            Method::PageNavigate | Method::PageBack | Method::PageForward | Method::PageReload => {
                let timeout = req.params.get("timeout_ms").and_then(Value::as_u64);
                let deadline = Instant::now() + Duration::from_millis(p::timeout_ms(timeout));
                self.feat.nav_waits.push(NavWait { tab: self.tab_id, req, deadline, started: false });
            }
            Method::PageScreenshot | Method::PagePdf => self.capture(req),
            _ => {
                let tab = req.params.get("tab").and_then(Value::as_u64).unwrap_or(self.tab_id);
                let started = scripts::PageJob::start(req.method, &req.params);
                match started {
                    Ok(Some((job, script))) => {
                        let id = self.feat.next_job;
                        self.feat.next_job += 1;
                        self.feat.jobs.insert(id, PageJob { job, req, tab });
                        self.run_job_script(id, &script);
                    }
                    Ok(None) => req.respond(Err(RpcError::new(RpcError::INTERNAL, "no page script for this method"))),
                    Err(e) => req.respond(Err(e)),
                }
            }
        }
    }

    /// Runs a page job's script in its tab; the result comes back as
    /// `UserMessage::AutomationScript`.
    pub(super) fn run_job_script(&mut self, id: u64, script: &str) {
        let Some(tab) = self.feat.jobs.get(&id).map(|j| j.tab) else {
            return;
        };
        let proxy = self.proxy.clone();
        let result = match self.view_of(tab) {
            // WebKitGTK directly: wry queues scripts sent before a new web
            // view's first commit and then drops their callbacks.
            #[cfg(target_os = "linux")]
            Some(WebPane::Wry(wv)) => {
                use wry::WebViewExtUnix as _;
                crate::headless_webkit::run_script(&wv.webview(), script, move |json| {
                    let _ = proxy.send_event(UserMessage::AutomationScript { job: id, json });
                });
                Ok(())
            }
            #[cfg(not(target_os = "linux"))]
            Some(WebPane::Wry(wv)) => wv
                .evaluate_script_with_callback(script, move |json| {
                    let _ = proxy.send_event(UserMessage::AutomationScript { job: id, json });
                })
                .map_err(|e| e.to_string()),
            Some(_) => Err("this web engine cannot run automation scripts".to_string()),
            None => Err("the tab shows no live web page (internal page, or unloaded: activate it first)".to_string()),
        };
        if let Err(e) = result
            && let Some(job) = self.feat.jobs.remove(&id)
        {
            job.req.respond(Err(RpcError::browser(e)));
        }
    }

    pub(super) fn job_result(&mut self, id: u64, json: &str) {
        let Some(job) = self.feat.jobs.get_mut(&id) else {
            return;
        };
        match job.job.on_result(json) {
            Step::Done(result) => {
                if let Some(job) = self.feat.jobs.remove(&id) {
                    job.req.respond(result);
                }
            }
            Step::Run(next) => self.run_job_script(id, &next),
            Step::RunLater(next) => {
                self.feat.delayed.push((Instant::now() + scripts::POLL_INTERVAL, id, next));
            }
        }
    }

    /// `page.screenshot` and `page.pdf`, from the tab's WebKit view.
    #[cfg(target_os = "linux")]
    fn capture(&mut self, req: AutoRequest) {
        use base64::Engine as _;
        use wry::WebViewExtUnix as _;
        let tab = req.params.get("tab").and_then(Value::as_u64).unwrap_or(self.tab_id);
        let Some(WebPane::Wry(wv)) = self.view_of(tab) else {
            req.respond(Err(RpcError::browser("the tab shows no live web page")));
            return;
        };
        let wk = wv.webview();
        if req.method == Method::PageScreenshot {
            let full_page = req.params.get("full_page").and_then(Value::as_bool).unwrap_or(false);
            crate::headless_webkit::snapshot_png(&wk, full_page, move |r| {
                req.respond(r.map_err(RpcError::browser).and_then(|(png, width, height)| {
                    let png_base64 = base64::engine::general_purpose::STANDARD.encode(png);
                    to_value(&p::ScreenshotResult { png_base64, width, height })
                }));
            });
            return;
        }
        let dir = match tempfile::Builder::new().prefix("browsecraft-pdf-").tempdir() {
            Ok(d) => d,
            Err(e) => {
                req.respond(Err(RpcError::browser(e.to_string())));
                return;
            }
        };
        let path = dir.path().join("page.pdf");
        let out = path.clone();
        crate::headless_webkit::print_pdf(&wk, &path, move |r| {
            let _keep = &dir; // deleted with the closure
            req.respond(r.map_err(RpcError::browser).and_then(|bytes| {
                let pdf_base64 = crate::headless_webkit::file_base64(&out).map_err(RpcError::browser)?;
                to_value(&p::PdfResult { pdf_base64, bytes: bytes as usize })
            }));
        });
    }

    #[cfg(not(target_os = "linux"))]
    fn capture(&mut self, req: AutoRequest) {
        req.respond(Err(RpcError::browser("screenshots and PDFs are only available on Linux for now")));
    }

    /// Load events of tab `id` for navigations clients wait for.
    pub(super) fn automation_load_event(&mut self, id: u64, finished: bool) {
        if self.feat.nav_waits.is_empty() {
            return;
        }
        let mut i = 0;
        while i < self.feat.nav_waits.len() {
            let w = &mut self.feat.nav_waits[i];
            if w.tab != id {
                i += 1;
                continue;
            }
            if !finished {
                w.started = true;
                i += 1;
                continue;
            }
            if !w.started {
                i += 1;
                continue;
            }
            let w = self.feat.nav_waits.remove(i);
            w.req.respond(to_value(&self.page_info(id)));
        }
    }
}
