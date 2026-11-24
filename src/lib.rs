//! Dith — an agent-friendly CLI and engine for crafting high-craft dithered imagery.
//!
//! The default `cli` feature builds the full native tool. With `--no-default-features` the
//! engine compiles for `wasm32-unknown-unknown`; add the `wasm` feature for the browser API.

pub mod animate;
#[cfg(feature = "cli")]
pub mod cli;
pub mod color;
#[cfg(feature = "cli")]
mod commands;
pub mod compare;
pub mod dither;
pub mod generate;
#[cfg(feature = "cli")]
pub mod inspect;
pub mod output;
pub mod palette;
mod par;
mod png_optimize;
pub mod preprocess;
pub mod preset;
pub mod recipe;
#[cfg(feature = "cli")]
pub mod update;
#[cfg(feature = "cli")]
pub mod viewer;
#[cfg(feature = "wasm")]
pub mod wasm;
pub mod webp;
