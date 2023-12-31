//! # Overview
//!
//! Glycin allows to decode images into [`gdk::Texture`]s and to extract image metadata.
//! The decoding happens in sandboxed modular image decoders.
//!
//! # Example
//!
//! ```no_run
//! # use glycin::*;
//! # async_std::task::block_on(async {
//! let file = gio::File::for_path("image.jpg");
//! let image = ImageRequest::new(file).request().await?;
//!
//! let height = image.info().height;
//! let texture = image.next_frame().await?;
//! # Ok::<(), Error>(()) });
//! ```

pub mod dbus;

mod api;
mod config;
mod icc;

pub use api::*;
pub use glycin_utils::{ImageInfo, RemoteError};
