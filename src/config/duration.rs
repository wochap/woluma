use std::time::Duration;

/// Parses durations like `400ms`, `2s`, `5m`, `1h`.
pub fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit())?;
    let (number, unit) = text.split_at(split);
    let number: u64 = number.parse().ok()?;
    let millis = match unit.trim() {
        "ms" => number,
        "s" => number.checked_mul(1_000)?,
        "m" => number.checked_mul(60_000)?,
        "h" => number.checked_mul(3_600_000)?,
        _ => return None,
    };
    Some(Duration::from_millis(millis))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units() {
        assert_eq!(parse_duration("400ms"), Some(Duration::from_millis(400)));
        assert_eq!(parse_duration("2s"), Some(Duration::from_secs(2)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration(" 0s "), Some(Duration::ZERO));
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "2", "s", "two seconds", "1.5s", "-1s", "2d"] {
            assert_eq!(parse_duration(bad), None, "{bad}");
        }
    }
}
