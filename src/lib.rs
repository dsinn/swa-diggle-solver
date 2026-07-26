pub mod game;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("APPDATA environment variable is not set")]
    NoAppData,
    #[error("save directory does not exist: {0}")]
    SaveDirMissing(PathBuf),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("lua: {0}")]
    Lua(#[from] mlua::Error),
}
