//! Default file names, UTC time stamps and part names for recordings.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Formats Unix seconds as `YYYY-MM-DD_HH-MM-SS` in UTC (local time would need
/// tz data we don't have).
pub fn utc_stamp(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}_{:02}-{:02}-{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian
/// (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// `dir/<UTC stamp>.wav`, or `-2`, `-3`, ... appended if that file exists.
pub fn default_path(dir: &Path, now: SystemTime) -> PathBuf {
    let secs = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    next_free(&dir.join(format!("{}.wav", utc_stamp(secs))))
}

/// File name of part `part` of a take: part 1 is `first` itself, part 2 is
/// `stem-002.ext`, and so on.
pub fn part_path(first: &Path, part: u32) -> PathBuf {
    if part <= 1 {
        return first.to_path_buf();
    }
    numbered(first, &format!("{part:03}"))
}

/// `path` if nothing exists there, else `stem-2.ext`, `stem-3.ext`, ... (the
/// first that doesn't exist).
pub fn next_free(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    (2u32..)
        .map(|n| numbered(path, &n.to_string()))
        .find(|p| !p.exists())
        .unwrap_or_else(|| path.to_path_buf())
}

/// `stem-<suffix>.ext` next to `path`.
fn numbered(path: &Path, suffix: &str) -> PathBuf {
    let stem = path.file_stem().unwrap_or_else(|| OsStr::new("take"));
    let mut name = stem.to_os_string();
    name.push("-");
    name.push(suffix);
    if let Some(ext) = path.extension() {
        name.push(".");
        name.push(ext);
    }
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn utc_stamp_epoch_and_leap_day() {
        assert_eq!(utc_stamp(0), "1970-01-01_00-00-00");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29_00-00-00");
        assert_eq!(utc_stamp(951_868_799), "2000-02-29_23-59-59");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blast-rec-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_path_avoids_existing_file() {
        let dir = temp_dir("naming-default");
        let now = UNIX_EPOCH + Duration::from_secs(951_782_400);
        let first = default_path(&dir, now);
        assert_eq!(first, dir.join("2000-02-29_00-00-00.wav"));
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(
            default_path(&dir, now),
            dir.join("2000-02-29_00-00-00-2.wav")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn part_path_numbers_from_two() {
        let first = Path::new("out/take.wav");
        assert_eq!(part_path(first, 1), first);
        assert_eq!(part_path(first, 2), Path::new("out/take-002.wav"));
        assert_eq!(part_path(first, 10), Path::new("out/take-010.wav"));
    }

    #[test]
    fn next_free_numbers_existing() {
        let dir = temp_dir("naming-free");
        let take = dir.join("take.wav");
        assert_eq!(next_free(&take), take);
        std::fs::write(&take, b"x").unwrap();
        assert_eq!(next_free(&take), dir.join("take-2.wav"));
        std::fs::write(dir.join("take-2.wav"), b"x").unwrap();
        assert_eq!(next_free(&take), dir.join("take-3.wav"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
