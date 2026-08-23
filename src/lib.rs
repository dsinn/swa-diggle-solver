pub mod act;
pub mod combat;
pub mod config;
pub mod itemchoice;
pub mod items;
pub mod fight;
pub mod game;
pub mod gear;
pub mod geometry;
pub mod heroselect;
pub mod innplay;
pub mod layout;
pub mod letters;
pub mod parity;
pub mod pick;
pub mod lexica;
pub mod navigate;
pub mod observe;
pub mod overworld;
pub mod score;
pub mod stamp;
pub mod rest;
pub mod rested;
pub mod flee;
pub mod search;
pub mod shrine;
pub mod shopplay;
pub mod shrineplay;
pub mod subworld;
pub mod buyer;
pub mod tables;
pub mod timing;
pub mod tower;
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
