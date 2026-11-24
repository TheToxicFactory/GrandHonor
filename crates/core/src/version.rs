use std::cmp::Ordering;

pub fn version_parts(value: &str) -> Vec<u32> {
    let mut parts = Vec::new();
    let mut current: Option<u32> = None;
    for ch in value.chars() {
        if let Some(digit) = ch.to_digit(10) {
            let acc = current.unwrap_or(0);
            current = Some(acc.saturating_mul(10).saturating_add(digit));
        } else if let Some(acc) = current.take() {
            parts.push(acc);
        }
    }
    if let Some(acc) = current {
        parts.push(acc);
    }
    parts
}

pub fn compare_version_parts(left: &[u32], right: &[u32]) -> Ordering {
    let len = left.len().max(right.len());
    for i in 0..len {
        let a = left.get(i).copied().unwrap_or(0);
        let b = right.get(i).copied().unwrap_or(0);
        match a.cmp(&b) {
            Ordering::Equal => continue,
            non_eq => return non_eq,
        }
    }
    Ordering::Equal
}

pub fn compare_versions(left: &str, right: &str) -> Ordering {
    let a = version_parts(left);
    let b = version_parts(right);
    if !a.is_empty() && !b.is_empty() {
        compare_version_parts(&a, &b)
    } else {
        left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase())
    }
}

pub fn is_date_code(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 8 && bytes.starts_with(b"20") && bytes.iter().all(|b| b.is_ascii_digit())
}

pub fn is_comparable_version(available: &str, installed: &str) -> bool {
    let avail = available.trim();
    let inst = installed.trim();
    if avail.is_empty() || inst.is_empty() || inst == "0" {
        return false;
    }
    if is_date_code(avail) != is_date_code(inst) {
        return false;
    }
    let ap = version_parts(avail);
    let ip = version_parts(inst);
    if ap.is_empty() || ip.is_empty() {
        return false;
    }
    ap.len().abs_diff(ip.len()) <= 1
}

pub fn is_newer_version(available: &str, installed: &str) -> bool {
    is_comparable_version(available, installed)
        && compare_versions(available, installed) == Ordering::Greater
}

/// Extracts the most detailed dotted version (e.g. `10.1.56.27`), else the
/// last >=6-digit numeric token, else trimmed input.
pub fn normalize_support_version(value: &str) -> String {
    let mut best_dotted: Option<(usize, &str)> = None;
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut dots = 0usize;
            while i < bytes.len() {
                if bytes[i].is_ascii_digit() {
                    i += 1;
                } else if bytes[i] == b'.'
                    && i + 1 < bytes.len()
                    && bytes[i + 1].is_ascii_digit()
                {
                    dots += 1;
                    i += 1;
                } else {
                    break;
                }
            }
            if dots > 0 {
                let slice = &value[start..i];
                if best_dotted.is_none_or(|(best_dots, _)| dots > best_dots) {
                    best_dotted = Some((dots, slice));
                }
            }
        } else {
            i += 1;
        }
    }
    if let Some((_, dotted)) = best_dotted {
        return dotted.to_string();
    }

    let mut last_long_num: Option<&str> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i - start >= 6 {
                last_long_num = Some(&value[start..i]);
            }
        } else {
            i += 1;
        }
    }
    last_long_num
        .map(str::to_string)
        .unwrap_or_else(|| value.trim().to_string())
}

pub fn normalize_support_title(title: &str, version: &str) -> String {
    let mut s = title.trim();
    loop {
        let before = s;
        if !version.is_empty() {
            let lower_s = s.to_ascii_lowercase();
            let lower_v = version.to_ascii_lowercase();
            if let Some(pos) = lower_s.rfind(&lower_v)
                && lower_s[pos + lower_v.len()..].trim().is_empty()
            {
                s = s[..pos].trim_end_matches(|c: char| c.is_whitespace() || c == '_');
            }
        }
        let lower_s = s.to_ascii_lowercase();
        if let Some(pos) = lower_s.rfind("firmware")
            && lower_s[pos + "firmware".len()..].trim().is_empty()
        {
            s = s[..pos].trim_end_matches(|c: char| c.is_whitespace() || c == '_');
        }
        if s == before {
            break;
        }
    }
    s.trim_matches(|c: char| c == ' ' || c == '_').to_string()
}

pub fn normalize_release_date(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let bytes = raw.as_bytes();
    if bytes.len() >= 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && (bytes[4] == b'-' || bytes[4] == b'/')
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && (bytes[7] == b'-' || bytes[7] == b'/')
        && bytes[8..10].iter().all(u8::is_ascii_digit)
    {
        return Some(format!(
            "{}-{}-{}",
            &raw[0..4],
            &raw[5..7],
            &raw[8..10]
        ));
    }
    if bytes.len() == 8 && bytes.iter().all(u8::is_ascii_digit) {
        return Some(format!("{}-{}-{}", &raw[0..4], &raw[4..6], &raw[6..8]));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparability_and_ordering() {
        assert!(!is_comparable_version("20260619", "10.18.26100.3"));
        assert!(!is_comparable_version("2.5", "10.1.19627.8492"));
        assert!(!is_comparable_version("1.2.3", "0"));
        assert!(is_comparable_version("10.1.56.28", "10.1.56.27"));
        assert!(is_newer_version("10.1.56.28", "10.1.56.27"));
        assert!(!is_newer_version("10.1.56.27", "10.1.56.27"));
    }

    #[test]
    fn support_normalization() {
        assert_eq!(
            normalize_support_version("Chipset 10.1.56.27 for Win11 23.2"),
            "10.1.56.27"
        );
        assert_eq!(normalize_support_version("Build 20260619"), "20260619");
        assert_eq!(
            normalize_support_title("BIOS Firmware 1.10", "1.10"),
            "BIOS"
        );
        assert_eq!(
            normalize_release_date(Some("2026/08/15 10:20:30")),
            Some("2026-08-15".to_string())
        );
    }
}
