//! Screen recording of the browser window, by driving `ffmpeg`.
//!
//! On Linux the window is an X11 window (an XWayland client under Wayland),
//! so ffmpeg's `x11grab` can capture it by window id. The recording is
//! written to `~/Videos/BrowseCraft-<timestamp>.<ext>`. Stopping is graceful:
//! `q` on ffmpeg's stdin lets it finish the file (moov atom, GIF palette).
//!
//! The browser keeps one [`Recorder`], calls [`Recorder::start`] for the
//! `StartRecording` command and [`Recorder::stop`] for `StopRecording`, and
//! polls [`Recorder::is_recording`] so the palette shows the right command.
// The API is wired into the browser in the integration step.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Container and codec of the recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Format {
    /// H.264 in MP4: small and plays everywhere.
    #[default]
    Mp4,
    /// VP9 in WebM.
    Webm,
    /// Animated GIF (12 fps, palette optimised): large, but pastes anywhere.
    Gif,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Mp4 => "mp4",
            Format::Webm => "webm",
            Format::Gif => "gif",
        }
    }
}

/// What to capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// An X11 window, by its XID (see [`x11_window_id`]).
    Window(u32),
    /// A screen rectangle, in X11 root coordinates.
    Region { x: i32, y: i32, w: u32, h: u32 },
}

/// Settings for one recording.
#[derive(Clone, Debug)]
pub struct Options {
    pub target: Target,
    pub format: Format,
    pub fps: u32,
    pub draw_mouse: bool,
}

impl Options {
    pub fn new(target: Target) -> Self {
        Options {
            target,
            format: Format::Mp4,
            fps: 30,
            draw_mouse: true,
        }
    }
}

/// The X11 window id of a winit window, when it is an X11 (or XWayland) one.
pub fn x11_window_id(window: &winit::window::Window) -> Option<u32> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(h) => u32::try_from(h.window).ok(),
        RawWindowHandle::Xcb(h) => Some(h.window.get()),
        _ => None,
    }
}

/// The ffmpeg arguments (without the program name) for these options.
/// `display` is the X display (`:0`) and `out` the output file.
pub fn build_args(o: &Options, display: &str, out: &Path) -> Vec<String> {
    // No `-nostdin`: stdin carries the graceful `q`.
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    a.extend(["-y", "-f", "x11grab"].map(String::from));
    a.extend(["-framerate".into(), o.fps.clamp(1, 60).to_string()]);
    a.extend(["-draw_mouse".into(), u8::from(o.draw_mouse).to_string()]);
    let input = match o.target {
        Target::Window(id) => {
            a.extend(["-window_id".into(), format!("0x{id:x}")]);
            display.to_string()
        }
        Target::Region { x, y, w, h } => {
            a.extend(["-video_size".into(), format!("{w}x{h}")]);
            format!("{display}+{x},{y}")
        }
    };
    a.extend(["-i".into(), input]);
    // Encoders need even dimensions.
    let even = "scale=trunc(iw/2)*2:trunc(ih/2)*2";
    match o.format {
        // A GIF is cut from an intermediate MP4 when the recording stops:
        // encoding it live loses frames on a static page (see `gif_args`).
        Format::Mp4 | Format::Gif => {
            a.extend(["-vf".into(), even.into()]);
            a.extend(
                [
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-crf",
                    "23",
                    "-pix_fmt",
                    "yuv420p",
                    "-movflags",
                    "+faststart",
                ]
                .map(String::from),
            );
        }
        Format::Webm => {
            a.extend(["-vf".into(), even.into()]);
            a.extend(
                [
                    "-c:v",
                    "libvpx-vp9",
                    "-deadline",
                    "realtime",
                    "-cpu-used",
                    "8",
                    "-b:v",
                    "0",
                    "-crf",
                    "34",
                ]
                .map(String::from),
            );
        }
    }
    a.push(out.to_string_lossy().into_owned());
    a
}

/// The ffmpeg arguments that turn the intermediate recording into a GIF
/// (12 fps, one optimised palette).
pub fn gif_args(input: &Path, out: &Path) -> Vec<String> {
    [
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-i",
        &input.to_string_lossy(),
        "-vf",
        "fps=12,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse",
        &out.to_string_lossy(),
    ]
    .map(String::from)
    .to_vec()
}

/// Where recordings go: `~/Videos` (created if needed), else the home folder.
pub fn output_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let videos = home.join("Videos");
    if videos.is_dir() || std::fs::create_dir_all(&videos).is_ok() {
        Some(videos)
    } else {
        Some(home)
    }
}

/// `BrowseCraft-20260101-120000.mp4` for a `stamp` like `20260101-120000`.
pub fn file_name(stamp: &str, format: Format) -> String {
    format!("BrowseCraft-{stamp}.{}", format.extension())
}

fn timestamp() -> String {
    // Local time without a date crate: ask `date`, fall back to unix seconds.
    #[cfg(unix)]
    if let Ok(out) = Command::new("date").arg("+%Y%m%d-%H%M%S").output()
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() && s.len() < 32 && s.chars().all(|c| c.is_ascii_digit() || c == '-') {
            return s;
        }
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        .to_string()
}

struct Running {
    child: Child,
    /// The file the user gets.
    path: PathBuf,
    /// For GIF: the MP4 ffmpeg is writing, converted on stop.
    temp: Option<PathBuf>,
    program: String,
    started: Instant,
    stderr: Arc<Mutex<String>>,
}

/// One screen recording at a time.
#[derive(Default)]
pub struct Recorder {
    running: Option<Running>,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts recording; returns the file being written.
    pub fn start(&mut self, options: &Options) -> Result<PathBuf, String> {
        if self.is_recording() {
            return Err("A recording is already running.".into());
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = options;
            Err("Screen recording is only available on Linux for now.".to_string())
        }
        #[cfg(target_os = "linux")]
        {
            let display = std::env::var("DISPLAY")
                .ok()
                .filter(|d| !d.is_empty())
                .ok_or("No X11 display: screen recording needs X11 or XWayland.")?;
            let dir = output_dir().ok_or("Cannot find the home folder for the recording.")?;
            let path = dir.join(file_name(&timestamp(), options.format));
            self.start_to(options, &display, &path, "ffmpeg")?;
            Ok(path)
        }
    }

    /// `start` with an explicit display, output file and ffmpeg program.
    pub fn start_to(
        &mut self,
        options: &Options,
        display: &str,
        path: &Path,
        program: &str,
    ) -> Result<(), String> {
        let temp = (options.format == Format::Gif).then(|| path.with_extension("gif.mp4"));
        let record_to = temp.as_deref().unwrap_or(path);
        let mut child = Command::new(program)
            .args(build_args(options, display, record_to))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    "ffmpeg was not found. Install it (for example `sudo pacman -S ffmpeg` \
                     or `sudo apt install ffmpeg`) to record the screen."
                        .to_string()
                } else {
                    format!("Could not start ffmpeg: {e}")
                }
            })?;
        // Keep the tail of ffmpeg's messages for error reports; draining the
        // pipe also stops ffmpeg from blocking on a full one.
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(mut pipe) = child.stderr.take() {
            let sink = Arc::clone(&stderr);
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                while let Ok(n) = pipe.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut s) = sink.lock() {
                        s.push_str(&String::from_utf8_lossy(&buf[..n]));
                        if s.len() > 4096 {
                            let cut = s.len() - 2048;
                            let cut = (cut..s.len()).find(|i| s.is_char_boundary(*i)).unwrap_or(0);
                            s.drain(..cut);
                        }
                    }
                }
            });
        }
        // A bad window id or display makes ffmpeg exit at once.
        std::thread::sleep(Duration::from_millis(400));
        if let Ok(Some(status)) = child.try_wait() {
            std::thread::sleep(Duration::from_millis(50));
            let msg = stderr
                .lock()
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            return Err(format!("ffmpeg stopped right away ({status}): {msg}"));
        }
        self.running = Some(Running {
            child,
            path: path.to_path_buf(),
            temp,
            program: program.to_string(),
            started: Instant::now(),
            stderr,
        });
        Ok(())
    }

    /// True while ffmpeg is running. A recording that died by itself is
    /// reaped here (its file stays).
    pub fn is_recording(&mut self) -> bool {
        let Some(r) = self.running.as_mut() else {
            return false;
        };
        if matches!(r.child.try_wait(), Ok(None)) {
            true
        } else {
            self.running = None;
            false
        }
    }

    /// How long the current recording has been running.
    pub fn elapsed(&self) -> Option<Duration> {
        self.running.as_ref().map(|r| r.started.elapsed())
    }

    /// The file being written.
    pub fn path(&self) -> Option<&Path> {
        self.running.as_ref().map(|r| r.path.as_path())
    }

    /// Finishes the recording: asks ffmpeg to quit and waits for it to close
    /// the file (up to 10 s, then it is killed). Returns the file.
    pub fn stop(&mut self) -> Result<PathBuf, String> {
        let Some(mut r) = self.running.take() else {
            return Err("No recording is running.".into());
        };
        if let Some(mut stdin) = r.child.stdin.take() {
            let _ = stdin.write_all(b"q\n");
            let _ = stdin.flush();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match r.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => {
                    let _ = r.child.kill();
                    let _ = r.child.wait();
                    break;
                }
            }
        }
        if let Some(temp) = &r.temp {
            let done = Command::new(&r.program)
                .args(gif_args(temp, &r.path))
                .stdin(Stdio::null())
                .output();
            let _ = std::fs::remove_file(temp);
            if !matches!(done, Ok(ref o) if o.status.success()) {
                return Err("Could not convert the recording to a GIF.".into());
            }
        }
        match std::fs::metadata(&r.path) {
            Ok(m) if m.len() > 0 => Ok(r.path),
            _ => {
                let msg = r
                    .stderr
                    .lock()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                Err(format!("The recording file is empty. {msg}"))
            }
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Never leave ffmpeg running after the browser quits.
        if self.running.is_some() {
            let _ = self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_command_line() {
        let o = Options::new(Target::Window(0x3a00007));
        let a = build_args(&o, ":0", Path::new("/tmp/o.mp4"));
        let joined = a.join(" ");
        assert!(joined.contains("-f x11grab"));
        assert!(joined.contains("-window_id 0x3a00007"));
        assert!(joined.contains("-framerate 30"));
        assert!(joined.contains("-i :0 "));
        assert!(joined.contains("libx264"));
        assert!(joined.contains("trunc(iw/2)*2"));
        assert_eq!(a.last().map(String::as_str), Some("/tmp/o.mp4"));
        // stdin must stay usable for the graceful stop.
        assert!(!a.iter().any(|s| s == "-nostdin"));
    }

    #[test]
    fn region_command_line() {
        let mut o = Options::new(Target::Region {
            x: 10,
            y: 20,
            w: 800,
            h: 600,
        });
        o.format = Format::Webm;
        o.fps = 500;
        o.draw_mouse = false;
        let joined = build_args(&o, ":1", Path::new("o.webm")).join(" ");
        assert!(joined.contains("-video_size 800x600"));
        assert!(joined.contains("-i :1+10,20"));
        assert!(joined.contains("-framerate 60"));
        assert!(joined.contains("-draw_mouse 0"));
        assert!(joined.contains("libvpx-vp9"));
    }

    #[test]
    fn gif_records_mp4_then_converts_with_a_palette() {
        let mut o = Options::new(Target::Window(1));
        o.format = Format::Gif;
        let joined = build_args(&o, ":0", Path::new("o.gif.mp4")).join(" ");
        assert!(joined.contains("libx264"));
        let conv = gif_args(Path::new("a.mp4"), Path::new("a.gif")).join(" ");
        assert!(conv.contains("palettegen") && conv.ends_with("a.gif"));
    }

    #[test]
    fn file_names() {
        assert_eq!(
            file_name("20260101-120000", Format::Mp4),
            "BrowseCraft-20260101-120000.mp4"
        );
        assert_eq!(file_name("1", Format::Gif), "BrowseCraft-1.gif");
    }

    #[test]
    fn missing_ffmpeg_is_a_clear_error() {
        let mut r = Recorder::new();
        let o = Options::new(Target::Window(1));
        let err = r
            .start_to(
                &o,
                ":0",
                Path::new("/tmp/never.mp4"),
                "definitely-not-ffmpeg-xyz",
            )
            .err()
            .unwrap_or_default();
        assert!(err.contains("ffmpeg was not found"), "{err}");
        assert!(!r.is_recording());
        assert!(r.stop().is_err());
    }

    /// Manual check against a real X11 window:
    /// `BC_REC_WINDOW=<xid> DISPLAY=:0 cargo test records_real_window -- --ignored`.
    #[test]
    #[ignore = "needs an X11 display, ffmpeg and a window id in BC_REC_WINDOW"]
    fn records_real_window() {
        let id: u32 = std::env::var("BC_REC_WINDOW")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let format = match std::env::var("BC_REC_FORMAT").as_deref() {
            Ok("webm") => Format::Webm,
            Ok("gif") => Format::Gif,
            _ => Format::Mp4,
        };
        let mut o = Options::new(Target::Window(id));
        o.format = format;
        let out = std::env::temp_dir().join(file_name("test", format));
        let mut r = Recorder::new();
        let display = std::env::var("DISPLAY").unwrap_or_default();
        r.start_to(&o, &display, &out, "ffmpeg").unwrap();
        assert!(r.is_recording());
        std::thread::sleep(Duration::from_secs(3));
        let file = r.stop().unwrap();
        assert!(!r.is_recording());
        println!("recorded {}", file.display());
    }
}
