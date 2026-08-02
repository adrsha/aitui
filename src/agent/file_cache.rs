//! In-memory cache of accessed file contents.
//!
//! The executor consults this cache before touching the disk: a file read once
//! in the process (and unchanged since — checked by mtime) is served from
//! memory, so repeated reads of the same file cost no IO. Mutating tools
//! invalidate or refresh the entry so the cache never serves stale content.
//! The cache is process-global and shared by the main agent and every child.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone)]
struct CacheEntry {
    /// Nanosecond mtime at load time; used to detect on-disk changes.
    mtime_ns: u128,
    content: String,
}

pub struct FileCache {
    entries: Mutex<HashMap<String, CacheEntry>>,
    hits: Mutex<u64>,
    misses: Mutex<u64>,
}

impl FileCache {
    fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            hits: Mutex::new(0),
            misses: Mutex::new(0),
        }
    }

    /// Serve `path` from cache if its mtime is unchanged; otherwise read the
    /// disk. Returns `(content, cached)`.
    pub fn read(&self, path: &Path) -> Result<(String, bool), String> {
        let mtime = file_mtime_ns(path).ok();
        let key = path.to_string_lossy().into_owned();
        if let Some(mtime) = mtime {
            let entries = self.entries.lock().unwrap();
            if let Some(entry) = entries.get(&key) {
                if entry.mtime_ns == mtime {
                    self.hits.lock().unwrap().wrapped_add(1);
                    return Ok((entry.content.clone(), true));
                }
            }
        }
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
        if let Some(mtime) = mtime {
            let entry = CacheEntry {
                mtime_ns: mtime,
                content: content.clone(),
            };
            self.entries.lock().unwrap().insert(key, entry);
        }
        self.misses.lock().unwrap().wrapped_add(1);
        Ok((content, false))
    }

    /// Read a file, returning only its content (cache-first). Exists so call
    /// sites that just need the text stay terse.
    pub fn read_content(&self, path: &Path) -> Result<String, String> {
        self.read(path).map(|(content, _)| content)
    }

    /// Record freshly written content (post-write/edit) so the next read of the
    /// same file is served from memory without re-reading the disk.
    pub fn store(&self, path: &Path, content: &str) {
        let mtime = file_mtime_ns(path).unwrap_or(0);
        let entry = CacheEntry {
            mtime_ns: mtime,
            content: content.to_string(),
        };
        self.entries
            .lock()
            .unwrap()
            .insert(path.to_string_lossy().into_owned(), entry);
    }

    /// Drop the entry for `path` (deletes, moves, downloads, failed writes).
    pub fn invalidate(&self, path: &Path) {
        self.entries
            .lock()
            .unwrap()
            .remove(&path.to_string_lossy().into_owned());
    }

    /// Number of cache hits so far (observable in tests).
    #[allow(dead_code)]
    pub fn hit_count(&self) -> u64 {
        *self.hits.lock().unwrap()
    }

    /// Number of cache misses so far (observable in tests).
    #[allow(dead_code)]
    pub fn miss_count(&self) -> u64 {
        *self.misses.lock().unwrap()
    }

    #[allow(dead_code)]
    pub fn stats(&self) -> (u64, u64) {
        (self.hit_count(), self.miss_count())
    }
}

trait WrappedAdd {
    fn wrapped_add(&mut self, value: u64);
}

impl WrappedAdd for u64 {
    fn wrapped_add(&mut self, value: u64) {
        *self = self.wrapping_add(value);
    }
}

fn file_mtime_ns(path: &Path) -> Result<u128, String> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .map(|modified| {
            modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        })
        .map_err(|e| format!("Cannot stat {}: {}", path.display(), e))
}

/// Process-global cache shared by the main agent and every child agent.
pub fn global_cache() -> &'static FileCache {
    static CACHE: OnceLock<FileCache> = OnceLock::new();
    CACHE.get_or_init(FileCache::new)
}

/// Cache-first read of a file's contents (convenience wrapper).
pub fn read_file(path: &Path) -> Result<(String, bool), String> {
    global_cache().read(path)
}

/// Cache-first read of a file's contents (content only).
pub fn read_file_content(path: &Path) -> Result<String, String> {
    global_cache().read_content(path)
}

/// Invalidate a path after a mutating operation.
pub fn invalidate(path: &Path) {
    global_cache().invalidate(path);
}

/// Store fresh content after a successful write/edit.
pub fn store(path: &Path, content: &str) {
    global_cache().store(path, content);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(content: &str) -> (std::path::PathBuf, std::fs::File) {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "aitui_file_cache_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = std::fs::File::create(&path).unwrap();
        std::fs::write(&path, content).unwrap();
        (path, file)
    }

    #[test]
    fn serves_from_cache_after_first_read() {
        let (path, _file) = temp_file("hello world\n");
        let cache = FileCache::new();
        let (first, cached) = cache.read(&path).unwrap();
        assert_eq!(first, "hello world\n");
        assert!(!cached);
        assert_eq!(cache.miss_count(), 1);
        let (second, cached) = cache.read(&path).unwrap();
        assert_eq!(second, "hello world\n");
        assert!(cached);
        assert_eq!(cache.hit_count(), 1);
    }

    #[test]
    fn invalidate_forces_reload() {
        let (path, _file) = temp_file("v1\n");
        let cache = FileCache::new();
        cache.read(&path).unwrap();
        cache.invalidate(&path);
        cache.read(&path).unwrap();
        assert_eq!(cache.hit_count(), 0);
        assert_eq!(cache.miss_count(), 2);
    }

    #[test]
    fn store_refreshes_entry() {
        let (path, _file) = temp_file("old\n");
        let cache = FileCache::new();
        cache.read(&path).unwrap();
        std::fs::write(&path, "new\n").unwrap();
        cache.store(&path, "new\n");
        let (content, cached) = cache.read(&path).unwrap();
        assert_eq!(content, "new\n");
        assert!(cached);
    }

    #[test]
    fn mtime_change_detects_external_edit() {
        let (path, _file) = temp_file("old\n");
        let cache = FileCache::new();
        cache.read(&path).unwrap();
        std::fs::write(&path, "new\n").unwrap();
        let (content, cached) = cache.read(&path).unwrap();
        assert_eq!(content, "new\n");
        assert!(!cached);
    }

    #[test]
    fn missing_file_is_an_error_not_a_cache_entry() {
        let path = std::path::PathBuf::from("/nonexistent/aitui_missing_file");
        let cache = FileCache::new();
        assert!(cache.read(&path).is_err());
        assert_eq!(cache.miss_count(), 0);
    }
}
