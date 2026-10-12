// Adapted from wluma (https://github.com/max-baz/wluma).
// Copyright (c) 2020-2022, Maxim Baz & Cyril Levis. ISC License, see NOTICE.

pub mod wayland;

use std::fmt;

pub use super::SAMPLE_INTERVAL;

#[derive(Debug, Clone, PartialEq)]
pub enum WaylandProtocol {
    Any,
    ExtImageCopyCaptureV1,
    WlrScreencopyUnstableV1,
    WlrExportDmabufUnstableV1,
}

impl WaylandProtocol {
    pub const fn actual_name(&self) -> Option<&'static str> {
        match self {
            Self::Any => None,
            Self::ExtImageCopyCaptureV1 => Some("ext-image-copy-capture-v1"),
            Self::WlrScreencopyUnstableV1 => Some("wlr-screencopy-unstable-v1"),
            Self::WlrExportDmabufUnstableV1 => Some("wlr-export-dmabuf-unstable-v1"),
        }
    }
}

impl fmt::Display for WaylandProtocol {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.actual_name().unwrap_or("any"))
    }
}
