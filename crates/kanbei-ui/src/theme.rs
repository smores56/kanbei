//! Named styles: the kernel-owned theme. Theme overlays contributed through
//! the scope composition are merged here (later overlays replace top-level
//! keys, mirroring kanbei-scopes' theme semantics); the kernel resolves
//! module style keys against this theme and never trusts unknown keys.

use std::collections::HashMap;

use serde_json::Value;

/// The default style key: used for unstyled cells.
pub const DEFAULT_STYLE: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Default,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    White,
    BrightBlack,
    BrightRed,
    BrightGreen,
    BrightYellow,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
    BrightWhite,
}

impl Color {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "default" => Color::Default,
            "black" => Color::Black,
            "red" => Color::Red,
            "green" => Color::Green,
            "yellow" => Color::Yellow,
            "blue" => Color::Blue,
            "magenta" => Color::Magenta,
            "cyan" => Color::Cyan,
            "white" => Color::White,
            "bright_black" | "gray" => Color::BrightBlack,
            "bright_red" => Color::BrightRed,
            "bright_green" => Color::BrightGreen,
            "bright_yellow" => Color::BrightYellow,
            "bright_blue" => Color::BrightBlue,
            "bright_magenta" => Color::BrightMagenta,
            "bright_cyan" => Color::BrightCyan,
            "bright_white" => Color::BrightWhite,
            _ => return None,
        })
    }

    /// SGR foreground code.
    pub fn ansi_fg(self) -> &'static str {
        match self {
            Color::Default => "39",
            Color::Black => "30",
            Color::Red => "31",
            Color::Green => "32",
            Color::Yellow => "33",
            Color::Blue => "34",
            Color::Magenta => "35",
            Color::Cyan => "36",
            Color::White => "37",
            Color::BrightBlack => "90",
            Color::BrightRed => "91",
            Color::BrightGreen => "92",
            Color::BrightYellow => "93",
            Color::BrightBlue => "94",
            Color::BrightMagenta => "95",
            Color::BrightCyan => "96",
            Color::BrightWhite => "97",
        }
    }

    /// SGR background code.
    pub fn ansi_bg(self) -> &'static str {
        match self {
            Color::Default => "49",
            Color::Black => "40",
            Color::Red => "41",
            Color::Green => "42",
            Color::Yellow => "43",
            Color::Blue => "44",
            Color::Magenta => "45",
            Color::Cyan => "46",
            Color::White => "47",
            Color::BrightBlack => "100",
            Color::BrightRed => "101",
            Color::BrightGreen => "102",
            Color::BrightYellow => "103",
            Color::BrightBlue => "104",
            Color::BrightMagenta => "105",
            Color::BrightCyan => "106",
            Color::BrightWhite => "107",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub underline: bool,
    pub reverse: bool,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            fg: Color::Default,
            bg: Color::Default,
            bold: false,
            underline: false,
            reverse: false,
        }
    }
}

/// Parse a theme overlay entry: `{"fg": "...", "bg": "...", "bold": bool,
/// "underline": bool, "reverse": bool}`. Unknown color names fail closed.
pub fn parse_style(v: &Value) -> Result<Style, ThemeError> {
    let obj = v.as_object().ok_or(ThemeError::NotAnObject)?;
    let mut style = Style::default();
    if let Some(fg) = obj.get("fg").and_then(Value::as_str) {
        style.fg = Color::parse(fg).ok_or_else(|| ThemeError::UnknownColor(fg.to_string()))?;
    }
    if let Some(bg) = obj.get("bg").and_then(Value::as_str) {
        style.bg = Color::parse(bg).ok_or_else(|| ThemeError::UnknownColor(bg.to_string()))?;
    }
    if let Some(bold) = obj.get("bold") {
        style.bold = bold.as_bool().ok_or(ThemeError::NotAnObject)?;
    }
    if let Some(underline) = obj.get("underline") {
        style.underline = underline.as_bool().ok_or(ThemeError::NotAnObject)?;
    }
    if let Some(reverse) = obj.get("reverse") {
        style.reverse = reverse.as_bool().ok_or(ThemeError::NotAnObject)?;
    }
    Ok(style)
}

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("theme overlay must be an object of named styles")]
    NotAnObject,
    #[error("unknown color {0:?}")]
    UnknownColor(String),
}

/// Kernel-owned theme: named styles. Unknown style keys resolve to
/// `DEFAULT_STYLE` at render time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    pub styles: HashMap<String, Style>,
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_theme()
    }
}

impl Theme {
    /// The built-in default theme: header/status/input/list_item/selected/
    /// error/banner named styles over a default foreground.
    pub fn default_theme() -> Self {
        let mut styles = HashMap::new();
        styles.insert(DEFAULT_STYLE.to_string(), Style::default());
        styles.insert(
            "header".to_string(),
            Style {
                fg: Color::BrightWhite,
                bold: true,
                ..Style::default()
            },
        );
        styles.insert(
            "status".to_string(),
            Style {
                fg: Color::BrightBlack,
                ..Style::default()
            },
        );
        styles.insert(
            "input".to_string(),
            Style {
                fg: Color::BrightWhite,
                ..Style::default()
            },
        );
        styles.insert(
            "list_item".to_string(),
            Style {
                fg: Color::Default,
                ..Style::default()
            },
        );
        styles.insert(
            "selected".to_string(),
            Style {
                reverse: true,
                ..Style::default()
            },
        );
        styles.insert(
            "error".to_string(),
            Style {
                fg: Color::BrightRed,
                ..Style::default()
            },
        );
        styles.insert(
            "banner".to_string(),
            Style {
                fg: Color::Black,
                bg: Color::BrightYellow,
                bold: true,
                ..Style::default()
            },
        );
        Theme { styles }
    }

    /// Apply a composition theme overlay: top-level keys replace existing
    /// styles; unknown keys are added.
    pub fn apply_overlay(&mut self, overlay: &Value) -> Result<(), ThemeError> {
        let obj = overlay.as_object().ok_or(ThemeError::NotAnObject)?;
        for (name, v) in obj {
            let style = parse_style(v)?;
            self.styles.insert(name.clone(), style);
        }
        Ok(())
    }

    /// Resolve a named style, falling back to the default style for unknown
    /// or empty names.
    pub fn style(&self, name: Option<&str>) -> &Style {
        match name {
            Some(n) if !n.is_empty() => self.styles.get(n).unwrap_or(&self.styles[DEFAULT_STYLE]),
            _ => &self.styles[DEFAULT_STYLE],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn overlay_merges_and_replaces() {
        let mut theme = Theme::default_theme();
        theme
            .apply_overlay(&json!({"header": {"fg": "red", "bold": false}, "custom": {"fg": "green"}}))
            .unwrap();
        assert_eq!(theme.style(Some("header")).fg, Color::Red);
        assert!(!theme.style(Some("header")).bold);
        assert_eq!(theme.style(Some("custom")).fg, Color::Green);
        assert_eq!(theme.style(Some("nope")).fg, Color::Default);
    }

    #[test]
    fn unknown_color_fails_closed() {
        let err = Theme::default_theme()
            .apply_overlay(&json!({"x": {"fg": "chartreuse"}}))
            .unwrap_err();
        assert!(matches!(err, ThemeError::UnknownColor(_)));
    }
}
