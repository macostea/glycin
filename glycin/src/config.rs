use async_std::{fs, path, prelude::*};
use gio::glib;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::OnceLock;

use crate::dbus::Error;

pub type MimeType = String;

const CONFIG_FILE_EXT: &str = "conf";
const API_VERSION: u8 = 0;

static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub image_decoders: HashMap<MimeType, ImageDecoderConfig>,
}

#[derive(Debug, Clone)]
pub struct ImageDecoderConfig {
    pub exec: std::path::PathBuf,
    pub expose_base_dir: bool,
}

impl Config {
    pub async fn cached() -> &'static Self {
        if let Some(config) = CONFIG.get() {
            config
        } else {
            let config = Self::load().await;
            CONFIG.get_or_init(|| config)
        }
    }

    pub fn get(&self, mime_type: &MimeType) -> Result<&ImageDecoderConfig, Error> {
        self.image_decoders
            .get(mime_type.as_str())
            .ok_or_else(|| Error::UnknownImageFormat(mime_type.to_string()))
    }

    async fn load() -> Self {
        let mut config = Config::default();

        let mut data_dirs = glib::system_data_dirs();
        data_dirs.push(glib::user_data_dir());

        for mut data_dir in data_dirs {
            data_dir.push("glycin-loaders");
            data_dir.push(format!("{API_VERSION}+"));
            data_dir.push("conf.d");

            if let Ok(mut config_files) = fs::read_dir(data_dir).await {
                while let Some(result) = config_files.next().await {
                    if let Ok(entry) = result {
                        if entry.path().extension() == Some(OsStr::new(CONFIG_FILE_EXT)) {
                            if let Err(err) = Self::load_file(&entry.path(), &mut config).await {
                                eprintln!("Failed to load config file: {err}");
                            }
                        }
                    }
                }
            }
        }

        config
    }

    async fn load_file(
        path: &path::Path,
        config: &mut Config,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let data = async_std::fs::read(path).await?;
        let bytes = glib::Bytes::from_owned(data);

        let keyfile = glib::KeyFile::new();
        keyfile.load_from_bytes(&bytes, glib::KeyFileFlags::NONE)?;

        for group in keyfile.groups() {
            let mut elements = group.to_str().split(':');
            let kind = elements.next();
            let mime_type = elements.next();

            if kind == Some("loader") {
                if let Some(mime_type) = mime_type {
                    let group = group.to_str().trim();
                    if let Ok(exec) = keyfile.string(group, "Exec") {
                        let expose_base_dir =
                            keyfile.boolean(group, "ExposeBaseDir").unwrap_or_default();

                        let cfg = ImageDecoderConfig {
                            exec: exec.into(),
                            expose_base_dir,
                        };

                        config.image_decoders.insert(mime_type.to_string(), cfg);
                    }
                }
            }
        }

        Ok(())
    }
}
