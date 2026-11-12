//! Theme catalog tooling for #1461: converters that translate popular
//! third-party theme formats into OpenCrabs' 43-role user-theme TOML.
//!
//! Two source formats are accepted:
//! - alacritty-theme TOML (`[colors.primary]` bg/fg plus the 16 ANSI colors
//!   under `[colors.normal]` / `[colors.bright]`), the format of the
//!   `alacritty/alacritty-theme` catalog (176 themes).
//! - opencode theme JSON (`defs` + `theme` token maps with `dark`/`light`
//!   variants, hex strings, ANSI integers, defs references and `"none"`),
//!   the format of the `sst/opencode` built-in themes (33 assets).
//!
//! Both converters emit the flat 43-key TOML schema loaded by
//! [`crate::tui::render::user_themes`] and pre-certify their output by
//! running it through the existing validator (`build_theme`), so a returned
//! `Ok(String)` is guaranteed to load unchanged — the hard invariant from
//! issue #1461.
//!
//! This module is generation-time tooling: nothing calls it at runtime (the
//! TUI loads built-ins from `render::presets` and user themes from
//! `~/.opencrabs/themes/`). It ships in the binary so the curated pack can be
//! regenerated from upstream sources and so the fixed role mapping stays
//! exercised by tests.
//!
//! Declarations only, which is what `CONTRIBUTING.md` asks of a `mod.rs`: the
//! palette types live in [`palette`] and each converter in its own file.

pub mod alacritty;
pub mod converter;
pub mod opencode;
pub mod palette;
pub mod theme_pack;

pub use converter::{convert_alacritty_toml, convert_opencode_json};
pub(crate) use palette::XTERM16;
pub use palette::{SourcePalette, Variant, mix};
