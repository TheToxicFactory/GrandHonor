//! File writes into the user's profile from an elevated process. `%APPDATA%\GrandHonor` and
//! `%LOCALAPPDATA%\GrandHonor` belong to the user, so a user-level program could replace them
//! (or a file in them) with a junction or symbolic link to a system location and make the
//! elevated host write there. Every write here therefore refuses a folder chain that contains a
//! reparse point, removes a link that stands where the file should be, and opens files with
//! `FILE_FLAG_OPEN_REPARSE_POINT`, so no link is ever followed.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

fn is_reparse_point(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// Creates `dir` if needed and checks that neither it nor any folder above it is a link.
pub fn prepare_dir(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    if dir.ancestors().any(is_reparse_point) {
        return Err(format!("{}: {}", dir.display(), grandhonor_core::locale::t("это ссылка, GrandHonor не пишет через ссылки", "is a link; GrandHonor does not write through links", "是链接，GrandHonor 不通过链接写入")));
    }
    Ok(())
}

/// Removes a link (not its target) that stands at `path`.
fn remove_link(path: &Path) -> Result<(), String> {
    if is_reparse_point(path) {
        fs::remove_file(path).or_else(|_| fs::remove_dir(path)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn open_no_follow(path: &Path, append: bool) -> Result<fs::File, String> {
    let mut options = OpenOptions::new();
    options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    if append {
        options.create(true).append(true);
    } else {
        options.create_new(true).write(true);
    }
    options.open(path).map_err(|e| e.to_string())
}

/// Writes the whole file through a temporary file and a rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or(grandhonor_core::locale::t("Нет папки", "No folder", "没有文件夹"))?;
    prepare_dir(dir)?;
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("")
    ));
    if fs::symlink_metadata(&tmp).is_ok() {
        fs::remove_file(&tmp).map_err(|e| e.to_string())?;
    }
    let mut file = open_no_follow(&tmp, false)?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    drop(file);
    remove_link(path)?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Appends to a file, creating it when missing.
pub fn append(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or(grandhonor_core::locale::t("Нет папки", "No folder", "没有文件夹"))?;
    prepare_dir(dir)?;
    remove_link(path)?;
    open_no_follow(path, true)?.write_all(bytes).map_err(|e| e.to_string())
}

/// Renames within a folder already checked by `prepare_dir`, replacing a link at the target.
pub fn rename(from: &Path, to: &Path) -> Result<(), String> {
    if is_reparse_point(from) {
        return Err(format!("{} is a link", from.display()));
    }
    remove_link(to)?;
    fs::rename(from, to).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_appends_and_replaces_atomically() {
        let dir = std::env::temp_dir().join(format!("gh-safe-fs-{}", std::process::id()));
        let file = dir.join("a.json");
        write_atomic(&file, b"one").unwrap();
        write_atomic(&file, b"two").unwrap();
        append(&file, b"+").unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"two+");
        assert!(!dir.join("a.json.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
