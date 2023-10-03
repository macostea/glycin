//! Utilities for building glycin decoders

#![cfg_attr(docsrs, feature(doc_auto_cfg))]

#[cfg(feature = "image-rs")]
#[doc(hidden)]
pub mod image_rs;

pub use anyhow;
pub use std::os::unix::net::UnixStream;

use anyhow::Context;
use gettextrs::gettext;
use serde::{Deserialize, Serialize};
use zbus::zvariant::{self, Optional, Type};

use std::ffi::CString;
use std::ops::{Deref, DerefMut};
use std::os::fd::AsRawFd;
use std::os::fd::{FromRawFd, IntoRawFd, RawFd};
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug)]
pub struct SharedMemory {
    memfd: RawFd,
    pub mmap: memmap::MmapMut,
}

impl SharedMemory {
    pub fn new(size: u64) -> Self {
        let memfd = nix::sys::memfd::memfd_create(
            &CString::new("glycin-frame").unwrap(),
            nix::sys::memfd::MemFdCreateFlag::MFD_CLOEXEC
                | nix::sys::memfd::MemFdCreateFlag::MFD_ALLOW_SEALING,
        )
        .expect("Failed to create memfd");
        nix::unistd::ftruncate(memfd, size.try_into().expect("Required memory too large"))
            .expect("Failed to set memfd size");
        let mmap = unsafe { memmap::MmapMut::map_mut(memfd) }.expect("Mailed to mmap memfd");

        Self { mmap, memfd }
    }

    pub fn into_texture(self) -> Texture {
        let owned_fd = unsafe { zvariant::OwnedFd::from_raw_fd(self.memfd) };
        Texture::MemFd(owned_fd)
    }
}

impl Deref for SharedMemory {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.mmap.deref()
    }
}

impl DerefMut for SharedMemory {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.mmap.deref_mut()
    }
}

#[derive(Deserialize, Serialize, Type, Debug)]
pub struct DecodingRequest {
    /// Source from which the loader reads the image data
    pub fd: zvariant::OwnedFd,
    pub details: DecodingDetails,
}

#[derive(Deserialize, Serialize, Type, Debug)]
pub struct DecodingDetails {
    pub mime_type: String,
    pub base_dir: Optional<std::path::PathBuf>,
}

#[derive(Deserialize, Serialize, Type, Debug, Clone, Default)]
pub struct FrameRequest {
    pub scale: Optional<(u32, u32)>,
    /// Instruction to only decode part of the image
    pub clip: Optional<(u32, u32, u32, u32)>,
}

/// Various image metadata
#[derive(Deserialize, Serialize, Type, Debug, Clone)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub format_name: String,
    pub exif: Optional<Vec<u8>>,
    pub xmp: Optional<Vec<u8>>,
    pub transformations_applied: bool,
    pub dimensions_text: Optional<String>,
    pub dimensions_inch: Optional<(f64, f64)>,
}

impl ImageInfo {
    pub fn new(width: u32, height: u32, format_name: String) -> Self {
        Self {
            width,
            height,
            format_name,
            exif: None.into(),
            xmp: None.into(),
            transformations_applied: false,
            dimensions_text: None.into(),
            dimensions_inch: None.into(),
        }
    }
}

#[derive(Deserialize, Serialize, Type, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub memory_format: MemoryFormat,
    pub texture: Texture,
    pub iccp: Optional<Vec<u8>>,
    pub cicp: Optional<Vec<u8>>,
    pub delay: Optional<Duration>,
}

impl Frame {
    pub fn new(width: u32, height: u32, memory_format: MemoryFormat, texture: Texture) -> Self {
        let stride = memory_format.n_bytes().u32() * width;

        Self {
            width,
            height,
            stride,
            memory_format,
            texture,
            iccp: None.into(),
            cicp: None.into(),
            delay: None.into(),
        }
    }
}

#[derive(Deserialize, Serialize, Type, Debug)]
pub enum Texture {
    MemFd(zvariant::OwnedFd),
}

#[derive(Deserialize, Serialize, Type, Debug, Clone, Copy)]
pub enum MemoryFormat {
    B8g8r8a8Premultiplied,
    A8r8g8b8Premultiplied,
    R8g8b8a8Premultiplied,
    B8g8r8a8,
    A8r8g8b8,
    R8g8b8a8,
    A8b8g8r8,
    R8g8b8,
    B8g8r8,
    R16g16b16,
    R16g16b16a16Premultiplied,
    R16g16b16a16,
    R16g16b16Float,
    R16g16b16a16Float,
    R32g32b32Float,
    R32g32b32a32FloatPremultiplied,
    R32g32b32a32Float,
    G8a8,
    G8,
    G16a16,
    G16,
}

impl MemoryFormat {
    pub const fn n_bytes(self) -> MemoryFormatBytes {
        match self {
            MemoryFormat::B8g8r8a8Premultiplied => MemoryFormatBytes::B4,
            MemoryFormat::A8r8g8b8Premultiplied => MemoryFormatBytes::B4,
            MemoryFormat::R8g8b8a8Premultiplied => MemoryFormatBytes::B4,
            MemoryFormat::B8g8r8a8 => MemoryFormatBytes::B4,
            MemoryFormat::A8r8g8b8 => MemoryFormatBytes::B4,
            MemoryFormat::R8g8b8a8 => MemoryFormatBytes::B4,
            MemoryFormat::A8b8g8r8 => MemoryFormatBytes::B4,
            MemoryFormat::R8g8b8 => MemoryFormatBytes::B3,
            MemoryFormat::B8g8r8 => MemoryFormatBytes::B3,
            MemoryFormat::R16g16b16 => MemoryFormatBytes::B6,
            MemoryFormat::R16g16b16a16Premultiplied => MemoryFormatBytes::B8,
            MemoryFormat::R16g16b16a16 => MemoryFormatBytes::B8,
            MemoryFormat::R16g16b16Float => MemoryFormatBytes::B6,
            MemoryFormat::R16g16b16a16Float => MemoryFormatBytes::B8,
            MemoryFormat::R32g32b32Float => MemoryFormatBytes::B12,
            MemoryFormat::R32g32b32a32FloatPremultiplied => MemoryFormatBytes::B16,
            MemoryFormat::R32g32b32a32Float => MemoryFormatBytes::B16,
            MemoryFormat::G8a8 => MemoryFormatBytes::B2,
            MemoryFormat::G8 => MemoryFormatBytes::B1,
            MemoryFormat::G16a16 => MemoryFormatBytes::B4,
            MemoryFormat::G16 => MemoryFormatBytes::B2,
        }
    }

    pub const fn n_channels(self) -> u8 {
        match self {
            MemoryFormat::B8g8r8a8Premultiplied => 4,
            MemoryFormat::A8r8g8b8Premultiplied => 4,
            MemoryFormat::R8g8b8a8Premultiplied => 4,
            MemoryFormat::B8g8r8a8 => 4,
            MemoryFormat::A8r8g8b8 => 4,
            MemoryFormat::R8g8b8a8 => 4,
            MemoryFormat::A8b8g8r8 => 4,
            MemoryFormat::R8g8b8 => 3,
            MemoryFormat::B8g8r8 => 3,
            MemoryFormat::R16g16b16 => 3,
            MemoryFormat::R16g16b16a16Premultiplied => 4,
            MemoryFormat::R16g16b16a16 => 4,
            MemoryFormat::R16g16b16Float => 3,
            MemoryFormat::R16g16b16a16Float => 4,
            MemoryFormat::R32g32b32Float => 3,
            MemoryFormat::R32g32b32a32FloatPremultiplied => 4,
            MemoryFormat::R32g32b32a32Float => 4,
            MemoryFormat::G8a8 => 2,
            MemoryFormat::G8 => 1,
            MemoryFormat::G16a16 => 2,
            MemoryFormat::G16 => 1,
        }
    }
}

pub enum MemoryFormatBytes {
    B1 = 1,
    B2 = 2,
    B3 = 3,
    B4 = 4,
    B6 = 6,
    B8 = 8,
    B12 = 12,
    B16 = 16,
}

impl MemoryFormatBytes {
    pub fn u32(self) -> u32 {
        self as u32
    }

    pub fn u64(self) -> u64 {
        self as u64
    }

    pub fn usize(self) -> usize {
        self as usize
    }
}

pub struct Communication {
    _dbus_connection: zbus::Connection,
}

impl Communication {
    pub fn spawn(decoder: impl Decoder + 'static) {
        async_std::task::block_on(async move {
            let _connection = Communication::new(decoder).await;
            std::future::pending::<()>().await;
        })
    }

    pub async fn new(decoder: impl Decoder + 'static) -> Self {
        let unix_stream = unsafe { UnixStream::from_raw_fd(std::io::stdin().as_raw_fd()) };

        let instruction_handler = DecodingInstruction {
            decoder: Mutex::new(Box::new(decoder)),
        };
        let dbus_connection = zbus::ConnectionBuilder::unix_stream(unix_stream)
            .p2p()
            .auth_mechanisms(&[zbus::AuthMechanism::Anonymous])
            .serve_at("/org/gnome/glycin", instruction_handler)
            .expect("Failed to setup instruction handler")
            .build()
            .await
            .expect("Failed to create private DBus connection");

        Communication {
            _dbus_connection: dbus_connection,
        }
    }
}

pub trait Decoder: Send {
    fn init(&self, stream: UnixStream, details: DecodingDetails)
        -> Result<ImageInfo, DecoderError>;
    fn decode_frame(&self, frame_request: FrameRequest) -> Result<Frame, DecoderError>;
}

struct DecodingInstruction {
    decoder: Mutex<Box<dyn Decoder>>,
}

#[zbus::dbus_interface(name = "org.gnome.glycin.DecodingInstruction")]
impl DecodingInstruction {
    async fn init(&self, message: DecodingRequest) -> Result<ImageInfo, RemoteError> {
        let fd = message.fd.into_raw_fd();
        let stream = unsafe { UnixStream::from_raw_fd(fd) };

        let image_info = self
            .decoder
            .lock()
            .or(Err(RemoteError::InternalDecoderError))?
            .init(stream, message.details)?;

        Ok(image_info)
    }

    async fn decode_frame(&self, frame_request: FrameRequest) -> Result<Frame, RemoteError> {
        self.decoder
            .lock()
            .or(Err(RemoteError::InternalDecoderError))?
            .decode_frame(frame_request)
            .map_err(Into::into)
    }
}

#[derive(zbus::DBusError, Debug, Clone)]
#[dbus_error(prefix = "org.gnome.glycin.Error")]
pub enum RemoteError {
    #[dbus_error(zbus_error)]
    ZBus(zbus::Error),
    DecodingError(String),
    InternalDecoderError,
    UnsupportedImageFormat(String),
    ConversionTooLargerError,
}

impl From<DecoderError> for RemoteError {
    fn from(err: DecoderError) -> Self {
        match err {
            DecoderError::DecodingError(msg) => Self::DecodingError(msg),
            DecoderError::InternalDecoderError => Self::InternalDecoderError,
            DecoderError::UnsupportedImageFormat(msg) => Self::UnsupportedImageFormat(msg),
            DecoderError::ConversionTooLargerError => Self::ConversionTooLargerError,
        }
    }
}

#[derive(Debug)]
pub enum DecoderError {
    DecodingError(String),
    InternalDecoderError,
    UnsupportedImageFormat(String),
    ConversionTooLargerError,
}

impl std::fmt::Display for DecoderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            Self::DecodingError(err) => write!(f, "{err}"),
            Self::InternalDecoderError => {
                write!(f, "{}", gettext("Internal error while interpreting image"))
            }
            Self::UnsupportedImageFormat(msg) => {
                write!(f, "{} {msg}", gettext("Unsupported image format: "))
            }
            err @ Self::ConversionTooLargerError => err.fmt(f),
        }
    }
}

impl std::error::Error for DecoderError {}

impl From<anyhow::Error> for DecoderError {
    fn from(err: anyhow::Error) -> Self {
        eprintln!("Decoding error: {err:?}");
        Self::DecodingError(format!("{err}"))
    }
}

impl From<ConversionTooLargerError> for DecoderError {
    fn from(err: ConversionTooLargerError) -> Self {
        eprintln!("Decoding error: {err:?}");
        Self::ConversionTooLargerError
    }
}

pub trait GenericContexts<T> {
    fn context_failed(self) -> anyhow::Result<T>;
    fn context_internal(self) -> Result<T, DecoderError>;
    fn context_unsupported(self, msg: String) -> Result<T, DecoderError>;
}

impl<T, E> GenericContexts<T> for Result<T, E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn context_failed(self) -> anyhow::Result<T> {
        self.with_context(|| gettext("Failed to decode image"))
    }

    fn context_internal(self) -> Result<T, DecoderError> {
        self.map_err(|_| DecoderError::InternalDecoderError)
    }

    fn context_unsupported(self, msg: String) -> Result<T, DecoderError> {
        self.map_err(|_| DecoderError::UnsupportedImageFormat(msg))
    }
}

impl<T> GenericContexts<T> for Option<T> {
    fn context_failed(self) -> anyhow::Result<T> {
        self.with_context(|| gettext("Failed to decode image"))
    }

    fn context_internal(self) -> Result<T, DecoderError> {
        self.ok_or(DecoderError::InternalDecoderError)
    }

    fn context_unsupported(self, msg: String) -> Result<T, DecoderError> {
        self.ok_or(DecoderError::UnsupportedImageFormat(msg))
    }
}

#[derive(Debug)]
pub struct ConversionTooLargerError;

impl std::fmt::Display for ConversionTooLargerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.write_str(&gettext("Dimension too large for system"))
    }
}

impl std::error::Error for ConversionTooLargerError {}

pub trait SafeConversion:
    TryInto<usize> + TryInto<i32> + TryInto<u32> + TryInto<i64> + TryInto<u64>
{
    fn try_usize(self) -> Result<usize, ConversionTooLargerError> {
        self.try_into().map_err(|_| ConversionTooLargerError)
    }

    fn try_i32(self) -> Result<i32, ConversionTooLargerError> {
        self.try_into().map_err(|_| ConversionTooLargerError)
    }

    fn try_u32(self) -> Result<u32, ConversionTooLargerError> {
        self.try_into().map_err(|_| ConversionTooLargerError)
    }

    fn try_i64(self) -> Result<i64, ConversionTooLargerError> {
        self.try_into().map_err(|_| ConversionTooLargerError)
    }

    fn try_u64(self) -> Result<u64, ConversionTooLargerError> {
        self.try_into().map_err(|_| ConversionTooLargerError)
    }
}

impl SafeConversion for usize {}
impl SafeConversion for u32 {}
impl SafeConversion for i32 {}
