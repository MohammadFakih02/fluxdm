//! Stable Windows-safe torrent paths shared with clients.
use std::path::PathBuf;

pub fn component(name: &str) -> String {
    let mut out: String = name.chars().map(|c| {
        if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }
    }).collect();
    out = out.trim_end_matches(['.', ' ']).to_owned();
    if out.is_empty() { out.push('_'); }
    let stem = out.split('.').next().unwrap_or("").trim_end().to_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ["COM", "LPT"].iter().any(|p| stem.strip_prefix(p).is_some_and(|n|
            matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "\u{b9}" | "\u{b2}" | "\u{b3}")));
    if reserved { out.insert(0, '_'); }
    let mut units = 0;
    out = out.chars().take_while(|c| { units += c.len_utf16(); units <= 255 }).collect();
    out.truncate(out.trim_end_matches(['.', ' ']).len());
    out
}

pub fn path(parts: impl IntoIterator<Item = impl AsRef<str>>) -> PathBuf {
    parts.into_iter().map(|p| component(p.as_ref())).collect()
}

/// Makes every path unique the way Windows compares names (ignoring case): a later file whose cleaned name
/// matches an earlier one ("a:b" and "a_b", or "Readme" and "README") gets " (2)", " (3)"… before its
/// extension. Files keep their order, so everyone who applies this to the same list gets the same names.
pub fn unique(paths: &mut [PathBuf]) {
    let mut seen = std::collections::HashSet::new();
    for p in paths.iter_mut() {
        if seen.insert(p.to_string_lossy().to_lowercase()) { continue; }
        let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        for n in 2.. {
            let candidate = p.with_file_name(format!("{stem} ({n}){ext}"));
            if seen.insert(candidate.to_string_lossy().to_lowercase()) {
                *p = candidate;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clashing_names_get_a_number() {
        let mut v: Vec<PathBuf> = ["d/a_b.txt", "d/a:b.txt", "d/README", "d/readme", "d/a_b (2).txt"].iter().map(|s| path(s.split('/'))).collect();
        unique(&mut v);
        let s: Vec<String> = v.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(s, ["d/a_b.txt", "d/a_b (2).txt", "d/README", "d/readme (2)", "d/a_b (2) (2).txt"]);
    }
}
