//! Pure rule engine: condition grammar, matching, settle and hysteresis.

mod condition;
mod engine;

pub use condition::{parse_num_cond, parse_time_range, NumCond, TimeRange};
pub use engine::{Engine, Inputs, Transition};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct When {
    pub luma: Option<NumCond>,
    pub als: Option<NumCond>,
    pub time: Option<TimeRange>,
}

impl When {
    pub fn is_empty(&self) -> bool {
        self.luma.is_none() && self.als.is_none() && self.time.is_none()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub name: String,
    pub when: When,
    pub brightness: Option<u8>,
    pub run: Vec<String>,
}
