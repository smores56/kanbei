//! Kernel-owned terminal/fallback boundary (architecture.md "UI" section,
//! R-27). This crate is the terminal safety group of the kernel API
//! inventory: `init`/`restore`/`read_input` (sanitized)/`render_snapshot`/
//! `fallback`.
//!
//! Structural guarantee (consistency 13, "Hot path"): this crate has **no**
//! dependency on kanbei-vm or kanbei-modules. Terminal-cell rendering, input
//! decoding/sanitization, focus/modal invariants, accessibility validation,
//! and render diffing are pure Rust; Luau/Wasm produces only `SemanticTree`
//! data and never draws terminal cells (R-27).
//!
//! Fault-class split (R-27): composition-validation failure is surfaced as a
//! staleness banner (a kernel overlay, see [`frame`]); a runtime component
//! fault becomes a kernel-authored placeholder tree ([`fallback`]); a kernel
//! render fault falls back to the kernel fallback UI ([`fallback::FallbackUi`])
//! and terminal restoration stays reliable ([`terminal::TerminalGuard`]).

pub mod accessibility;
pub mod builtin;
pub mod conversation;
pub mod diff;
pub mod fallback;
pub mod focus;
pub mod frame;
pub mod input;
pub mod terminal;
pub mod theme;
pub mod tree;
pub mod tui;

pub use builtin::{BUILTIN_UI_COMPONENT, BUILTIN_UI_NAME, BUILTIN_UI_SOURCE};
pub use conversation::{
    BubbleRow, ConversationState, OutcomeClass, StepStatus, ToolStep, TranscriptRow, TurnState,
    TurnView,
};
pub use diff::{CellEdit, FrameDiff};
pub use focus::{FocusDirection, FocusModel, KeyClassifier, ReservedAction};
pub use frame::{RenderContext, RenderError, RenderOutput, TerminalFrame};
pub use input::{InputDecoder, InputEvent, UiEvent, UiEventKind, UiProvenance};
pub use terminal::{Terminal, TerminalGuard, TermiosTerminal};
pub use theme::{Color, Style, Theme};
pub use tui::{
    build_viewport, key_to_input, resolve_style, total_rows, transcript_paragraph, Row, StyledRow,
};
pub use tree::{Node, NodeKind, SemanticTree, TreeError};
