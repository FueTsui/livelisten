//! One-way, daily Markdown views of cards. JSON remains the complete source of truth.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::{Card, CardBook, CardKind};

const FOLDERS: [&str; 3] = ["进行中", "已完成", "已归档"];
const MARKER: &str = "<!-- LiveListen daily notes v1; content-fnv1a=";
const PROTECTION_FILE: &str = ".livelisten-protected.json";
const PROTECTION_FORMAT: &str = "LiveListen protected daily notes v1";
static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Protection {
    format: String,
    files: BTreeMap<String, String>,
}

impl Default for Protection {
    fn default() -> Self {
        Self { format: PROTECTION_FORMAT.into(), files: BTreeMap::new() }
    }
}

impl Protection {
    fn contains(&self, root: &Path, path: &Path, bytes: &[u8]) -> bool {
        let Ok(relative) = path.strip_prefix(root) else { return true };
        let key = relative.to_string_lossy().replace('\\', "/");
        self.files.get(&key).is_some_and(|hash| u64::from_str_radix(hash, 16).is_ok_and(|hash| hash == checksum(bytes)))
    }
}

fn read_protection(root: &Path) -> io::Result<(Protection, Option<Vec<u8>>)> {
    let bytes = read_regular(&root.join(PROTECTION_FILE))?;
    let Some(bytes) = bytes else { return Ok((Protection::default(), None)) };
    if bytes.len() > 1024 * 1024 {
        return Err(conflict());
    }
    let protection: Protection = serde_json::from_slice(&bytes).map_err(|_| conflict())?;
    if protection.format != PROTECTION_FORMAT
        || protection.files.iter().any(|(path, hash)| {
            let Some((folder, file)) = path.split_once('/') else { return true };
            !FOLDERS.contains(&folder)
                || !date_filename(file)
                || hash.len() != 16
                || u64::from_str_radix(hash, 16).is_err()
        })
    {
        return Err(conflict());
    }
    Ok((protection, Some(bytes)))
}

/// Before a missing/corrupt source is replaced, persist ownership exceptions for its old
/// exports. They remain protected across new notes and restarts, without moving user files.
pub(super) fn protect_orphans(config_dir: &Path) -> io::Result<()> {
    let root = config_dir.join("随手记");
    match fs::symlink_metadata(&root) {
        Ok(metadata) if !metadata.is_dir() || is_link(&metadata) => return Ok(()),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    let (mut protection, previous) = read_protection(&root)?;
    let mut changed = false;
    for folder in FOLDERS {
        let directory = root.join(folder);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if !metadata.is_dir() || is_link(&metadata) => continue,
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str().filter(|name| date_filename(name)) else { continue };
            let metadata = fs::symlink_metadata(entry.path())?;
            if !metadata.is_file() || is_link(&metadata) {
                continue;
            }
            let bytes = fs::read(entry.path())?;
            if is_unchanged_generated(&bytes) {
                let key = format!("{folder}/{name}");
                let hash = format!("{:016x}", checksum(&bytes));
                if protection.files.get(&key) != Some(&hash) {
                    protection.files.insert(key, hash);
                    changed = true;
                }
            }
        }
    }
    if changed {
        write_observed(&root.join(PROTECTION_FILE), &serde_json::to_vec_pretty(&protection)?, previous)?;
    }
    Ok(())
}

fn status(card: &Card) -> &'static str {
    if card.archived {
        FOLDERS[2]
    } else if card.done {
        FOLDERS[1]
    } else {
        FOLDERS[0]
    }
}

/// Uses the same system time zone as DateTimeOffset.ToLocalTime in the Windows UI.
fn local_stamp(ms: u64) -> String {
    let ms = ms.min(253_402_300_799_999); // Last millisecond in the four-digit year range.
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTimeEx};
        let ticks = (ms + 11_644_473_600_000) * 10_000;
        let file = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
        let mut utc = SYSTEMTIME::default();
        let mut local = SYSTEMTIME::default();
        // SAFETY: Both output pointers refer to initialized, correctly sized local structs;
        // a null time-zone pointer asks Windows for the current system's dynamic rules.
        let converted = unsafe {
            FileTimeToSystemTime(&file, &mut utc)
                .and_then(|()| SystemTimeToTzSpecificLocalTimeEx(None, &utc, &mut local))
        };
        if converted.is_ok() && local.wYear <= 9999 {
            return format!(
                "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
            );
        }
    }
    crate::paths::iso_from_ms(ms)[..19].replace('T', " ")
}

// An edit-detection checksum, not an authentication or security signature.
fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3))
}

fn generated(body: &str) -> Vec<u8> {
    format!("{MARKER}{:016x} -->\n{body}", checksum(body.as_bytes())).into_bytes()
}

/// The marker alone does not authorize replacement: manual changes invalidate the checksum.
fn is_unchanged_generated(bytes: &[u8]) -> bool {
    let Some(split) = bytes.iter().position(|b| *b == b'\n') else { return false };
    let Ok(line) = std::str::from_utf8(&bytes[..split]) else { return false };
    let Some(hash) = line.strip_prefix(MARKER).and_then(|line| line.strip_suffix(" -->")) else { return false };
    hash.len() == 16 && u64::from_str_radix(hash, 16).is_ok_and(|hash| hash == checksum(&bytes[split + 1..]))
}

fn conflict() -> io::Error {
    io::Error::new(io::ErrorKind::AlreadyExists, "daily note path contains a user file or a manually edited export")
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT includes junctions.
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

fn ensure_directory(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path)?;
            if metadata.is_dir() && !is_link(&metadata) { Ok(()) } else { Err(conflict()) }
        }
        Err(e) => Err(e),
    }
}

fn read_regular(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !is_link(&metadata) => fs::read(path).map(Some),
        Ok(_) => Err(conflict()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn write_generated(path: &Path, desired: &[u8]) -> io::Result<()> {
    let previous = read_regular(path)?;
    if previous.as_deref() == Some(desired) {
        return Ok(());
    }
    if previous.as_ref().is_some_and(|bytes| !is_unchanged_generated(bytes)) {
        return Err(conflict());
    }
    write_observed(path, desired, previous)
}

fn write_observed(path: &Path, desired: &[u8], previous: Option<Vec<u8>>) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let (temp, mut file) = loop {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".livelisten-{}-{id}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        file.write_all(desired)?;
        file.sync_all()?;
        drop(file);
        // Refuse an edit made while the replacement was being prepared.
        if read_regular(path)? != previous {
            return Err(conflict());
        }
        if previous.is_some() {
            fs::rename(&temp, path)
        } else {
            // Atomic creation without replacement also protects a concurrently created user file.
            fs::hard_link(&temp, path)
        }
    })();
    let _ = fs::remove_file(&temp);
    result
}

fn date_filename(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 13
        && &bytes[10..] == b".md"
        && bytes[..10]
            .iter()
            .enumerate()
            .all(|(index, c)| if index == 4 || index == 7 { *c == b'-' } else { c.is_ascii_digit() })
}

pub(super) fn sync(book: &CardBook, config_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(config_dir)?;
    let root = config_dir.join("随手记");
    ensure_directory(&root)?;
    for folder in FOLDERS {
        ensure_directory(&root.join(folder))?;
    }
    // An invalid manifest is a conflict, never permission to overwrite or clean anything.
    let (protection, _) = read_protection(&root)?;

    let mut cards: Vec<_> = book.cards.iter().collect();
    cards.sort_by_key(|card| (card.created_ms, card.id));
    let mut documents: BTreeMap<PathBuf, String> = BTreeMap::new();
    for card in cards {
        let stamp = local_stamp(card.created_ms);
        let day = &stamp[..10];
        let folder = status(card);
        let path = root.join(folder).join(format!("{day}.md"));
        let body = documents.entry(path).or_insert_with(|| format!(
            "# {day} · {folder}\n\n> 由实时聆听自动生成，请在应用中编辑。手动修改此文件会暂停覆盖；完整格式保存在 cards.json。\n\n"
        ));
        let kind = match card.kind {
            CardKind::Note => "随手记",
            CardKind::Todo => "待办",
        };
        let check = if card.done { "x" } else { " " };
        let text = card.text.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\n  ");
        body.push_str(&format!("## {} · {kind} · #{}\n\n- [{check}] {text}\n\n", &stamp[11..], card.id));
    }
    // Finish new destinations first; a failed move must not remove the previous readable copy.
    for (path, body) in &documents {
        if read_regular(path)?.is_some_and(|bytes| protection.contains(&root, path, &bytes)) {
            return Err(conflict());
        }
        write_generated(path, &generated(body))?;
    }
    for folder in FOLDERS {
        for entry in fs::read_dir(root.join(folder))? {
            let entry = entry?;
            let path = entry.path();
            if documents.contains_key(&path) || !entry.file_name().to_str().is_some_and(date_filename) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file() || is_link(&metadata) {
                continue;
            }
            let bytes = fs::read(&path)?;
            if !protection.contains(&root, &path, &bytes)
                && is_unchanged_generated(&bytes)
                && read_regular(&path)?.as_deref() == Some(bytes.as_slice())
            {
                fs::remove_file(&path)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            loop {
                let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!("ll-markdown-{}-{id}", std::process::id()));
                match fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(e) => panic!("test directory: {e}"),
                }
            }
        }

        fn daily(&self, folder: &str, ms: u64) -> PathBuf {
            self.0.join("随手记").join(folder).join(format!("{}.md", &local_stamp(ms)[..10]))
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn daily_files_group_types_preserve_unicode_and_do_not_rewrite_unchanged_files() {
        let dir = TestDir::new();
        let mut book = CardBook::default();
        let ms = 1_794_096_000_000;
        let id = book.add(CardKind::Note, "你好 📝\n第二行", "t".into(), ms);
        book.add(CardKind::Todo, "买牛奶", "t".into(), ms + 1000);
        sync(&book, &dir.0).unwrap();
        let path = dir.daily("进行中", ms);
        let bytes = fs::read(&path).unwrap();
        assert!(is_unchanged_generated(&bytes));
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains(&format!("随手记 · #{id}")) && text.contains("待办 · #2"));
        assert!(text.contains("- [ ] 你好 📝\n  第二行") && text.contains("- [ ] 买牛奶"));
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        sync(&book, &dir.0).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
        for folder in FOLDERS {
            assert!(dir.0.join("随手记").join(folder).is_dir());
        }
    }

    #[test]
    fn state_and_date_changes_move_entries_and_delete_removes_empty_generated_days() {
        let dir = TestDir::new();
        let mut book = CardBook::default();
        let ms = 1_794_096_000_000;
        let id = book.add(CardKind::Todo, "任务", "t".into(), ms);
        sync(&book, &dir.0).unwrap();
        book.update(id, &json!({"done": true})).unwrap();
        sync(&book, &dir.0).unwrap();
        assert!(!dir.daily("进行中", ms).exists());
        assert!(fs::read_to_string(dir.daily("已完成", ms)).unwrap().contains("- [x] 任务"));
        book.update(id, &json!({"archived": true})).unwrap();
        sync(&book, &dir.0).unwrap();
        assert!(!dir.daily("已完成", ms).exists());
        assert!(dir.daily("已归档", ms).exists());
        let next = ms + 86_400_000;
        book.update(id, &json!({"createdMs": next})).unwrap();
        sync(&book, &dir.0).unwrap();
        assert!(!dir.daily("已归档", ms).exists());
        assert!(dir.daily("已归档", next).exists());
        book.remove(id);
        sync(&book, &dir.0).unwrap();
        assert!(!dir.daily("已归档", next).exists());
    }

    #[test]
    fn user_files_and_manually_edited_exports_are_preserved() {
        let dir = TestDir::new();
        let mut book = CardBook::default();
        let ms = 1_794_096_000_000;
        let id = book.add(CardKind::Note, "原文", "t".into(), ms);
        sync(&CardBook::default(), &dir.0).unwrap();
        let path = dir.daily("进行中", ms);
        fs::write(&path, "用户自己的日记").unwrap();
        assert_eq!(sync(&book, &dir.0).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&path).unwrap(), "用户自己的日记");
        fs::remove_file(&path).unwrap();
        sync(&book, &dir.0).unwrap();
        let edited = format!("{}\n手动补充", fs::read_to_string(&path).unwrap());
        fs::write(&path, &edited).unwrap();
        assert_eq!(sync(&book, &dir.0).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);
        // The edited copy is also preserved when it is no longer a generated destination.
        book.update(id, &json!({"done": true})).unwrap();
        sync(&book, &dir.0).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), edited);
        assert!(dir.daily("已完成", ms).exists());
    }

    #[test]
    fn conflicting_destination_keeps_the_previous_generated_copy_until_retry() {
        let dir = TestDir::new();
        let mut book = CardBook::default();
        let ms = 1_794_096_000_000;
        let id = book.add(CardKind::Todo, "迁移", "t".into(), ms);
        sync(&book, &dir.0).unwrap();
        let before = fs::read(dir.daily("进行中", ms)).unwrap();
        let target = dir.daily("已完成", ms);
        fs::write(&target, "留住用户文件").unwrap();
        book.update(id, &json!({"done": true})).unwrap();
        assert!(sync(&book, &dir.0).is_err());
        assert_eq!(fs::read(dir.daily("进行中", ms)).unwrap(), before);
        assert_eq!(fs::read_to_string(&target).unwrap(), "留住用户文件");
        fs::remove_file(&target).unwrap();
        sync(&book, &dir.0).unwrap();
        assert!(!dir.daily("进行中", ms).exists());
        assert!(is_unchanged_generated(&fs::read(&target).unwrap()));
    }

    #[test]
    fn obstructed_folder_is_an_error_without_modifying_the_obstruction() {
        let dir = TestDir::new();
        let root = dir.0.join("随手记");
        fs::write(&root, "不能覆盖").unwrap();
        assert!(sync(&CardBook::default(), &dir.0).is_err());
        assert_eq!(fs::read_to_string(&root).unwrap(), "不能覆盖");
    }

    #[test]
    fn orphan_protection_preserves_old_days_and_rejects_same_day_replacement() {
        let dir = TestDir::new();
        let mut old = CardBook::default();
        let ms = 1_794_096_000_000;
        old.add(CardKind::Note, "主文件丢失后的唯一旧记录", "t".into(), ms);
        sync(&old, &dir.0).unwrap();
        let old_path = dir.daily("进行中", ms);
        let bytes = fs::read(&old_path).unwrap();
        protect_orphans(&dir.0).unwrap();
        let mut fresh = CardBook::default();
        let id = fresh.add(CardKind::Note, "新的卡片", "t".into(), ms);
        assert_eq!(sync(&fresh, &dir.0).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&old_path).unwrap(), bytes);
        fresh.update(id, &json!({"createdMs": ms + 86_400_000})).unwrap();
        sync(&fresh, &dir.0).unwrap();
        assert_eq!(fs::read(&old_path).unwrap(), bytes);
        assert!(dir.daily("进行中", ms + 86_400_000).exists());
        sync(&CardBook::default(), &dir.0).unwrap();
        assert_eq!(fs::read(&old_path).unwrap(), bytes);
        assert!(!dir.daily("进行中", ms + 86_400_000).exists());
    }

    #[test]
    fn invalid_or_unscoped_protection_manifest_never_authorizes_cleanup() {
        let dir = TestDir::new();
        let mut book = CardBook::default();
        let ms = 1_794_096_000_000;
        book.add(CardKind::Note, "保留", "t".into(), ms);
        sync(&book, &dir.0).unwrap();
        let before = fs::read(dir.daily("进行中", ms)).unwrap();
        for content in [
            "{broken".to_string(),
            json!({"format": PROTECTION_FORMAT, "files": {"../2026-11-08.md": "0000000000000000"}}).to_string(),
            json!({"format": PROTECTION_FORMAT, "files": {"进行中/2026-11-08.md/../other": "0000000000000000"}})
                .to_string(),
        ] {
            fs::write(dir.0.join("随手记").join(PROTECTION_FILE), content).unwrap();
            assert!(sync(&CardBook::default(), &dir.0).is_err());
            assert_eq!(fs::read(dir.daily("进行中", ms)).unwrap(), before);
        }
    }
}
