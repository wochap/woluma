use chrono::NaiveTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// A `luma`/`als` condition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NumCond {
    Cmp {
        op: Op,
        value: f64,
    },
    /// Inclusive range.
    Range {
        lo: f64,
        hi: f64,
    },
}

/// Parses a non-negative decimal in 0–100. `Err(None)` means malformed syntax.
fn parse_number(text: &str) -> Result<f64, Option<String>> {
    let text = text.trim();
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let valid = match text.split_once('.') {
        Some((int, frac)) => digits(int) && digits(frac),
        None => digits(text),
    };
    let value: f64 = if valid {
        text.parse().map_err(|_| None)?
    } else {
        return Err(None);
    };
    if value > 100.0 {
        return Err(Some(format!("{text} is out of range 0-100")));
    }
    Ok(value)
}

pub fn parse_num_cond(text: &str) -> Result<NumCond, String> {
    let invalid = || {
        format!(
            "invalid condition \"{text}\": an explicit operator is required, use one of \
             \"< N\", \"<= N\", \"> N\", \">= N\", \"== N\", \"!= N\" or \"A..B\""
        )
    };
    let trimmed = text.trim();
    if let Some((lo, hi)) = trimmed.split_once("..") {
        let lo = parse_number(lo).map_err(|e| e.unwrap_or_else(invalid))?;
        let hi = parse_number(hi).map_err(|e| e.unwrap_or_else(invalid))?;
        if lo > hi {
            return Err(format!(
                "inverted range \"{trimmed}\": start must not exceed end"
            ));
        }
        return Ok(NumCond::Range { lo, hi });
    }
    let (op, rest) = [
        ("<=", Op::Le),
        (">=", Op::Ge),
        ("==", Op::Eq),
        ("!=", Op::Ne),
        ("<", Op::Lt),
        (">", Op::Gt),
    ]
    .into_iter()
    .find_map(|(sym, op)| trimmed.strip_prefix(sym).map(|rest| (op, rest)))
    .ok_or_else(invalid)?;
    let value = parse_number(rest).map_err(|e| e.unwrap_or_else(invalid))?;
    Ok(NumCond::Cmp { op, value })
}

impl NumCond {
    pub fn matches(&self, v: f64) -> bool {
        match *self {
            NumCond::Cmp { op, value } => match op {
                Op::Lt => v < value,
                Op::Le => v <= value,
                Op::Gt => v > value,
                Op::Ge => v >= value,
                Op::Eq => v == value,
                Op::Ne => v != value,
            },
            NumCond::Range { lo, hi } => lo <= v && v <= hi,
        }
    }

    /// Widens the condition by `h` in the direction that keeps it matching, clamped to 0–100.
    pub fn widened(&self, h: f64) -> NumCond {
        let clamp = |v: f64| v.clamp(0.0, 100.0);
        match *self {
            NumCond::Cmp { op, value } => match op {
                Op::Gt | Op::Ge => NumCond::Cmp {
                    op,
                    value: clamp(value - h),
                },
                Op::Lt | Op::Le => NumCond::Cmp {
                    op,
                    value: clamp(value + h),
                },
                Op::Eq => NumCond::Range {
                    lo: clamp(value - h),
                    hi: clamp(value + h),
                },
                Op::Ne => *self,
            },
            NumCond::Range { lo, hi } => NumCond::Range {
                lo: clamp(lo - h),
                hi: clamp(hi + h),
            },
        }
    }
}

/// Half-open local time range `[start, end)`, wrapping past midnight when `end < start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    pub start: NaiveTime,
    pub end: NaiveTime,
}

pub fn parse_time_range(text: &str) -> Result<TimeRange, String> {
    let invalid = || format!("invalid time range \"{text}\", expected \"HH:MM-HH:MM\"");
    let (start, end) = text.trim().split_once('-').ok_or_else(invalid)?;
    let parse = |s: &str| {
        let s = s.trim();
        let ok = s.len() == 5 && s.as_bytes()[2] == b':';
        ok.then(|| NaiveTime::parse_from_str(s, "%H:%M").ok())
            .flatten()
            .ok_or_else(invalid)
    };
    let (start, end) = (parse(start)?, parse(end)?);
    if start == end {
        return Err(format!(
            "time range \"{text}\" is empty: start and end must differ"
        ));
    }
    Ok(TimeRange { start, end })
}

impl TimeRange {
    pub fn contains(&self, t: NaiveTime) -> bool {
        if self.start < self.end {
            self.start <= t && t < self.end
        } else {
            t >= self.start || t < self.end
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn parses_operators() {
        assert_eq!(
            parse_num_cond(">= 70"),
            Ok(NumCond::Cmp {
                op: Op::Ge,
                value: 70.0
            })
        );
        assert_eq!(
            parse_num_cond("<20"),
            Ok(NumCond::Cmp {
                op: Op::Lt,
                value: 20.0
            })
        );
        assert_eq!(
            parse_num_cond(" <= 20.5 "),
            Ok(NumCond::Cmp {
                op: Op::Le,
                value: 20.5
            })
        );
        assert_eq!(
            parse_num_cond("> 0"),
            Ok(NumCond::Cmp {
                op: Op::Gt,
                value: 0.0
            })
        );
        assert_eq!(
            parse_num_cond("== 50"),
            Ok(NumCond::Cmp {
                op: Op::Eq,
                value: 50.0
            })
        );
        assert_eq!(
            parse_num_cond("!= 50"),
            Ok(NumCond::Cmp {
                op: Op::Ne,
                value: 50.0
            })
        );
        assert_eq!(
            parse_num_cond("30..70"),
            Ok(NumCond::Range { lo: 30.0, hi: 70.0 })
        );
        assert_eq!(
            parse_num_cond(" 30 .. 70 "),
            Ok(NumCond::Range { lo: 30.0, hi: 70.0 })
        );
    }

    #[test]
    fn rejects_bare_and_malformed() {
        for bad in [
            "70", "", "=> 70", "= 70", ">= ", ">= abc", ">= -5", ">= 101", "1..", "..5", "a..b",
        ] {
            assert!(parse_num_cond(bad).is_err(), "{bad}");
        }
        assert!(parse_num_cond("70")
            .unwrap_err()
            .contains("explicit operator"));
        assert!(parse_num_cond("70..30").unwrap_err().contains("inverted"));
    }

    #[test]
    fn matching() {
        let ge = parse_num_cond(">= 70").unwrap();
        assert!(ge.matches(70.0) && ge.matches(100.0) && !ge.matches(69.0));
        let range = parse_num_cond("30..70").unwrap();
        assert!(range.matches(30.0) && range.matches(70.0) && !range.matches(71.0));
        let ne = parse_num_cond("!= 50").unwrap();
        assert!(!ne.matches(50.0) && ne.matches(49.0));
    }

    #[test]
    fn widening_table() {
        let cases = [
            (
                ">= 70",
                NumCond::Cmp {
                    op: Op::Ge,
                    value: 65.0,
                },
            ),
            (
                "> 70",
                NumCond::Cmp {
                    op: Op::Gt,
                    value: 65.0,
                },
            ),
            (
                "<= 20",
                NumCond::Cmp {
                    op: Op::Le,
                    value: 25.0,
                },
            ),
            (
                "< 20",
                NumCond::Cmp {
                    op: Op::Lt,
                    value: 25.0,
                },
            ),
            ("30..70", NumCond::Range { lo: 25.0, hi: 75.0 }),
            ("== 50", NumCond::Range { lo: 45.0, hi: 55.0 }),
            (
                "!= 50",
                NumCond::Cmp {
                    op: Op::Ne,
                    value: 50.0,
                },
            ),
            (
                ">= 2",
                NumCond::Cmp {
                    op: Op::Ge,
                    value: 0.0,
                },
            ),
            (
                "<= 98",
                NumCond::Cmp {
                    op: Op::Le,
                    value: 100.0,
                },
            ),
            ("0..100", NumCond::Range { lo: 0.0, hi: 100.0 }),
        ];
        for (text, expected) in cases {
            assert_eq!(
                parse_num_cond(text).unwrap().widened(5.0),
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn time_ranges() {
        let day = parse_time_range("07:00-21:00").unwrap();
        assert!(day.contains(t(14, 30)) && day.contains(t(7, 0)) && !day.contains(t(21, 0)));
        let night = parse_time_range("21:00-07:00").unwrap();
        assert!(night.contains(t(2, 15)) && night.contains(t(21, 0)) && night.contains(t(23, 59)));
        assert!(!night.contains(t(7, 0)) && !night.contains(t(12, 0)));
    }

    #[test]
    fn time_range_errors() {
        for bad in [
            "07:00",
            "7:00-21:00",
            "07:00-25:00",
            "aa:bb-cc:dd",
            "07:00-21:00-22:00",
        ] {
            assert!(parse_time_range(bad).is_err(), "{bad}");
        }
        assert!(parse_time_range("07:00-07:00")
            .unwrap_err()
            .contains("differ"));
    }
}
