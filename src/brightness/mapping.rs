//! Linear percent <-> raw mapping. `min` is a floor: backlights use 1 so 0% never turns
//! the panel off, DDC uses 0.

pub const BACKLIGHT_MIN: u32 = 1;
pub const DDC_MIN: u32 = 0;

pub fn pct_to_raw(pct: u8, min: u32, max: u32) -> u32 {
    let pct = u64::from(pct.min(100));
    let raw = (pct * u64::from(max) + 50) / 100;
    (raw as u32).clamp(min.min(max), max)
}

pub fn raw_to_pct(raw: u32, max: u32) -> u8 {
    if max == 0 {
        return 0;
    }
    ((u64::from(raw.min(max)) * 100 + u64::from(max) / 2) / u64::from(max)) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_examples() {
        assert_eq!(pct_to_raw(40, BACKLIGHT_MIN, 255), 102);
        assert_eq!(pct_to_raw(0, BACKLIGHT_MIN, 255), 1);
        assert_eq!(pct_to_raw(0, DDC_MIN, 100), 0);
        assert_eq!(raw_to_pct(37, 100), 37);
    }

    #[test]
    fn bounds_and_round_trip() {
        assert_eq!(pct_to_raw(100, BACKLIGHT_MIN, 255), 255);
        assert_eq!(raw_to_pct(255, 255), 100);
        assert_eq!(raw_to_pct(0, 0), 0);
        for pct in 0..=100 {
            assert_eq!(raw_to_pct(pct_to_raw(pct, DDC_MIN, 255), 255), pct);
        }
    }
}
