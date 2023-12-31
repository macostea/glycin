use crate::config;
use crate::dbus::*;
use gio::prelude::*;
use glycin_utils::ImageInfo;
use std::sync::OnceLock;

pub use crate::config::MimeType;
pub use crate::dbus::Error;

static IS_FLATPAKED: OnceLock<bool> = OnceLock::new();

pub type Result<T> = std::result::Result<T, Error>;

async fn is_flatpaked() -> bool {
    if let Some(result) = IS_FLATPAKED.get() {
        *result
    } else {
        let flatpaked = async_std::path::Path::new("/.flatpak-info").is_file().await;
        *IS_FLATPAKED.get_or_init(|| flatpaked)
    }
}

#[derive(Debug, Copy, Clone)]
pub enum SandboxMechanism {
    Bwrap,
    FlatpakSpawn,
    NotSandboxed,
}

impl SandboxMechanism {
    pub async fn detect() -> Self {
        if is_flatpaked().await {
            Self::FlatpakSpawn
        } else {
            Self::Bwrap
        }
    }
}

/// Image request builder
#[derive(Debug)]
pub struct ImageRequest {
    file: gio::File,
    cancellable: gio::Cancellable,
    sandbox_mechanism: Option<SandboxMechanism>,
}

impl ImageRequest {
    pub fn new(file: gio::File) -> Self {
        Self {
            file,
            cancellable: gio::Cancellable::new(),
            sandbox_mechanism: None,
        }
    }

    pub fn sandbox_mechanism(&mut self, sandbox_mechanism: Option<SandboxMechanism>) -> &mut Self {
        self.sandbox_mechanism = sandbox_mechanism;
        self
    }

    pub fn cancellable(&mut self, cancellable: impl IsA<gio::Cancellable>) -> &mut Self {
        self.cancellable = cancellable.upcast();
        self
    }

    pub async fn request<'a>(self) -> Result<Image<'a>> {
        let config = config::Config::cached().await;

        let gfile_worker = GFileWorker::spawn(self.file.clone(), self.cancellable.clone());
        let mime_type = Self::guess_mime_type(&gfile_worker).await?;
        let decoder_config = config.get(&mime_type)?;

        let sandbox_mechanism = if let Some(m) = self.sandbox_mechanism {
            m
        } else {
            SandboxMechanism::detect().await
        };

        let base_dir = if decoder_config.expose_base_dir {
            self.file.parent().and_then(|x| x.path())
        } else {
            None
        };

        let process = DecoderProcess::new(
            &mime_type,
            config,
            sandbox_mechanism,
            self.cancellable.as_ref(),
        )
        .await?;

        let info = process.init(gfile_worker, base_dir).await?;

        Ok(Image {
            process,
            info,
            request: self,
            mime_type,
        })
    }

    async fn guess_mime_type(gfile_worker: &GFileWorker) -> Result<String> {
        let head = gfile_worker.head().await?;
        let (content_type, unsure) = gio::content_type_guess(None::<String>, &head);
        let mime_type = gio::content_type_get_mime_type(&content_type)
            .ok_or_else(|| Error::UnknownImageFormat(content_type.to_string()));

        // Prefer file extension for TIFF since it can be a RAW format as well
        let is_tiff = mime_type.clone().ok() == Some("image/tiff".into());

        if unsure || is_tiff {
            if let Some(filename) = gfile_worker.file().basename() {
                let content_type_fn = gio::content_type_guess(Some(filename), &head).0;
                return gio::content_type_get_mime_type(&content_type_fn)
                    .ok_or_else(|| Error::UnknownImageFormat(content_type_fn.to_string()))
                    .map(|x| x.to_string());
            }
        }

        mime_type.map(|x| x.to_string())
    }
}

/// Image handle containing metadata and allowing frame requests
#[derive(Debug)]
pub struct Image<'a> {
    request: ImageRequest,
    process: DecoderProcess<'a>,
    info: ImageInfo,
    mime_type: MimeType,
}

impl<'a> Image<'a> {
    pub async fn next_frame(&self) -> Result<Frame> {
        self.process
            .decode_frame(glycin_utils::FrameRequest::default())
            .await
            .map_err(Into::into)
    }

    pub async fn texture(self) -> Result<gdk::Texture> {
        self.process
            .decode_frame(glycin_utils::FrameRequest::default())
            .await
            .map(|x| x.texture)
            .map_err(Into::into)
    }

    pub async fn specific_frame(&self, frame_request: FrameRequest) -> Result<Frame> {
        self.process
            .decode_frame(frame_request.request)
            .await
            .map_err(Into::into)
    }

    pub fn info(&self) -> &ImageInfo {
        &self.info
    }

    pub fn mime_type(&self) -> MimeType {
        self.mime_type.clone()
    }

    pub fn format_name(&self) -> String {
        self.info().format_name.clone()
    }

    pub fn request(&self) -> &ImageRequest {
        &self.request
    }
}

impl Drop for ImageRequest {
    fn drop(&mut self) {
        self.cancellable.cancel();
    }
}

pub struct Frame {
    pub texture: gdk::Texture,
    pub delay: Option<std::time::Duration>,
}

#[derive(Default, Debug)]
#[must_use]
pub struct FrameRequest {
    request: glycin_utils::FrameRequest,
}

impl FrameRequest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scale(mut self, width: u32, height: u32) -> Self {
        self.request.scale = Some((width, height)).into();
        self
    }

    pub fn clip(mut self, x: u32, y: u32, width: u32, height: u32) -> Self {
        self.request.clip = Some((x, y, width, height)).into();
        self
    }
}

/// Returns a list of mime types for the supported image formats
pub async fn image_formats() -> Vec<MimeType> {
    config::Config::cached()
        .await
        .image_decoders
        .keys()
        .cloned()
        .collect()
}
