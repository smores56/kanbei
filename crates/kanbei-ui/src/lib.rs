//! Kernel-owned terminal/fallback boundary (architecture.md "UI" section,
//! R-27). This crate is the terminal safety group of the kernel API
//! inventory: `init`/`restore`/`read_input` (sanitized)/`render_snapshot`/
//! `fallback`.
//!
//! Structural guarantee (consistency 13, "Hot path"): this crate has **no**
//! dependency on kanbei-vm or kanbei-modules. ratatui-backed rendering, input
//! decoding/sanitization, focus/modal invariants, and accessibility validation
//! are pure Rust; Luau/Wasm produces only `SemanticTree` data and never draws
//! terminal cells (R-27).
//!
//! Fault-class split (R-27): composition-validation failure is surfaced as a
//! staleness banner (a kernel overlay, see [`frame`]); a runtime component
//! fault becomes a kernel-authored placeholder tree ([`fallback`]); a kernel
//! render fault falls back to the kernel fallback UI ([`fallback::FallbackUi`])
//! and terminal restoration stays reliable ([`terminal::TerminalGuard`]).

pub mod accessibility;
pub mod builtin;
pub mod fallback;
pub mod focus;
pub mod frame;
pub mod input;
pub mod terminal;
pub mod theme;
pub mod transcript;
pub mod tree;
pub mod tui;

pub use builtin::{BUILTIN_UI_COMPONENT, BUILTIN_UI_NAME, BUILTIN_UI_SOURCE};
pub use focus::{FocusDirection, FocusModel, KeyClassifier, ReservedAction};
pub use frame::{RenderContext, RenderError, RenderOutput};
pub use input::{InputDecoder, InputEvent, UiEvent, UiEventKind, UiProvenance};
pub use terminal::{Terminal, TerminalGuard, TermiosTerminal};
pub use theme::{Color, Style, Theme};
pub use transcript::{TranscriptRow, render_transcript, transcript_rows};
pub use tui::{
    build_viewport, key_to_input, resolve_style, total_rows, transcript_paragraph, Row, StyledRow,
};
pub use tree::{ListItem, Node, NodeKind, NodeProps, SemanticTree, Span, TreeError};
