//! Brightness devices (sysfs backlight, DDC/CI) and the per-output controller that applies
//! rule brightness, ramps, detects manual changes and pauses.

pub mod backlight;
mod controller;
pub mod ddc;
pub mod mapping;
mod ramp;

#[allow(unused_imports)] // `Status` is read by the control socket (add-control-cli)
pub use controller::{Connector, Controller, Msg, Profile, Settings, Status, Timing};

use anyhow::Result;

/// A brightness device in raw units. Calls may block.
pub trait Device: Send + 'static {
    fn read(&mut self) -> Result<u32>;
    fn write(&mut self, raw: u32) -> Result<()>;
    fn min(&self) -> u32;
    fn max(&self) -> u32;
    /// Starts pushing raw values on external change notifications. Devices without change
    /// events are polled by the controller instead.
    fn watch(&mut self, _sink: async_channel::Sender<u32>) -> Result<()> {
        Ok(())
    }
}
