//! The tovuer client: logic in [`app`], drawing in [`ui`], the link to the
//! local peer node in [`net`], files and updates in [`transfer`], the
//! `tovuer` terminal command in [`install`].

pub mod app;
pub mod demo;
pub mod install;
pub mod net;
pub mod settings;
pub mod transfer;
pub mod ui;

/// Build version: set by a release through `TOVUER_VERSION`, otherwise the package version.
pub const VERSION: &str = match option_env!("TOVUER_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
