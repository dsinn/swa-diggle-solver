pub mod act;
pub mod combat;
pub mod config;
pub mod game;
pub mod geometry;
pub mod layout;
pub mod lexica;
pub mod observe;
pub mod overworld;
pub mod score;
pub mod rest;
pub mod search;
pub mod shrine;
pub mod subworld;
pub mod tables;
pub mod typist;
pub mod win;

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
    #[error("config: {0}")]
    Config(String),
    #[error("win32: {0}")]
    Win32(String),
}
