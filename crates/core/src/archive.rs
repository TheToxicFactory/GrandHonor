use crc32fast::Hasher;
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

pub const MAX_EXTRACTED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_ARCHIVE_ENTRIES: usize = 20_000;
pub const MAX_NESTED_DEPTH: usize = 4;

#[derive(Default)]
pub struct ArchiveBudget {
    entries: usize,
    bytes_written: u64,
}

impl ArchiveBudget {
    pub fn extract(&mut self, archive_path: &Path, destination: &Path) -> io::Result<()> {
        fs::create_dir_all(destination)?;
        let canonical_root = fs::canonicalize(destination)?;
        let mut file = BufReader::new(File::open(archive_path)?);
        let file_len = file.seek(SeekFrom::End(0))?;
        let entries = read_central_directory(&mut file, file_len)?;

        for entry in entries {
            self.entries += 1;
            if self.entries > MAX_ARCHIVE_ENTRIES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "The driver archive exceeds entry limits.",
                ));
            }
            if entry.uncompressed_size > MAX_EXTRACTED_BYTES.saturating_sub(self.bytes_written) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "The driver archive exceeds extraction limits.",
                ));
            }

            let relative = sanitize_relative_path(&entry.name)?;
            if entry.is_symlink {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "The driver archive contains a symbolic link.",
                ));
            }
            if entry.is_dir {
                let target_dir = destination.join(&relative);
                ensure_under_root(destination, &canonical_root, &target_dir)?;
                fs::create_dir_all(&target_dir)?;
                continue;
            }

            let target = destination.join(&relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
                let canonical_parent = fs::canonicalize(parent)?;
                if !canonical_parent.starts_with(&canonical_root) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "The driver archive contains an unsafe path.",
                    ));
                }
            }

            extract_entry(&mut file, &entry, &target, &mut self.bytes_written)?;
        }
        Ok(())
    }
}

pub fn create_zip_from_directory(source_dir: &Path, dest_zip: &Path) -> io::Result<()> {
    let mut files = Vec::new();
    collect_files(source_dir, source_dir, &mut files)?;

    let out_file = File::create(dest_zip)?;
    let mut writer = BufWriter::new(out_file);
    let mut central_records = Vec::new();
    let mut offset: u64 = 0;

    for (rel_name, full_path) in files {
        let name_bytes = rel_name.as_bytes();
        let name_len = u16::try_from(name_bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Archive path too long"))?;

        let mut src = File::open(&full_path)?;
        let mut raw = Vec::new();
        src.read_to_end(&mut raw)?;
        let mut crc_hasher = Hasher::new();
        crc_hasher.update(&raw);
        let crc = crc_hasher.finalize();

        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw)?;
        let compressed = encoder.finish()?;

        let (method, payload) = if compressed.len() < raw.len() {
            (8u16, compressed)
        } else {
            (0u16, raw.clone())
        };

        let comp_size = u32::try_from(payload.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Entry too large"))?;
        let uncomp_size = u32::try_from(raw.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Entry too large"))?;
        let local_offset = u32::try_from(offset)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Archive exceeds 4 GiB"))?;

        // Local file header
        writer.write_all(&0x0403_4B50u32.to_le_bytes())?;
        writer.write_all(&20u16.to_le_bytes())?; // version needed
        writer.write_all(&0x0800u16.to_le_bytes())?; // UTF-8 flag
        writer.write_all(&method.to_le_bytes())?;
        writer.write_all(&0u16.to_le_bytes())?; // mod time
        writer.write_all(&0u16.to_le_bytes())?; // mod date
        writer.write_all(&crc.to_le_bytes())?;
        writer.write_all(&comp_size.to_le_bytes())?;
        writer.write_all(&uncomp_size.to_le_bytes())?;
        writer.write_all(&name_len.to_le_bytes())?;
        writer.write_all(&0u16.to_le_bytes())?; // extra len
        writer.write_all(name_bytes)?;
        writer.write_all(&payload)?;

        offset += 30 + name_bytes.len() as u64 + payload.len() as u64;

        central_records.push(CentralRecord {
            name: rel_name,
            method,
            crc,
            comp_size,
            uncomp_size,
            local_offset,
        });
    }

    let cd_offset = u32::try_from(offset)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Archive exceeds 4 GiB"))?;
    let mut cd_size: u64 = 0;

    for rec in &central_records {
        let name_bytes = rec.name.as_bytes();
        let name_len = name_bytes.len() as u16;
        writer.write_all(&0x0201_4B50u32.to_le_bytes())?;
        writer.write_all(&20u16.to_le_bytes())?; // version made by
        writer.write_all(&20u16.to_le_bytes())?; // version needed
        writer.write_all(&0x0800u16.to_le_bytes())?; // UTF-8
        writer.write_all(&rec.method.to_le_bytes())?;
        writer.write_all(&0u16.to_le_bytes())?;
        writer.write_all(&0u16.to_le_bytes())?;
        writer.write_all(&rec.crc.to_le_bytes())?;
        writer.write_all(&rec.comp_size.to_le_bytes())?;
        writer.write_all(&rec.uncomp_size.to_le_bytes())?;
        writer.write_all(&name_len.to_le_bytes())?;
        writer.write_all(&0u16.to_le_bytes())?;
        writer.write_all(&0u16.to_le_bytes())?;
        writer.write_all(&0u16.to_le_bytes())?;
        writer.write_all(&0u16.to_le_bytes())?;
        writer.write_all(&0u32.to_le_bytes())?;
        writer.write_all(&rec.local_offset.to_le_bytes())?;
        writer.write_all(name_bytes)?;
        cd_size += 46 + name_bytes.len() as u64;
    }

    let total_entries = u16::try_from(central_records.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Too many archive entries"))?;
    let cd_size_u32 = u32::try_from(cd_size)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Central directory too large"))?;

    writer.write_all(&0x0605_4B50u32.to_le_bytes())?;
    writer.write_all(&0u16.to_le_bytes())?;
    writer.write_all(&0u16.to_le_bytes())?;
    writer.write_all(&total_entries.to_le_bytes())?;
    writer.write_all(&total_entries.to_le_bytes())?;
    writer.write_all(&cd_size_u32.to_le_bytes())?;
    writer.write_all(&cd_offset.to_le_bytes())?;
    writer.write_all(&0u16.to_le_bytes())?;
    writer.flush()?;
    Ok(())
}

struct CentralRecord {
    name: String,
    method: u16,
    crc: u32,
    comp_size: u32,
    uncomp_size: u32,
    local_offset: u32,
}

struct ZipEntryMeta {
    name: String,
    method: u16,
    crc: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    local_header_offset: u64,
    is_dir: bool,
    is_symlink: bool,
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect_files(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(io::Error::other)?
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
    Ok(())
}

fn sanitize_relative_path(raw: &str) -> io::Result<PathBuf> {
    if raw.contains('\0') || raw.contains(':') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The driver archive contains an unsafe path.",
        ));
    }
    let normalized = raw.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The driver archive contains an absolute path.",
        ));
    }
    let mut result = PathBuf::new();
    for component in Path::new(&normalized).components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "The driver archive contains an unsafe path.",
                ));
            }
        }
    }
    Ok(result)
}

fn ensure_under_root(base: &Path, canonical_root: &Path, target: &Path) -> io::Result<()> {
    let mut probe = target.to_path_buf();
    while !probe.exists() {
        if !probe.pop() || probe == base {
            return Ok(());
        }
    }
    if !fs::canonicalize(&probe)?.starts_with(canonical_root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The driver archive points outside the destination directory.",
        ));
    }
    Ok(())
}

fn read_u16_le(buf: &[u8]) -> u16 {
    u16::from_le_bytes([buf[0], buf[1]])
}

fn read_u32_le(buf: &[u8]) -> u32 {
    u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])
}

fn read_u64_le(buf: &[u8]) -> u64 {
    u64::from_le_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ])
}

fn read_central_directory<R: Read + Seek>(
    reader: &mut R,
    file_len: u64,
) -> io::Result<Vec<ZipEntryMeta>> {
    let tail_len = file_len.min(65_557) as usize;
    reader.seek(SeekFrom::Start(file_len - tail_len as u64))?;
    let mut tail = vec![0u8; tail_len];
    reader.read_exact(&mut tail)?;

    let eocd_pos = (0..=tail_len.saturating_sub(22))
        .rev()
        .find(|&i| tail[i..i + 4] == [0x50, 0x4B, 0x05, 0x06])
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid ZIP archive"))?;

    let eocd = &tail[eocd_pos..];
    let total_entries_16 = read_u16_le(&eocd[10..12]) as u64;
    let cd_size_32 = read_u32_le(&eocd[12..16]) as u64;
    let cd_offset_32 = read_u32_le(&eocd[16..20]) as u64;

    let (cd_offset, cd_size, total_entries) =
        if cd_offset_32 == 0xFFFF_FFFF || total_entries_16 == 0xFFFF {
            read_zip64_locator(reader, file_len - tail_len as u64 + eocd_pos as u64)?
        } else {
            (cd_offset_32, cd_size_32, total_entries_16)
        };

    if cd_offset + cd_size > file_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Corrupt ZIP central directory",
        ));
    }

    reader.seek(SeekFrom::Start(cd_offset))?;
    let mut cd_buf = vec![0u8; cd_size as usize];
    reader.read_exact(&mut cd_buf)?;

    let mut entries = Vec::with_capacity(total_entries.min(MAX_ARCHIVE_ENTRIES as u64) as usize);
    let mut pos = 0usize;
    for _ in 0..total_entries {
        if pos + 46 > cd_buf.len() || cd_buf[pos..pos + 4] != [0x50, 0x4B, 0x01, 0x02] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid ZIP central directory header",
            ));
        }
        let version_made = read_u16_le(&cd_buf[pos + 4..pos + 6]);
        let method = read_u16_le(&cd_buf[pos + 10..pos + 12]);
        let crc = read_u32_le(&cd_buf[pos + 16..pos + 20]);
        let mut comp_size = read_u32_le(&cd_buf[pos + 20..pos + 24]) as u64;
        let mut uncomp_size = read_u32_le(&cd_buf[pos + 24..pos + 28]) as u64;
        let name_len = read_u16_le(&cd_buf[pos + 28..pos + 30]) as usize;
        let extra_len = read_u16_le(&cd_buf[pos + 30..pos + 32]) as usize;
        let comment_len = read_u16_le(&cd_buf[pos + 32..pos + 34]) as usize;
        let ext_attr = read_u32_le(&cd_buf[pos + 38..pos + 42]);
        let mut local_offset = read_u32_le(&cd_buf[pos + 42..pos + 46]) as u64;

        let rec_end = pos + 46 + name_len + extra_len + comment_len;
        if rec_end > cd_buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Truncated ZIP central directory header",
            ));
        }

        let name = String::from_utf8_lossy(&cd_buf[pos + 46..pos + 46 + name_len]).into_owned();
        let extra = &cd_buf[pos + 46 + name_len..pos + 46 + name_len + extra_len];
        apply_zip64_extra(extra, &mut uncomp_size, &mut comp_size, &mut local_offset);

        let unix_mode = (ext_attr >> 16) & 0xF000;
        let is_symlink = (version_made >> 8) == 3 && unix_mode == 0xA000;
        let is_dir = name.ends_with('/') || name.ends_with('\\');

        entries.push(ZipEntryMeta {
            name,
            method,
            crc,
            compressed_size: comp_size,
            uncompressed_size: uncomp_size,
            local_header_offset: local_offset,
            is_dir,
            is_symlink,
        });
        pos = rec_end;
    }

    Ok(entries)
}

fn read_zip64_locator<R: Read + Seek>(
    reader: &mut R,
    eocd_abs_offset: u64,
) -> io::Result<(u64, u64, u64)> {
    if eocd_abs_offset < 20 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Missing Zip64 locator",
        ));
    }
    reader.seek(SeekFrom::Start(eocd_abs_offset - 20))?;
    let mut loc = [0u8; 20];
    reader.read_exact(&mut loc)?;
    if loc[0..4] != [0x50, 0x4B, 0x06, 0x07] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid Zip64 locator",
        ));
    }
    let eocd64_offset = read_u64_le(&loc[8..16]);
    reader.seek(SeekFrom::Start(eocd64_offset))?;
    let mut eocd64 = [0u8; 56];
    reader.read_exact(&mut eocd64)?;
    if eocd64[0..4] != [0x50, 0x4B, 0x06, 0x06] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid Zip64 EOCD",
        ));
    }
    let total_entries = read_u64_le(&eocd64[32..40]);
    let cd_size = read_u64_le(&eocd64[40..48]);
    let cd_offset = read_u64_le(&eocd64[48..56]);
    Ok((cd_offset, cd_size, total_entries))
}

fn apply_zip64_extra(
    mut extra: &[u8],
    uncomp_size: &mut u64,
    comp_size: &mut u64,
    local_offset: &mut u64,
) {
    while extra.len() >= 4 {
        let id = read_u16_le(&extra[0..2]);
        let size = read_u16_le(&extra[2..4]) as usize;
        extra = &extra[4..];
        if extra.len() < size {
            break;
        }
        if id == 0x0001 {
            let mut field = &extra[..size];
            if *uncomp_size == 0xFFFF_FFFF && field.len() >= 8 {
                *uncomp_size = read_u64_le(&field[..8]);
                field = &field[8..];
            }
            if *comp_size == 0xFFFF_FFFF && field.len() >= 8 {
                *comp_size = read_u64_le(&field[..8]);
                field = &field[8..];
            }
            if *local_offset == 0xFFFF_FFFF && field.len() >= 8 {
                *local_offset = read_u64_le(&field[..8]);
            }
            break;
        }
        extra = &extra[size..];
    }
}

fn extract_entry<R: Read + Seek>(
    reader: &mut R,
    entry: &ZipEntryMeta,
    target: &Path,
    bytes_written: &mut u64,
) -> io::Result<()> {
    reader.seek(SeekFrom::Start(entry.local_header_offset))?;
    let mut lfh = [0u8; 30];
    reader.read_exact(&mut lfh)?;
    if lfh[0..4] != [0x50, 0x4B, 0x03, 0x04] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid ZIP local file header",
        ));
    }
    let name_len = read_u16_le(&lfh[26..28]) as i64;
    let extra_len = read_u16_le(&lfh[28..30]) as i64;
    reader.seek(SeekFrom::Current(name_len + extra_len))?;

    let limited = reader.take(entry.compressed_size);
    let mut out = File::options().write(true).create_new(true).open(target)?;
    let mut crc = Hasher::new();
    let mut buf = [0u8; 65_536];

    let mut copy_checked = |src: &mut dyn Read| -> io::Result<()> {
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            *bytes_written = bytes_written
                .checked_add(n as u64)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Archive overflow"))?;
            if *bytes_written > MAX_EXTRACTED_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "The driver archive exceeds extraction limits.",
                ));
            }
            crc.update(&buf[..n]);
            out.write_all(&buf[..n])?;
        }
        Ok(())
    };

    match entry.method {
        0 => {
            let mut src = limited;
            copy_checked(&mut src)?;
        }
        8 => {
            let mut decoder = DeflateDecoder::new(limited);
            copy_checked(&mut decoder)?;
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("Unsupported ZIP compression method {}", entry.method),
            ));
        }
    }
    out.flush()?;
    if crc.finalize() != entry.crc {
        let _ = fs::remove_file(target);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "CRC mismatch in driver archive",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_zip_and_reject_traversal() {
        let temp = std::env::temp_dir().join(format!("gh-zip-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);
        let src = temp.join("src");
        let dst = temp.join("dst");
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::write(src.join("a.inf"), b"[Version]\nSignature=\"$WINDOWS NT$\"\n").unwrap();
        fs::write(src.join("nested/b.sys"), vec![42u8; 4096]).unwrap();

        let archive = temp.join("drivers.zip");
        create_zip_from_directory(&src, &archive).unwrap();

        let mut budget = ArchiveBudget::default();
        budget.extract(&archive, &dst).unwrap();
        assert_eq!(
            fs::read(dst.join("a.inf")).unwrap(),
            b"[Version]\nSignature=\"$WINDOWS NT$\"\n"
        );
        assert_eq!(fs::read(dst.join("nested/b.sys")).unwrap(), vec![42u8; 4096]);

        assert!(sanitize_relative_path("../evil.exe").is_err());
        assert!(sanitize_relative_path("C:\\Windows\\evil.exe").is_err());
        assert!(sanitize_relative_path("/abs/evil.exe").is_err());
        let _ = fs::remove_dir_all(&temp);
    }
}
