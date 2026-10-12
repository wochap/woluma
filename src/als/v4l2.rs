//! Minimal V4L2 capture and control ioctls (single-planar MMAP streaming). Hand-rolled on
//! `libc` so the build needs neither libclang nor kernel headers.

use std::ffi::CString;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

const BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const MEMORY_MMAP: u32 = 1;
const FIELD_NONE: u32 = 1;
const FRMSIZE_TYPE_DISCRETE: u32 = 1;

pub const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

#[repr(C)]
struct FmtDesc {
    index: u32,
    typ: u32,
    flags: u32,
    description: [u8; 32],
    pixelformat: u32,
    mbus_code: u32,
    reserved: [u32; 3],
}

#[repr(C)]
struct FrmSizeEnum {
    index: u32,
    pixel_format: u32,
    typ: u32,
    /// Discrete: width, height. Stepwise: min_w, max_w, step_w, min_h, max_h, step_h.
    size: [u32; 6],
    reserved: [u32; 2],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PixFormat {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub bytesperline: u32,
    pub sizeimage: u32,
    colorspace: u32,
    private: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

#[repr(C)]
struct Format {
    typ: u32,
    // The kernel union is 8-byte aligned (it contains pointers) and 200 bytes long.
    _pad: u32,
    pix: PixFormat,
    raw: [u8; 200 - size_of::<PixFormat>()],
}

#[repr(C)]
struct RequestBuffers {
    count: u32,
    typ: u32,
    memory: u32,
    capabilities: u32,
    flags: u8,
    reserved: [u8; 3],
}

#[repr(C)]
struct Buffer {
    index: u32,
    typ: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    timestamp: libc::timeval,
    timecode: [u32; 4],
    sequence: u32,
    memory: u32,
    /// Union of `offset` (u32), `userptr`, `planes`, `fd`.
    m: u64,
    length: u32,
    reserved2: u32,
    request_fd: u32,
}

#[repr(C)]
struct QueryCtrl {
    id: u32,
    typ: u32,
    name: [u8; 32],
    minimum: i32,
    maximum: i32,
    step: i32,
    default_value: i32,
    flags: u32,
    reserved: [u32; 2],
}

#[repr(C)]
struct Control {
    id: u32,
    value: i32,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<FmtDesc>() == 64);
    assert!(size_of::<FrmSizeEnum>() == 44);
    assert!(size_of::<Format>() == 208);
    assert!(size_of::<RequestBuffers>() == 20);
    assert!(size_of::<Buffer>() == 88);
    assert!(size_of::<QueryCtrl>() == 68);
    assert!(size_of::<Control>() == 8);
};

const fn iowr<T>(nr: u64) -> u64 {
    (3 << 30) | ((size_of::<T>() as u64) << 16) | ((b'V' as u64) << 8) | nr
}

const fn iow<T>(nr: u64) -> u64 {
    (1 << 30) | ((size_of::<T>() as u64) << 16) | ((b'V' as u64) << 8) | nr
}

const VIDIOC_ENUM_FMT: u64 = iowr::<FmtDesc>(2);
const VIDIOC_S_FMT: u64 = iowr::<Format>(5);
const VIDIOC_REQBUFS: u64 = iowr::<RequestBuffers>(8);
const VIDIOC_QUERYBUF: u64 = iowr::<Buffer>(9);
const VIDIOC_QBUF: u64 = iowr::<Buffer>(15);
const VIDIOC_DQBUF: u64 = iowr::<Buffer>(17);
const VIDIOC_STREAMON: u64 = iow::<i32>(18);
const VIDIOC_STREAMOFF: u64 = iow::<i32>(19);
const VIDIOC_G_CTRL: u64 = iowr::<Control>(27);
const VIDIOC_S_CTRL: u64 = iowr::<Control>(28);
const VIDIOC_QUERYCTRL: u64 = iowr::<QueryCtrl>(36);
const VIDIOC_ENUM_FRAMESIZES: u64 = iowr::<FrmSizeEnum>(74);

pub struct Device {
    fd: OwnedFd,
}

impl Device {
    pub fn open(path: &Path) -> io::Result<Self> {
        let path = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: valid NUL-terminated path; the fd is owned from here on.
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }

    fn ioctl<T>(&self, request: u64, arg: &mut T) -> io::Result<()> {
        loop {
            // SAFETY: `request` encodes `size_of::<T>()` and `T` mirrors the kernel struct.
            let ret = unsafe { libc::ioctl(self.fd.as_raw_fd(), request as _, arg as *mut T) };
            if ret >= 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                return Err(error);
            }
        }
    }

    /// Pixel formats offered for capture.
    pub fn formats(&self) -> io::Result<Vec<u32>> {
        let mut formats = Vec::new();
        for index in 0.. {
            let mut desc: FmtDesc = unsafe { zeroed() };
            desc.index = index;
            desc.typ = BUF_TYPE_VIDEO_CAPTURE;
            match self.ioctl(VIDIOC_ENUM_FMT, &mut desc) {
                Ok(()) => formats.push(desc.pixelformat),
                Err(e) if e.raw_os_error() == Some(libc::EINVAL) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(formats)
    }

    /// Frame sizes for `pixelformat`; stepwise ranges yield their minimum.
    pub fn frame_sizes(&self, pixelformat: u32) -> io::Result<Vec<(u32, u32)>> {
        let mut sizes = Vec::new();
        for index in 0.. {
            let mut size: FrmSizeEnum = unsafe { zeroed() };
            size.index = index;
            size.pixel_format = pixelformat;
            match self.ioctl(VIDIOC_ENUM_FRAMESIZES, &mut size) {
                Ok(()) if size.typ == FRMSIZE_TYPE_DISCRETE => {
                    sizes.push((size.size[0], size.size[1]))
                }
                Ok(()) => {
                    sizes.push((size.size[0], size.size[3]));
                    break;
                }
                Err(e) if e.raw_os_error() == Some(libc::EINVAL) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(sizes)
    }

    /// Applies a capture format, returning what the driver chose.
    pub fn set_format(&self, width: u32, height: u32, pixelformat: u32) -> io::Result<PixFormat> {
        let mut format: Format = unsafe { zeroed() };
        format.typ = BUF_TYPE_VIDEO_CAPTURE;
        format.pix.width = width;
        format.pix.height = height;
        format.pix.pixelformat = pixelformat;
        format.pix.field = FIELD_NONE;
        self.ioctl(VIDIOC_S_FMT, &mut format)?;
        Ok(format.pix)
    }

    pub fn control(&self, id: u32) -> io::Result<i32> {
        let mut control = Control { id, value: 0 };
        self.ioctl(VIDIOC_G_CTRL, &mut control)?;
        Ok(control.value)
    }

    pub fn set_control(&self, id: u32, value: i32) -> io::Result<()> {
        self.ioctl(VIDIOC_S_CTRL, &mut Control { id, value })
    }

    /// The (minimum, maximum) of a control.
    pub fn control_range(&self, id: u32) -> io::Result<(i32, i32)> {
        let mut query: QueryCtrl = unsafe { zeroed() };
        query.id = id;
        self.ioctl(VIDIOC_QUERYCTRL, &mut query)?;
        Ok((query.minimum, query.maximum))
    }

    /// Streams `frames` frames through `buffers` MMAP buffers, calling `on_frame` with each.
    pub fn capture<E: From<io::Error>>(
        &self,
        buffers: u32,
        frames: u32,
        timeout: Duration,
        mut on_frame: impl FnMut(&[u8]) -> Result<(), E>,
    ) -> Result<(), E> {
        let stream = Stream::start(self, buffers)?;
        for _ in 0..frames {
            let (index, used) = stream.dequeue(timeout)?;
            let mapping = &stream.mappings[index];
            // SAFETY: the buffer is dequeued, so the driver is not writing to it.
            let data = unsafe { std::slice::from_raw_parts(mapping.0 as *const u8, mapping.1) };
            on_frame(&data[..used.min(mapping.1)])?;
            stream.queue(index as u32)?;
        }
        Ok(())
    }
}

/// Active MMAP stream; stops streaming, unmaps and frees the buffers on drop.
struct Stream<'a> {
    device: &'a Device,
    mappings: Vec<(*mut libc::c_void, usize)>,
    streaming: bool,
}

impl<'a> Stream<'a> {
    fn start(device: &'a Device, count: u32) -> io::Result<Self> {
        let mut request: RequestBuffers = unsafe { zeroed() };
        request.count = count;
        request.typ = BUF_TYPE_VIDEO_CAPTURE;
        request.memory = MEMORY_MMAP;
        device.ioctl(VIDIOC_REQBUFS, &mut request)?;
        let mut stream = Self {
            device,
            mappings: Vec::new(),
            streaming: false,
        };
        for index in 0..request.count {
            let mut buffer = stream.buffer(index);
            device.ioctl(VIDIOC_QUERYBUF, &mut buffer)?;
            // SAFETY: mapping the driver buffer at the offset it reported.
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    buffer.length as usize,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    device.fd.as_raw_fd(),
                    (buffer.m & 0xffff_ffff) as libc::off_t,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            stream.mappings.push((ptr, buffer.length as usize));
            stream.queue(index)?;
        }
        let mut typ = BUF_TYPE_VIDEO_CAPTURE as i32;
        device.ioctl(VIDIOC_STREAMON, &mut typ)?;
        stream.streaming = true;
        Ok(stream)
    }

    fn buffer(&self, index: u32) -> Buffer {
        let mut buffer: Buffer = unsafe { zeroed() };
        buffer.index = index;
        buffer.typ = BUF_TYPE_VIDEO_CAPTURE;
        buffer.memory = MEMORY_MMAP;
        buffer
    }

    fn queue(&self, index: u32) -> io::Result<()> {
        self.device.ioctl(VIDIOC_QBUF, &mut self.buffer(index))
    }

    /// Waits for a filled buffer, returning its index and used bytes.
    fn dequeue(&self, timeout: Duration) -> io::Result<(usize, usize)> {
        let mut poll = libc::pollfd {
            fd: self.device.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, timeout.as_millis() as i32) };
        match ready {
            r if r < 0 => return Err(io::Error::last_os_error()),
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "no frame from the camera",
                ))
            }
            _ => {}
        }
        let mut buffer = self.buffer(0);
        self.device.ioctl(VIDIOC_DQBUF, &mut buffer)?;
        Ok((buffer.index as usize, buffer.bytesused as usize))
    }
}

impl Drop for Stream<'_> {
    fn drop(&mut self) {
        if self.streaming {
            let mut typ = BUF_TYPE_VIDEO_CAPTURE as i32;
            let _ = self.device.ioctl(VIDIOC_STREAMOFF, &mut typ);
        }
        for &(ptr, len) in &self.mappings {
            // SAFETY: each mapping came from `mmap` above.
            unsafe { libc::munmap(ptr, len) };
        }
        let mut request: RequestBuffers = unsafe { zeroed() };
        request.typ = BUF_TYPE_VIDEO_CAPTURE;
        request.memory = MEMORY_MMAP;
        let _ = self.device.ioctl(VIDIOC_REQBUFS, &mut request);
    }
}
