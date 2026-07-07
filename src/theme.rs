// Use the terminal palette instead of fixed RGB values. `Reset` lets the main
// workspace inherit the terminal's configured foreground/background, while the
// named colors below resolve through the user's ANSI color scheme.
use ratatui::style::Color;

pub const BG: Color = Color::Reset;
pub const PANEL2: Color = Color::Black;
pub const BORDER: Color = Color::DarkGray;
pub const BORDER_SOFT: Color = Color::DarkGray;
pub const ACCENT: Color = Color::LightGreen;
pub const ACCENT_DIM: Color = Color::Green;
pub const ON_ACCENT: Color = Color::Black;
// Background for rows in a visual selection; distinct from the subtler PANEL2
// cursor tint while still coming from the terminal's ANSI palette.
pub const SEL: Color = Color::DarkGray;
pub const TEXT: Color = Color::Reset;
pub const MUTED: Color = Color::Gray;
pub const FAINT: Color = Color::DarkGray;
pub const ERROR: Color = Color::LightRed;

// SQL syntax
pub const SQL_KEYWORD: Color = Color::Magenta;
pub const SQL_STRING: Color = Color::Green;
pub const SQL_NUMBER: Color = Color::Yellow;
pub const SQL_COMMENT: Color = Color::DarkGray;
pub const SQL_OPERATOR: Color = Color::Gray;

// Result value categories
pub const V_STRING: Color = Color::Yellow;
pub const V_UUID: Color = Color::Cyan;
pub const V_DATE: Color = Color::Magenta;
pub const V_NUMBER: Color = Color::Blue;
pub const V_BOOL: Color = Color::LightMagenta;
pub const V_BINARY: Color = Color::Green;
pub const V_NULL: Color = Color::DarkGray;

// JSON viewer
pub const J_PROP: Color = Color::Cyan;
pub const J_STRING: Color = Color::Yellow;
pub const J_NUMBER: Color = Color::Blue;
pub const J_BOOL: Color = Color::Magenta;
pub const J_PUNCT: Color = Color::Gray;
