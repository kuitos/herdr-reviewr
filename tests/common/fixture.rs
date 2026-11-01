//! The fake herdr's fixture-file naming, shared by the fake and the tests that write fixtures.
#![allow(unreachable_pub)]

use std::path::{Path, PathBuf};

/// The fixture file for `pane`, `:` spelled `_` as Windows requires.
pub fn fixture(dir: &Path, kind: &str, pane: &str, suffix: &str) -> PathBuf {
    dir.join(format!("{kind}-{}{suffix}", pane.replace(':', "_")))
}
