//! Small pictures of the tabs' pages, for the tab overview and the hover
//! cards.
//!
//! The browser asks WebKit for a snapshot of a page when its tab is hidden
//! and a moment after a page loads; the snapshot is scaled down to at most
//! [`MAX_WIDTH`] pixels wide and kept here, keyed by tab id, within a byte
//! budget (least recently used first out). Unloaded (suspended) tabs keep
//! their last picture. The pictures of tabs that are saved in the session
//! are also written to disk as small JPEGs (`thumbnails/` in the profile
//! folder, owner-only), keyed by a hash of the address, so a restored
//! session shows them before the tabs load again.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// The widest picture kept, in pixels.
pub const MAX_WIDTH: usize = 480;
/// The tallest picture kept: pages are cropped to a landscape card.
pub const MAX_HEIGHT: usize = 360;
/// The default memory budget for all the pictures together.
pub const DEFAULT_CAP: usize = 24 * 1024 * 1024;
/// The largest thumbnail file read back from disk.
const MAX_FILE: u64 = 512 * 1024;
/// At most this many thumbnail files are kept on disk.
const MAX_FILES: usize = 400;

/// One page picture: straight RGBA, `w × h`.
#[derive(Clone, Debug)]
pub struct Thumb {
    pub w: usize,
    pub h: usize,
    pub rgba: Arc<Vec<u8>>,
    /// Bumped on every new picture of the tab, so textures know when to
    /// upload again.
    pub stamp: u64,
}

impl Thumb {
    fn bytes(&self) -> usize {
        self.rgba.len()
    }
}

/// The pictures, by tab id, with a byte cap.
pub struct ThumbnailCache {
    entries: HashMap<u64, (Thumb, u64)>,
    bytes: usize,
    cap: usize,
    /// Use counter for least-recently-used eviction.
    clock: u64,
    next_stamp: u64,
}

impl Default for ThumbnailCache {
    fn default() -> Self {
        ThumbnailCache::new(DEFAULT_CAP)
    }
}

impl ThumbnailCache {
    pub fn new(cap: usize) -> Self {
        ThumbnailCache { entries: HashMap::new(), bytes: 0, cap, clock: 0, next_stamp: 1 }
    }

    /// Keeps a new picture of tab `tab` (replacing its old one) and evicts
    /// the least recently used pictures beyond the cap. A picture larger
    /// than the whole cap, or malformed, is refused. Returns its stamp.
    pub fn insert(&mut self, tab: u64, w: usize, h: usize, rgba: Vec<u8>) -> Option<u64> {
        if w == 0 || h == 0 || rgba.len() != w * h * 4 || rgba.len() > self.cap {
            return None;
        }
        self.remove(tab);
        self.clock += 1;
        let stamp = self.next_stamp;
        self.next_stamp += 1;
        let thumb = Thumb { w, h, rgba: Arc::new(rgba), stamp };
        self.bytes += thumb.bytes();
        self.entries.insert(tab, (thumb, self.clock));
        while self.bytes > self.cap {
            let Some(oldest) = self.entries.iter().filter(|(id, _)| **id != tab).min_by_key(|(_, (_, used))| *used).map(|(id, _)| *id) else {
                break;
            };
            self.remove(oldest);
        }
        Some(stamp)
    }

    /// Tab `tab`'s picture, marked as just used.
    pub fn get(&mut self, tab: u64) -> Option<&Thumb> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(&tab).map(|(thumb, used)| {
            *used = clock;
            &*thumb
        })
    }

    /// Tab `tab`'s picture without marking it used.
    pub fn peek(&self, tab: u64) -> Option<&Thumb> {
        self.entries.get(&tab).map(|(t, _)| t)
    }

    pub fn contains(&self, tab: u64) -> bool {
        self.entries.contains_key(&tab)
    }

    pub fn remove(&mut self, tab: u64) {
        if let Some((thumb, _)) = self.entries.remove(&tab) {
            self.bytes -= thumb.bytes();
        }
    }

    /// Drops the pictures of tabs that are gone.
    pub fn retain(&mut self, keep: impl Fn(u64) -> bool) {
        let gone: Vec<u64> = self.entries.keys().copied().filter(|id| !keep(*id)).collect();
        for id in gone {
            self.remove(id);
        }
    }

    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// The size a page `w × h` is scaled to: at most `MAX_WIDTH` wide, and its
/// top cropped to a 4:3 landscape at most (a very tall page is cut, as a
/// tab card shows the top of the page).
pub fn target_size(w: usize, h: usize) -> (usize, usize, usize) {
    if w == 0 || h == 0 {
        return (0, 0, 0);
    }
    // The part of the page kept: no taller than 3/4 of its width.
    let crop_h = h.min(w * 3 / 4).max(1);
    let tw = w.clamp(1, MAX_WIDTH);
    let th = (crop_h * tw / w).clamp(1, MAX_HEIGHT);
    (tw, th, crop_h)
}

// ---- on disk ----------------------------------------------------------------

/// The folder of the saved pictures (owner-only).
fn dir() -> Result<PathBuf, String> {
    crate::storage::profile_dir("thumbnails")
}

/// The file name of `url`'s picture: a 64-bit FNV-1a hash, so the folder
/// lists no addresses.
pub fn file_name(url: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}.jpg")
}

/// Writes `url`'s picture as a JPEG on a background thread.
pub fn save(url: &str, thumb: &Thumb) {
    let (url, w, h, rgba) = (url.to_string(), thumb.w, thumb.h, thumb.rgba.clone());
    std::thread::spawn(move || {
        let Ok(dir) = dir() else { return };
        if crate::storage::private_dir(&dir).is_err() {
            return;
        }
        let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let mut jpeg = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 72);
        if image::ImageEncoder::write_image(encoder, &rgb, w as u32, h as u32, image::ExtendedColorType::Rgb8).is_err() {
            return;
        }
        let _ = crate::storage::write_private(&dir.join(file_name(&url)), &jpeg);
    });
}

/// Reads `url`'s saved picture (bounded in size), as RGBA.
pub fn load(url: &str) -> Option<(usize, usize, Vec<u8>)> {
    let path = dir().ok()?.join(file_name(url));
    let bytes = crate::storage::read_limited(&path, MAX_FILE).ok()?;
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).ok()?;
    if image.width() as usize > MAX_WIDTH || image.height() as usize > MAX_HEIGHT {
        return None;
    }
    let rgba = image.into_rgba8();
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    Some((w, h, rgba.into_raw()))
}

/// Deletes the saved pictures of addresses no longer in the session (and
/// the oldest beyond `MAX_FILES`).
pub fn prune(keep_urls: &[String]) {
    let keep: std::collections::HashSet<String> = keep_urls.iter().map(|u| file_name(u)).collect();
    std::thread::spawn(move || {
        let Ok(dir) = dir() else { return };
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        let mut kept = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".jpg") {
                continue;
            }
            if keep.contains(&name) {
                let modified = entry.metadata().and_then(|m| m.modified()).ok();
                kept.push((modified, entry.path()));
            } else {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        if kept.len() > MAX_FILES {
            kept.sort_by_key(|(m, _)| *m);
            for (_, path) in kept.iter().take(kept.len() - MAX_FILES) {
                let _ = std::fs::remove_file(path);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(w: usize, h: usize) -> Vec<u8> {
        vec![7; w * h * 4]
    }

    #[test]
    fn the_cache_stays_under_its_cap() {
        let mut cache = ThumbnailCache::new(1000);
        assert!(cache.insert(1, 10, 10, px(10, 10)).is_some()); // 400 bytes
        assert!(cache.insert(2, 10, 10, px(10, 10)).is_some());
        assert_eq!(cache.bytes(), 800);
        // Tab 1 was used last: tab 2 goes first.
        assert!(cache.get(1).is_some());
        assert!(cache.insert(3, 10, 10, px(10, 10)).is_some());
        assert!(cache.bytes() <= 1000);
        assert!(cache.contains(1) && cache.contains(3) && !cache.contains(2));
        // Replacing a picture does not count it twice.
        assert!(cache.insert(3, 5, 5, px(5, 5)).is_some());
        assert_eq!(cache.bytes(), 400 + 100);
    }

    #[test]
    fn bad_or_huge_pictures_are_refused() {
        let mut cache = ThumbnailCache::new(1000);
        assert!(cache.insert(1, 10, 10, vec![0; 3]).is_none(), "wrong length");
        assert!(cache.insert(1, 0, 10, Vec::new()).is_none(), "empty");
        assert!(cache.insert(1, 20, 20, px(20, 20)).is_none(), "over the whole cap");
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn stamps_change_with_each_picture() {
        let mut cache = ThumbnailCache::new(10_000);
        let a = cache.insert(1, 2, 2, px(2, 2));
        let b = cache.insert(1, 2, 2, px(2, 2));
        assert!(a.is_some() && b.is_some() && a != b);
        assert_eq!(cache.peek(1).map(|t| t.stamp), b);
        cache.retain(|id| id != 1);
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn pages_are_scaled_and_cropped_to_a_card() {
        assert_eq!(target_size(1280, 800), (480, 300, 800));
        // A tall page keeps its top 4:3.
        assert_eq!(target_size(1000, 5000), (480, 360, 750));
        // Small pages are not enlarged.
        assert_eq!(target_size(300, 200), (300, 200, 200));
        assert_eq!(target_size(0, 10), (0, 0, 0));
    }

    #[test]
    fn file_names_hide_the_address() {
        let name = file_name("https://example.com/secret?q=1");
        assert!(name.ends_with(".jpg") && !name.contains("example"));
        assert_ne!(name, file_name("https://example.com/other"));
        assert_eq!(name.len(), 20);
    }
}
