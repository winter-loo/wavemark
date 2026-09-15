//! Time formatting helpers shared by the GUI readouts and the CLI output.
//!
//! Audio editing is all about exact time points. We format to millisecond
//! precision everywhere because an AI agent consuming an annotation needs the
//! exact range, not a rounded one.

/// Format seconds as `M:SS.mmm` (or `H:MM:SS.mmm` past one hour).
pub fn format_ms(seconds: f64) -> String {
    if seconds.is_finite() && seconds >= 0.0 {
        let total_ms = (seconds * 1000.0).round() as u64;
        let ms = total_ms % 1000;
        let total_s = total_ms / 1000;
        let s = total_s % 60;
        let total_m = total_s / 60;
        let m = total_m % 60;
        let h = total_m / 60;
        if h > 0 {
            format!("{h}:{m:02}:{s:02}.{ms:03}")
        } else {
            format!("{m}:{s:02}.{ms:03}")
        }
    } else {
        "0:00.000".to_string()
    }
}

/// Format seconds as `H:MM:SS` style with a leading `0:` only when useful.
pub fn format_hms(seconds: f64) -> String {
    let base = format_ms(seconds);
    // format_ms already handles the h>0 case; expose a seconds-only variant too.
    base
}

/// Parse a timestamp string (`M:SS.mmm`, `SS.mmm`, or plain seconds) to seconds.
pub fn parse_ts(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Plain float seconds ("12.345") with no colon.
    if !s.contains(':') {
        return s.parse::<f64>().ok();
    }
    let parts: Vec<&str> = s.split(':').collect();
    let nums: Vec<f64> = parts
        .iter()
        .map(|p| p.parse::<f64>())
        .collect::<Result<_, _>>()
        .ok()?;
    match nums.len() {
        2 => Some(nums[0] * 60.0 + nums[1]),
        3 => Some(nums[0] * 3600.0 + nums[1] * 60.0 + nums[2]),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_minutes_seconds_millis() {
        assert_eq!(format_ms(0.0), "0:00.000");
        assert_eq!(format_ms(5.4), "0:05.400");
        assert_eq!(format_ms(75.0), "1:15.000");
        assert_eq!(format_ms(3661.5), "1:01:01.500");
    }

    #[test]
    fn parses_back() {
        for v in [0.0, 5.4, 75.0, 3661.5] {
            assert!((parse_ts(&format_ms(v)).unwrap() - v).abs() < 0.002, "{v}");
        }
    }
}
