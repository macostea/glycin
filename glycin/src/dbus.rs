//! Internal DBus API

use crate::api::{self, SandboxMechanism};
use crate::config;

use async_std::process::ExitStatus;
use futures::channel::oneshot;
use futures::future;
use futures::FutureExt;
use gdk::prelude::*;
use gio::glib;
use glycin_utils::*;
use zbus::zvariant;

use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct DecoderProcess<'a> {
    _dbus_connection: zbus::Connection,
    decoding_instruction: DecodingInstructionProxy<'a>,
    mime_type: String,
}

impl<'a> DecoderProcess<'a> {
    pub async fn new(
        mime_type: &config::MimeType,
        config: &config::Config,
        sandbox_mechanism: SandboxMechanism,
        cancellable: &gio::Cancellable,
    ) -> Result<DecoderProcess<'a>, Error> {
        let decoder_bin = config.get(mime_type)?.exec.clone();

        let (unix_stream, fd_decoder) = std::os::unix::net::UnixStream::pair()?;
        unix_stream
            .set_nonblocking(true)
            .expect("Couldn't set nonblocking");
        fd_decoder
            .set_nonblocking(true)
            .expect("Couldn't set nonblocking");

        let (bin, args, final_arg) = match sandbox_mechanism {
            SandboxMechanism::Bwrap => (
                "bwrap".into(),
                vec![
                    "--unshare-all",
                    "--die-with-parent",
                    // change working directory to something that exists
                    "--chdir",
                    "/",
                    "--ro-bind",
                    "/",
                    "/",
                    "--dev",
                    "/dev",
                ],
                Some(decoder_bin),
            ),
            SandboxMechanism::FlatpakSpawn => {
                (
                    "flatpak-spawn".into(),
                    vec![
                        "--sandbox",
                        // die with parent
                        "--watch-bus",
                        // change working directory to something that exists
                        "--directory=/",
                    ],
                    Some(decoder_bin),
                )
            }
            SandboxMechanism::NotSandboxed => {
                eprintln!("WARNING: Glycin running without sandbox.");
                (decoder_bin, vec![], None)
            }
        };

        let mut command = async_std::process::Command::new(bin);

        command.stdin(OwnedFd::from(fd_decoder));

        command.args(args);
        if let Some(arg) = final_arg {
            command.arg(arg);
        }

        let cmd_debug = format!("{:?}", command);
        let mut subprocess = command
            .spawn()
            .map_err(|err| Error::SpawnError(cmd_debug, Arc::new(err)))?;

        let guid = zbus::Guid::generate();
        let dbus_result = zbus::ConnectionBuilder::unix_stream(unix_stream)
            .p2p()
            .server(&guid)
            .auth_mechanisms(&[zbus::AuthMechanism::Anonymous])
            .build()
            .shared();

        futures::select! {
            _result = dbus_result.clone().fuse() => Ok(()),
            _result = cancellable.future().fuse() => {
                let _result = subprocess.kill();
                Err(glib::Error::from(gio::Cancelled)).map_err(Into::into)
            },
            return_status = subprocess.status().fuse() => match return_status {
                Ok(status) => Err(Error::PrematureExit(status)),
                Err(err) => Err(err.into()),
            }
        }?;

        cancellable.connect_cancelled_local(move |_| {
            let _result = subprocess.kill();
        });

        let dbus_connection = dbus_result.await?;

        let decoding_instruction = DecodingInstructionProxy::new(&dbus_connection)
            .await
            .expect("Failed to create decoding instruction proxy");

        Ok(Self {
            _dbus_connection: dbus_connection,
            decoding_instruction,
            mime_type: mime_type.to_string(),
        })
    }

    pub async fn init(
        &self,
        gfile_worker: GFileWorker,
        base_dir: Option<std::path::PathBuf>,
    ) -> Result<ImageInfo, Error> {
        let (remote_reader, writer) = std::os::unix::net::UnixStream::pair()?;

        gfile_worker.write_to(writer)?;

        let fd = unsafe { zvariant::OwnedFd::from_raw_fd(remote_reader.as_raw_fd()) };
        let mime_type = self.mime_type.clone();

        let details = DecodingDetails {
            mime_type,
            base_dir: base_dir.into(),
        };

        let image_info = self
            .decoding_instruction
            .init(DecodingRequest { fd, details })
            .shared();

        let reader_error = gfile_worker.error();
        futures::pin_mut!(reader_error);

        futures::select! {
            _result = image_info.clone().fuse() => Ok(()),
            result = reader_error.fuse() => result,
        }?;

        image_info.await.map_err(Into::into)
    }

    pub async fn decode_frame(&self, frame_request: FrameRequest) -> Result<api::Frame, Error> {
        let mut frame = self
            .decoding_instruction
            .decode_frame(frame_request)
            .await?;

        let Texture::MemFd(fd) = &frame.texture;
        let raw_fd = fd.as_raw_fd();
        let mut mmap = unsafe { memmap::MmapMut::map_mut(raw_fd) }?;

        if mmap.len() < (frame.stride * frame.height).try_usize()? {
            return Err(Error::TextureTooSmall {
                texture_size: mmap.len(),
                frame: format!("{:?}", frame),
            });
        }

        if frame.stride < frame.width * frame.memory_format.n_bytes().u32() {
            return Err(Error::StrideTooSmall(format!("{:?}", frame)));
        }

        // Align stride with pixel size if necessary
        let mut mmap = if frame.stride % frame.memory_format.n_bytes().u32() == 0 {
            mmap
        } else {
            let width = frame.width.try_usize()? * frame.memory_format.n_bytes().usize();
            let stride = frame.stride.try_usize()?;
            let mut source = vec![0; width];
            for row in 1..frame.height.try_usize()? {
                source.copy_from_slice(&mmap[row * stride..row * stride + width]);
                mmap[row * width..(row + 1) * width].copy_from_slice(&source);
            }
            frame.stride = width.try_u32()?;

            // This mmap would have the wrong size after ftruncate
            drop(mmap);

            nix::unistd::ftruncate(
                raw_fd,
                (frame.height * frame.stride)
                    .try_into()
                    .map_err(|_| ConversionTooLargerError)?,
            )
            .unwrap();

            // Need a new mmap with correct size
            unsafe { memmap::MmapMut::map_mut(raw_fd) }?
        };

        if let Err(err) = crate::icc::apply_transformation(&frame, &mut mmap) {
            eprintln!("Failed to apply ICC profile: {err}");
        }
        drop(mmap);

        let mfd = memfd::Memfd::try_from_fd(raw_fd).unwrap();
        // 🦭
        mfd.add_seals(&[
            memfd::FileSeal::SealShrink,
            memfd::FileSeal::SealGrow,
            memfd::FileSeal::SealWrite,
            memfd::FileSeal::SealSeal,
        ])
        .unwrap();

        let bytes: glib::Bytes = unsafe {
            let mmap = glib::ffi::g_mapped_file_new_from_fd(
                raw_fd,
                glib::ffi::GFALSE,
                std::ptr::null_mut(),
            );
            let bytes = glib::translate::from_glib_full(glib::ffi::g_mapped_file_get_bytes(mmap));
            glib::ffi::g_mapped_file_unref(mmap);
            bytes
        };

        let texture = gdk::MemoryTexture::new(
            frame.width.try_i32()?,
            frame.height.try_i32()?,
            gdk_memory_format(frame.memory_format),
            &bytes,
            frame.stride.try_usize()?,
        );

        Ok(api::Frame {
            texture: texture.upcast(),
            delay: frame.delay.into(),
        })
    }
}

use std::io::Write;
const BUF_SIZE: usize = u16::MAX as usize;

#[zbus::dbus_proxy(
    interface = "org.gnome.glycin.DecodingInstruction",
    default_path = "/org/gnome/glycin"
)]
trait DecodingInstruction {
    async fn init(&self, message: DecodingRequest) -> Result<ImageInfo, RemoteError>;
    async fn decode_frame(&self, frame_request: FrameRequest) -> Result<Frame, RemoteError>;
}

const fn gdk_memory_format(format: MemoryFormat) -> gdk::MemoryFormat {
    match format {
        MemoryFormat::B8g8r8a8Premultiplied => gdk::MemoryFormat::B8g8r8a8Premultiplied,
        MemoryFormat::A8r8g8b8Premultiplied => gdk::MemoryFormat::A8r8g8b8Premultiplied,
        MemoryFormat::R8g8b8a8Premultiplied => gdk::MemoryFormat::R8g8b8a8Premultiplied,
        MemoryFormat::B8g8r8a8 => gdk::MemoryFormat::B8g8r8a8,
        MemoryFormat::A8r8g8b8 => gdk::MemoryFormat::A8r8g8b8,
        MemoryFormat::R8g8b8a8 => gdk::MemoryFormat::R8g8b8a8,
        MemoryFormat::A8b8g8r8 => gdk::MemoryFormat::A8b8g8r8,
        MemoryFormat::R8g8b8 => gdk::MemoryFormat::R8g8b8,
        MemoryFormat::B8g8r8 => gdk::MemoryFormat::B8g8r8,
        MemoryFormat::R16g16b16 => gdk::MemoryFormat::R16g16b16,
        MemoryFormat::R16g16b16a16Premultiplied => gdk::MemoryFormat::R16g16b16a16Premultiplied,
        MemoryFormat::R16g16b16a16 => gdk::MemoryFormat::R16g16b16a16,
        MemoryFormat::R16g16b16Float => gdk::MemoryFormat::R16g16b16Float,
        MemoryFormat::R16g16b16a16Float => gdk::MemoryFormat::R16g16b16a16Float,
        MemoryFormat::R32g32b32Float => gdk::MemoryFormat::R32g32b32Float,
        MemoryFormat::R32g32b32a32FloatPremultiplied => {
            gdk::MemoryFormat::R32g32b32a32FloatPremultiplied
        }
        MemoryFormat::R32g32b32a32Float => gdk::MemoryFormat::R32g32b32a32Float,
        MemoryFormat::G8a8 => gdk::MemoryFormat::__Unknown(19),
        MemoryFormat::G8 => gdk::MemoryFormat::__Unknown(20),
        MemoryFormat::G16a16 => gdk::MemoryFormat::__Unknown(22),
        MemoryFormat::G16 => gdk::MemoryFormat::__Unknown(23),
    }
}

pub struct GFileWorker {
    file: gio::File,
    writer_send: Mutex<Option<oneshot::Sender<UnixStream>>>,
    first_bytes_recv: future::Shared<oneshot::Receiver<Arc<Vec<u8>>>>,
    error_recv: future::Shared<oneshot::Receiver<Result<(), Error>>>,
}
use std::sync::Mutex;
impl GFileWorker {
    pub fn spawn(file: gio::File, cancellable: gio::Cancellable) -> GFileWorker {
        let gfile = file.clone();

        let (error_send, error_recv) = oneshot::channel();
        let (first_bytes_send, first_bytes_recv) = oneshot::channel();
        let (writer_send, writer_recv) = oneshot::channel();

        std::thread::spawn(move || {
            Self::handle_errors(error_send, move || {
                let reader = gfile.read(Some(&cancellable))?;
                let mut buf = vec![0; BUF_SIZE];

                let n = reader.read(&mut buf, Some(&cancellable))?;
                let first_bytes = Arc::new(buf[..n].to_vec());
                first_bytes_send
                    .send(first_bytes.clone())
                    .or(Err(Error::InternalCommunicationCanceled))?;

                let mut writer: UnixStream = async_std::task::block_on(writer_recv)?;

                writer.write_all(&first_bytes)?;
                drop(first_bytes);

                loop {
                    let n = reader.read(&mut buf, Some(&cancellable))?;
                    if n == 0 {
                        break;
                    }
                    writer.write_all(&buf[..n])?;
                }

                Ok(())
            })
        });

        GFileWorker {
            file,
            writer_send: Mutex::new(Some(writer_send)),
            first_bytes_recv: first_bytes_recv.shared(),
            error_recv: error_recv.shared(),
        }
    }

    fn handle_errors(
        error_send: oneshot::Sender<Result<(), Error>>,
        f: impl FnOnce() -> Result<(), Error>,
    ) {
        let result = f();
        let _result = error_send.send(result);
    }

    pub fn write_to(&self, stream: UnixStream) -> Result<(), Error> {
        let sender = std::mem::take(&mut *self.writer_send.lock().unwrap());

        sender
            // TODO: this fails if write_to is called a second time
            .unwrap()
            .send(stream)
            .or(Err(Error::InternalCommunicationCanceled))
    }

    pub fn file(&self) -> &gio::File {
        &self.file
    }

    pub async fn error(&self) -> Result<(), Error> {
        match self.error_recv.clone().await {
            Ok(result) => result,
            Err(_) => Ok(()),
        }
    }

    pub async fn head(&self) -> Result<Arc<Vec<u8>>, Error> {
        futures::select!(
            err = self.error_recv.clone() => err?,
            _bytes = self.first_bytes_recv.clone() => Ok(()),
        )?;

        match self.first_bytes_recv.clone().await {
            Err(_) => self.error_recv.clone().await?.map(|_| Default::default()),
            Ok(bytes) => Ok(bytes),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Error {
    RemoteError(RemoteError),
    GLibError(glib::Error),
    StdIoError(Arc<std::io::Error>),
    InternalCommunicationCanceled,
    UnknownImageFormat(String),
    PrematureExit(ExitStatus),
    ConversionTooLargerError,
    SpawnError(String, Arc<std::io::Error>),
    TextureTooSmall { texture_size: usize, frame: String },
    StrideTooSmall(String),
}

impl Error {
    pub fn unsupported_format(&self) -> Option<String> {
        match self {
            Self::UnknownImageFormat(mime_type) => Some(mime_type.clone()),
            Self::RemoteError(RemoteError::UnsupportedImageFormat(msg)) => Some(msg.clone()),
            _ => None,
        }
    }
}

impl From<RemoteError> for Error {
    fn from(err: RemoteError) -> Self {
        Self::RemoteError(err)
    }
}

impl From<glib::Error> for Error {
    fn from(err: glib::Error) -> Self {
        Self::GLibError(err)
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::StdIoError(Arc::new(err))
    }
}

impl From<oneshot::Canceled> for Error {
    fn from(_err: oneshot::Canceled) -> Self {
        Self::InternalCommunicationCanceled
    }
}

impl From<zbus::Error> for Error {
    fn from(err: zbus::Error) -> Self {
        Self::RemoteError(RemoteError::ZBus(err))
    }
}

impl From<ConversionTooLargerError> for Error {
    fn from(_err: ConversionTooLargerError) -> Self {
        Self::ConversionTooLargerError
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> StdResult<(), std::fmt::Error> {
        match self {
            Self::RemoteError(err) => write!(f, "{err}"),
            Self::GLibError(err) => write!(f, "{err}"),
            Self::StdIoError(err) => write!(f, "{err}"),
            Self::InternalCommunicationCanceled => {
                write!(f, "Internal communication was unexpectedly canceled")
            }
            Self::UnknownImageFormat(mime_type) => {
                write!(f, "Unknown image format: {mime_type}")
            }
            Self::PrematureExit(status) => {
                write!(f, "Loader process exited early: {status}")
            }
            err @ Self::ConversionTooLargerError => err.fmt(f),
            Self::SpawnError(cmd, err) => write!(f, "Could not spawn `{cmd}`: {err}"),
            Self::TextureTooSmall {
                texture_size,
                frame,
            } => write!(
                f,
                "Texture is only {texture_size} but was announced differently: {frame}"
            ),
            Self::StrideTooSmall(frame) => write!(f, "Stride is smaller than possible: {frame}"),
        }
    }
}

impl std::error::Error for Error {}

pub type StdResult<T, E> = std::result::Result<T, E>;
