//! Library crate for the flamegraph viewer.
//!
//! Exposes the common profile [`profile::Profile`] representation and the
//! [`parsers`] that build it from the supported profile formats. The Bevy
//! binary (`main.rs`), the `validate` binary and the integration tests all
//! share this code.

pub mod flame;
pub mod parsers;
pub mod profile;
