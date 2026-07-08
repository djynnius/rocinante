//! Human-friendly interval parsing for `/loop`: "30s", "5m", "1h", "90m".

use std::time::Duration;

pub fn parse(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (digits, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let value: u64 = digits
        .parse()
        .map_err(|_| format!("bad interval `{s}` — use forms like 30s, 5m, 1h"))?;
    if value == 0 {
        return Err("interval must be positive".into());
    }
    let seconds = match unit.trim() {
        "s" | "sec" | "secs" => value,
        "m" | "min" | "mins" => value * 60,
        "h" | "hr" | "hrs" => value * 3600,
        _ => return Err(format!("bad interval unit `{unit}` — use s, m, or h")),
    };
    const MIN: u64 = 5;
    if seconds < MIN {
        return Err(format!("interval too short — minimum {MIN}s"));
    }
    Ok(Duration::from_secs(seconds))
}

/// Compact display: 300s -> "5m".
pub fn display(d: Duration) -> String {
    let secs = d.as_secs();
    if secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units() {
        assert_eq!(parse("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse("1h").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse("").is_err());
        assert!(parse("5x").is_err());
        assert!(parse("m").is_err());
        assert!(parse("0m").is_err());
        assert!(parse("2s").is_err()); // below minimum
    }

    #[test]
    fn displays_compactly() {
        assert_eq!(display(Duration::from_secs(300)), "5m");
        assert_eq!(display(Duration::from_secs(3600)), "1h");
        assert_eq!(display(Duration::from_secs(45)), "45s");
    }
}
