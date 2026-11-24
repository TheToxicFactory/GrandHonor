//! Reads the identity GrandHonor needs straight from the firmware's SMBIOS table (the raw
//! `RSMB` buffer of `GetSystemFirmwareTable`), so it no longer depends on the copy HONOR's
//! software writes to `HKLM\SOFTWARE\HONOR\BIOS` (which goes stale once that software is gone:
//! on ZQC-P it still said BIOS 1.07 after an update to 1.12).

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Firmware {
    /// Type 0: BIOS version, e.g. `1.12`.
    pub bios_version: Option<String>,
    /// Type 1: product (`ZQC-P`), SKU (`C233`), family (`HONOR MagicBook`).
    pub product: Option<String>,
    pub sku: Option<String>,
    pub family: Option<String>,
    /// Type 3 SKU, e.g. `ZhuqueC-7211T`; its part before `-` is HONOR's model code name.
    pub chassis_sku: Option<String>,
}

impl Firmware {
    /// HONOR's internal model code name (`ZhuqueC`), from the chassis SKU.
    pub fn code_name(&self) -> Option<String> {
        let sku = self.chassis_sku.as_deref()?;
        let name = sku.split('-').next()?.trim();
        (name.len() >= 3 && name.chars().all(|c| c.is_ascii_alphanumeric())).then(|| name.to_string())
    }
}

/// Parses an `RSMB` buffer: an 8-byte header, then SMBIOS structures (formatted area followed
/// by NUL-terminated strings and a double NUL). Unknown or malformed data ends parsing.
pub fn parse(raw: &[u8]) -> Firmware {
    let mut out = Firmware::default();
    if raw.len() < 8 {
        return out;
    }
    let length = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
    let table = &raw[8..raw.len().min(8 + length)];
    let mut i = 0;
    while i + 4 <= table.len() {
        let kind = table[i];
        let formatted = table[i + 1] as usize;
        if formatted < 4 || i + formatted > table.len() {
            break;
        }
        let area = &table[i..i + formatted];
        // Strings: NUL-terminated, the set ends with an extra NUL.
        let mut strings: Vec<String> = Vec::new();
        let mut j = i + formatted;
        if table.get(j) == Some(&0) && table.get(j + 1) == Some(&0) {
            j += 2;
        } else {
            while j < table.len() {
                let end = match table[j..].iter().position(|&b| b == 0) {
                    Some(p) => j + p,
                    None => table.len(),
                };
                strings.push(String::from_utf8_lossy(&table[j..end]).trim().to_string());
                j = end + 1;
                if table.get(j) == Some(&0) {
                    j += 1;
                    break;
                }
            }
        }
        let text = |offset: usize| -> Option<String> {
            let index = *area.get(offset)? as usize;
            let s = strings.get(index.checked_sub(1)?)?;
            let bad = s.is_empty()
                || s.eq_ignore_ascii_case("N/A")
                || s.eq_ignore_ascii_case("NULL")
                || s.eq_ignore_ascii_case("To be filled by O.E.M.")
                || s.eq_ignore_ascii_case("Default string");
            (!bad).then(|| s.clone())
        };
        match kind {
            0 => out.bios_version = out.bios_version.take().or_else(|| text(0x05)),
            1 => {
                out.product = out.product.take().or_else(|| text(0x05));
                out.sku = out.sku.take().or_else(|| text(0x19));
                out.family = out.family.take().or_else(|| text(0x1A));
            }
            3 => {
                // The SKU string follows the contained-element records (SMBIOS 2.7+).
                let count = *area.get(0x13).unwrap_or(&0) as usize;
                let size = *area.get(0x14).unwrap_or(&0) as usize;
                out.chassis_sku = out.chassis_sku.take().or_else(|| text(0x15 + count * size));
            }
            127 => break,
            _ => {}
        }
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn structure(kind: u8, area: &[u8], strings: &[&str]) -> Vec<u8> {
        let mut v = vec![kind, (area.len() + 4) as u8, 0, 0];
        v.extend_from_slice(area);
        if strings.is_empty() {
            v.extend_from_slice(&[0, 0]);
        } else {
            for s in strings {
                v.extend_from_slice(s.as_bytes());
                v.push(0);
            }
            v.push(0);
        }
        v
    }

    fn rsmb(table: &[u8]) -> Vec<u8> {
        let mut raw = vec![0, 3, 4, 0];
        raw.extend_from_slice(&(table.len() as u32).to_le_bytes());
        raw.extend_from_slice(table);
        raw
    }

    #[test]
    fn reads_the_zqc_p_identity() {
        let mut table = Vec::new();
        // Type 0: vendor=1, version=2 (offset 0x05), date=3 (0x08).
        let mut t0 = vec![0u8; 0x14];
        t0[0x04 - 4] = 1;
        t0[0x05 - 4] = 2;
        t0[0x08 - 4] = 3;
        table.extend(structure(0, &t0, &["HONOR", "1.12", "09/30/2026"]));
        // Type 1: manufacturer 1, product 2, version 3, serial 4, SKU 5, family 6.
        let mut t1 = vec![0u8; 0x1B - 4];
        t1[0x04 - 4] = 1;
        t1[0x05 - 4] = 2;
        t1[0x06 - 4] = 3;
        t1[0x07 - 4] = 4;
        t1[0x19 - 4] = 5;
        t1[0x1A - 4] = 6;
        table.extend(structure(1, &t1, &["HONOR", "ZQC-P", "M1230", "SERIAL", "C233", "HONOR MagicBook"]));
        // Type 3 with no contained elements: SKU string at 0x15.
        let mut t3 = vec![0u8; 0x16 - 4];
        t3[0x04 - 4] = 1;
        t3[0x15 - 4] = 4;
        table.extend(structure(3, &t3, &["HONOR", "M1230", "SERIAL", "ZhuqueC-7211T"]));
        table.extend(structure(127, &[], &[]));
        let fw = parse(&rsmb(&table));
        assert_eq!(fw.bios_version.as_deref(), Some("1.12"));
        assert_eq!(fw.product.as_deref(), Some("ZQC-P"));
        assert_eq!(fw.sku.as_deref(), Some("C233"));
        assert_eq!(fw.family.as_deref(), Some("HONOR MagicBook"));
        assert_eq!(fw.code_name().as_deref(), Some("ZhuqueC"));
    }

    #[test]
    fn placeholders_and_garbage_give_nothing() {
        let mut t1 = vec![0u8; 0x1B - 4];
        t1[0x05 - 4] = 1;
        t1[0x19 - 4] = 9; // no such string
        let table = structure(1, &t1, &["N/A"]);
        let fw = parse(&rsmb(&table));
        assert_eq!(fw, Firmware::default());
        assert_eq!(parse(&[1, 2, 3]), Firmware::default());
        assert_eq!(parse(&rsmb(&[1, 200, 0, 0])), Firmware::default());
    }
}
