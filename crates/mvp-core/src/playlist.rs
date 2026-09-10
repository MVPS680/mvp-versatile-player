//! Playlist model, M3U/PLS import and folder expansion.

use std::cmp::Ordering;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{MediaError, Result};
use crate::util::{self, MediaKind};

/// What happens when the current entry finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum RepeatMode {
    /// Stop at the end of the list.
    #[default]
    Off,
    /// Wrap around to the first entry.
    All,
    /// Repeat the current entry forever.
    One,
}

impl RepeatMode {
    /// Cycle `Off → All → One → Off`, the order every player's button uses.
    pub fn next(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::All,
            RepeatMode::All => RepeatMode::One,
            RepeatMode::One => RepeatMode::Off,
        }
    }

    /// Short label for the OSD.
    pub fn label(self) -> &'static str {
        match self {
            RepeatMode::Off => "不循环",
            RepeatMode::All => "列表循环",
            RepeatMode::One => "单曲循环",
        }
    }
}

/// One entry in the playlist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaylistItem {
    /// File path or URL.
    pub source: String,
    /// Display title; falls back to the file name.
    pub title: String,
    /// Duration in seconds once known.
    pub duration: Option<f64>,
    /// `true` when `source` is a network URL.
    pub is_url: bool,
}

impl PlaylistItem {
    /// Build an item from a path, inferring the title.
    pub fn from_path(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        let source = path.to_string_lossy().into_owned();
        let title = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| source.clone());
        Self {
            source,
            title,
            duration: None,
            is_url: false,
        }
    }

    /// Build an item from a URL or an arbitrary source string.
    pub fn from_url(source: impl Into<String>) -> Self {
        let source = source.into();
        let title = source
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or(&source)
            .to_string();
        Self {
            source,
            title,
            duration: None,
            is_url: true,
        }
    }

    /// The path form, for local entries.
    pub fn path(&self) -> PathBuf {
        PathBuf::from(&self.source)
    }

    /// `true` when this entry exists on disk (always true for URLs).
    pub fn exists(&self) -> bool {
        self.is_url || Path::new(&self.source).exists()
    }
}

/// An ordered list of media to play.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Playlist {
    items: Vec<PlaylistItem>,
    current: Option<usize>,
    repeat: RepeatMode,
    shuffle: bool,
    /// Playback order when shuffle is on: a permutation of `0..items.len()`.
    #[serde(skip)]
    order: Vec<usize>,
    /// Position inside `order`.
    #[serde(skip)]
    order_pos: usize,
}

impl Playlist {
    /// An empty playlist.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every entry, in list order.
    pub fn items(&self) -> &[PlaylistItem] {
        &self.items
    }

    /// Mutable access to every entry.
    pub fn items_mut(&mut self) -> &mut Vec<PlaylistItem> {
        &mut self.items
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// `true` when there is nothing to play.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Index of the entry being played.
    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    /// The entry being played.
    pub fn current(&self) -> Option<&PlaylistItem> {
        self.current.and_then(|i| self.items.get(i))
    }

    /// Replace the whole list, keeping playback stopped.
    pub fn set_items(&mut self, items: Vec<PlaylistItem>) {
        self.items = items;
        self.current = None;
        self.rebuild_order();
    }

    /// Append an entry and return its index.
    pub fn add(&mut self, item: PlaylistItem) -> usize {
        self.items.push(item);
        self.rebuild_order();
        self.items.len() - 1
    }

    /// Append several entries.
    pub fn extend(&mut self, items: impl IntoIterator<Item = PlaylistItem>) {
        self.items.extend(items);
        self.rebuild_order();
    }

    /// Insert at `index` (clamped).
    pub fn insert(&mut self, index: usize, item: PlaylistItem) {
        let index = index.min(self.items.len());
        self.items.insert(index, item);
        if let Some(current) = self.current {
            if index <= current {
                self.current = Some(current + 1);
            }
        }
        self.rebuild_order();
    }

    /// Remove the entry at `index`, keeping `current` pointing at the same file
    /// where possible. Returns what was removed.
    pub fn remove(&mut self, index: usize) -> Option<PlaylistItem> {
        if index >= self.items.len() {
            return None;
        }
        let removed = self.items.remove(index);
        match self.current {
            Some(current) if current == index => {
                self.current = if self.items.is_empty() {
                    None
                } else {
                    Some(current.min(self.items.len() - 1))
                };
            }
            Some(current) if current > index => self.current = Some(current - 1),
            _ => {}
        }
        self.rebuild_order();
        Some(removed)
    }

    /// Move an entry, used by drag & drop in the playlist panel.
    pub fn move_item(&mut self, from: usize, to: usize) {
        if from >= self.items.len() || to >= self.items.len() || from == to {
            return;
        }
        let item = self.items.remove(from);
        self.items.insert(to, item);
        self.current = match self.current {
            Some(current) if current == from => Some(to),
            Some(current) => {
                let mut c = current;
                if from < current {
                    c -= 1;
                }
                if to <= c {
                    c += 1;
                }
                Some(c)
            }
            None => None,
        };
        self.rebuild_order();
    }

    /// Remove everything and stop.
    pub fn clear(&mut self) {
        self.items.clear();
        self.current = None;
        self.order.clear();
        self.order_pos = 0;
    }

    /// Remove every entry that does not exist on disk any more.
    pub fn prune_missing(&mut self) -> usize {
        let before = self.items.len();
        let current_source = self.current().map(|i| i.source.clone());
        self.items.retain(|i| i.exists());
        self.current = current_source
            .and_then(|s| self.items.iter().position(|i| i.source == s))
            .or(if self.items.is_empty() { None } else { Some(0) });
        self.rebuild_order();
        before - self.items.len()
    }

    /// Mark an index as playing.
    pub fn set_current(&mut self, index: Option<usize>) {
        self.current = index.filter(|i| *i < self.items.len());
        if let (Some(index), true) = (self.current, self.shuffle) {
            if let Some(pos) = self.order.iter().position(|i| *i == index) {
                self.order_pos = pos;
            }
        }
    }

    /// The index that should play after the current one.
    ///
    /// `auto` is `true` when the current entry ended by itself, which is what
    /// makes [`RepeatMode::One`] behave; pressing "next" manually always moves on.
    pub fn next_index(&self, auto: bool) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        if auto && self.repeat == RepeatMode::One {
            return self.current.or(Some(0));
        }
        if self.shuffle && self.order.len() == self.items.len() {
            let next_pos = self.order_pos + 1;
            if next_pos < self.order.len() {
                return Some(self.order[next_pos]);
            }
            return match self.repeat {
                RepeatMode::Off if auto => None,
                _ => self.order.first().copied(),
            };
        }
        let current = self.current.unwrap_or(0);
        let next = current + 1;
        if next < self.items.len() {
            Some(next)
        } else {
            match self.repeat {
                RepeatMode::Off if auto => None,
                _ => Some(0),
            }
        }
    }

    /// The index that should play before the current one.
    pub fn prev_index(&self) -> Option<usize> {
        if self.items.is_empty() {
            return None;
        }
        if self.shuffle && self.order.len() == self.items.len() {
            let pos = self.order_pos.checked_sub(1).unwrap_or(self.order.len() - 1);
            return self.order.get(pos).copied();
        }
        let current = self.current.unwrap_or(0);
        if current == 0 {
            Some(self.items.len() - 1)
        } else {
            Some(current - 1)
        }
    }

    /// Current repeat mode.
    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    /// Set the repeat mode.
    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
    }

    /// `true` when shuffle is on.
    pub fn shuffle(&self) -> bool {
        self.shuffle
    }

    /// Turn shuffle on or off. The current entry stays current.
    pub fn set_shuffle(&mut self, on: bool) {
        if self.shuffle == on {
            return;
        }
        self.shuffle = on;
        self.rebuild_order();
    }

    /// Recompute the shuffled order. Deterministic given the same list length so
    /// that toggling shuffle twice is not surprising; a real shuffle is applied
    /// because the seed comes from the wall clock.
    fn rebuild_order(&mut self) {
        self.order = (0..self.items.len()).collect();
        if self.shuffle {
            // Fisher–Yates with a cheap xorshift seeded from the clock.
            let mut seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E37_79B9_7F4A_7C15)
                | 1;
            for i in (1..self.order.len()).rev() {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let j = (seed % (i as u64 + 1)) as usize;
                self.order.swap(i, j);
            }
        }
        if let Some(current) = self.current {
            self.order_pos = self
                .order
                .iter()
                .position(|i| *i == current)
                .unwrap_or(0);
        } else {
            self.order_pos = 0;
        }
    }

    /// Write the list as an extended M3U file.
    pub fn save_m3u(&self, path: impl AsRef<Path>) -> Result<()> {
        let mut file = std::fs::File::create(path.as_ref())?;
        writeln!(file, "#EXTM3U")?;
        for item in &self.items {
            if let Some(duration) = item.duration {
                writeln!(file, "#EXTINF:{:.3},{}", duration, item.title)?;
            } else {
                writeln!(file, "#EXTINF:-1,{}", item.title)?;
            }
            writeln!(file, "{}", item.source)?;
        }
        file.flush()?;
        Ok(())
    }
}

/// Read an `M3U`/`M3U8`/`PLS` file into playlist entries.
///
/// Relative entries are resolved against the playlist's own directory, which is
/// what every other player does and what makes portable playlists work.
pub fn read_playlist_file(path: &Path) -> Result<Vec<PlaylistItem>> {
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let extension = util::extension_of(path).unwrap_or_default();
    let mut items = Vec::new();
    let mut pending_title: Option<String> = None;
    let mut pending_duration: Option<f64> = None;

    if extension == "pls" {
        for line in reader.lines().map_while(std::result::Result::ok) {
            let line = line.trim();
            if let Some(rest) = line
                .strip_prefix("File")
                .and_then(|r| r.split_once('='))
                .map(|(_, v)| v.trim())
            {
                items.push(resolve_entry(rest, &base));
            } else if let Some(rest) = line
                .strip_prefix("Title")
                .and_then(|r| r.split_once('='))
                .map(|(_, v)| v.trim())
            {
                if let Some(last) = items.last_mut() {
                    last.title = rest.to_string();
                }
            } else if let Some(rest) = line
                .strip_prefix("Length")
                .and_then(|r| r.split_once('='))
                .map(|(_, v)| v.trim())
            {
                if let Some(last) = items.last_mut() {
                    last.duration = rest.parse::<f64>().ok().filter(|d| *d > 0.0);
                }
            }
        }
        return Ok(items);
    }

    for line in reader.lines().map_while(std::result::Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            // `#EXTINF:<seconds>,<title>`
            let (duration, title) = rest.split_once(',').unwrap_or((rest, ""));
            pending_duration = duration.trim().parse::<f64>().ok().filter(|d| *d > 0.0);
            pending_title = Some(title.trim().to_string()).filter(|t| !t.is_empty());
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let mut item = resolve_entry(line, &base);
        if let Some(title) = pending_title.take() {
            item.title = title;
        }
        item.duration = pending_duration.take();
        items.push(item);
    }
    Ok(items)
}

fn resolve_entry(entry: &str, base: &Path) -> PlaylistItem {
    if util::is_url(entry) {
        return PlaylistItem::from_url(entry);
    }
    let candidate = PathBuf::from(entry);
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        base.join(candidate)
    };
    PlaylistItem::from_path(resolved)
}

/// `true` when `path` is an HLS manifest (a `.m3u8` that describes segments)
/// rather than a user playlist listing separate files.
pub fn is_hls_manifest(path: &Path) -> bool {
    if util::extension_of(path).as_deref() != Some("m3u8") {
        return false;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    BufReader::new(file)
        .lines()
        .map_while(std::result::Result::ok)
        .take(64)
        .any(|line| {
            let line = line.trim();
            line.starts_with("#EXT-X-STREAM-INF")
                || line.starts_with("#EXT-X-TARGETDURATION")
                || line.starts_with("#EXT-X-MEDIA-SEQUENCE")
        })
}

/// Extensions the player will pull out of a folder, in the order a folder is
/// expanded.
fn is_supported_media(path: &Path) -> bool {
    matches!(
        util::classify(path),
        MediaKind::Video | MediaKind::Audio | MediaKind::Image
    )
}

/// Expand a mixed list of files and folders into a flat, naturally sorted list
/// of media paths.
///
/// Folders are walked recursively, playlist files are replaced by their entries,
/// and everything else is passed through. This is what "Open folder…" and a
/// drag-and-drop of a directory both go through.
pub fn expand_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut found: Vec<PathBuf> = walkdir::WalkDir::new(path)
                .max_depth(4)
                .follow_links(false)
                .into_iter()
                .filter_map(std::result::Result::ok)
                .filter(|e| e.file_type().is_file())
                .map(|e| e.into_path())
                .filter(|p| is_supported_media(p))
                .collect();
            found.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
            out.extend(found);
        } else if util::classify(path) == MediaKind::Playlist && !is_hls_manifest(path) {
            match read_playlist_file(path) {
                Ok(items) => out.extend(items.into_iter().map(|i| i.path())),
                Err(err) => log::warn!("无法读取播放列表 {}: {err}", path.display()),
            }
        } else {
            out.push(path.clone());
        }
    }
    out
}

/// Sort paths the way humans expect: `ep2` before `ep10`, and case differences
/// only break ties between otherwise identical names.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let ordering = natural_cmp_ignore_case(a, b);
    if ordering == Ordering::Equal {
        a.cmp(b)
    } else {
        ordering
    }
}

fn natural_cmp_ignore_case(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ac), Some(bc)) => {
                if ac.is_ascii_digit() && bc.is_ascii_digit() {
                    let mut an = String::new();
                    while ai.peek().is_some_and(|c| c.is_ascii_digit()) {
                        an.push(ai.next().unwrap_or('0'));
                    }
                    let mut bn = String::new();
                    while bi.peek().is_some_and(|c| c.is_ascii_digit()) {
                        bn.push(bi.next().unwrap_or('0'));
                    }
                    let av = an.trim_start_matches('0');
                    let bv = bn.trim_start_matches('0');
                    // A longer digit run is a bigger number, so compare by
                    // length first and only then lexically (avoids overflow on
                    // absurdly long runs).
                    let ord = av
                        .len()
                        .cmp(&bv.len())
                        .then_with(|| av.cmp(bv))
                        .then_with(|| an.len().cmp(&bn.len()));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                } else {
                    let ord = ac.to_ascii_lowercase().cmp(&bc.to_ascii_lowercase());
                    if ord != Ordering::Equal {
                        return ord;
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

/// Expand a list of files into playlist items, skipping anything unreadable.
pub fn items_from_paths(paths: &[PathBuf]) -> Vec<PlaylistItem> {
    expand_paths(paths)
        .into_iter()
        .map(PlaylistItem::from_path)
        .collect()
}

/// Validate that a playlist can actually be played, producing a friendly error.
pub fn ensure_playable(item: &PlaylistItem) -> Result<()> {
    if item.is_url {
        return Ok(());
    }
    let path = item.path();
    if !path.exists() {
        return Err(MediaError::other(format!("文件不存在: {}", path.display())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str) -> PlaylistItem {
        PlaylistItem {
            source: name.to_string(),
            title: name.to_string(),
            duration: None,
            is_url: false,
        }
    }

    #[test]
    fn repeat_mode_cycles() {
        assert_eq!(RepeatMode::Off.next(), RepeatMode::All);
        assert_eq!(RepeatMode::All.next(), RepeatMode::One);
        assert_eq!(RepeatMode::One.next(), RepeatMode::Off);
    }

    #[test]
    fn next_and_previous_wrap_only_when_repeating() {
        let mut list = Playlist::new();
        for i in 0..3 {
            list.add(item(&format!("f{i}.mp4")));
        }
        list.set_current(Some(2));
        assert_eq!(list.next_index(true), None, "no repeat: playback stops");
        assert_eq!(list.next_index(false), Some(0), "manual next wraps");
        list.set_repeat(RepeatMode::All);
        assert_eq!(list.next_index(true), Some(0));
        list.set_repeat(RepeatMode::One);
        assert_eq!(list.next_index(true), Some(2));
        assert_eq!(list.prev_index(), Some(1));
        list.set_current(Some(0));
        assert_eq!(list.prev_index(), Some(2));
    }

    #[test]
    fn removing_entries_keeps_the_current_one() {
        let mut list = Playlist::new();
        for i in 0..4 {
            list.add(item(&format!("f{i}.mp4")));
        }
        list.set_current(Some(2));
        list.remove(0);
        assert_eq!(list.current_index(), Some(1));
        assert_eq!(list.current().map(|i| i.title.as_str()), Some("f2.mp4"));
        list.remove(1);
        assert_eq!(list.current().map(|i| i.title.as_str()), Some("f3.mp4"));
    }

    #[test]
    fn moving_entries_follows_the_current_one() {
        let mut list = Playlist::new();
        for i in 0..4 {
            list.add(item(&format!("f{i}.mp4")));
        }
        list.set_current(Some(1));
        list.move_item(1, 3);
        assert_eq!(list.current_index(), Some(3));
        assert_eq!(list.current().map(|i| i.title.as_str()), Some("f1.mp4"));
    }

    #[test]
    fn shuffle_keeps_every_entry_exactly_once() {
        let mut list = Playlist::new();
        for i in 0..32 {
            list.add(item(&format!("f{i}.mp4")));
        }
        list.set_current(Some(5));
        list.set_shuffle(true);
        let mut order = list.order.clone();
        order.sort_unstable();
        assert_eq!(order, (0..32).collect::<Vec<_>>());
        // The current entry stays current through the shuffle.
        assert_eq!(list.current_index(), Some(5));
    }

    #[test]
    fn natural_sort_orders_numbers_as_numbers() {
        let mut names = vec!["ep10.mp4", "ep2.mp4", "ep1.mp4", "EP3.mp4"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["ep1.mp4", "ep2.mp4", "EP3.mp4", "ep10.mp4"]);
    }

    #[test]
    fn m3u_round_trips() {
        let dir = std::env::temp_dir().join("mvp-playlist-test");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("list.m3u");
        let mut list = Playlist::new();
        let mut a = item("a.mp4");
        a.duration = Some(12.5);
        list.add(a);
        list.add(item("b.mp4"));
        list.save_m3u(&file).expect("write m3u");
        let back = read_playlist_file(&file).expect("read m3u");
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].title, "a.mp4");
        assert!((back[0].duration.unwrap_or_default() - 12.5).abs() < 1e-6);
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn pls_is_understood() {
        let dir = std::env::temp_dir().join("mvp-playlist-test");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("list.pls");
        std::fs::write(
            &file,
            "[playlist]\nNumberOfEntries=2\nFile1=a.mp4\nTitle1=First\nLength1=61\nFile2=b.mp4\n",
        )
        .unwrap();
        let items = read_playlist_file(&file).expect("read pls");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "First");
        assert_eq!(items[0].duration, Some(61.0));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn relative_entries_resolve_against_the_playlist() {
        let dir = std::env::temp_dir().join("mvp-playlist-rel");
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("rel.m3u");
        std::fs::write(&file, "#EXTM3U\nsub/movie.mkv\n").unwrap();
        let items = read_playlist_file(&file).expect("read");
        assert_eq!(items.len(), 1);
        assert!(items[0].source.ends_with("sub\\movie.mkv") || items[0].source.ends_with("sub/movie.mkv"));
        assert!(Path::new(&items[0].source).is_absolute());
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn pruning_drops_entries_that_vanished() {
        let mut list = Playlist::new();
        list.add(PlaylistItem::from_url("https://example.com/a.mp4"));
        list.add(item("definitely-missing-file.mp4"));
        let removed = list.prune_missing();
        assert_eq!(removed, 1);
        assert_eq!(list.len(), 1);
    }
}
