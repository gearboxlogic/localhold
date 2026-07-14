//! Tincture palette for the TUI, following `assets/brand/cli.md`: truecolor
//! when the terminal advertises it, indexed 256-color fallbacks otherwise.

use ratatui::style::{Color, Modifier, Style};

/// Brand tinctures resolved against terminal capabilities.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Theme {
    /// Or — the accent. One gold voice per screen.
    pub or: Color,
    /// Azure (light variant) — identifiers, scopes, paths.
    pub azure: Color,
    /// Vert — success verbs only.
    pub vert: Color,
    /// Gules — error verbs only.
    pub gules: Color,
}

impl Theme {
    /// Pick truecolor or indexed tinctures from `COLORTERM`.
    pub(crate) fn detect() -> Self {
        let truecolor = std::env::var("COLORTERM").is_ok_and(|value| value.contains("truecolor") || value.contains("24bit"));
        if truecolor { Self::truecolor() } else { Self::indexed() }
    }

    /// Dark-theme truecolor variants from the brand spec.
    const fn truecolor() -> Self {
        Self {
            or: Color::Rgb(200, 155, 60),
            azure: Color::Rgb(127, 163, 212),
            vert: Color::Rgb(107, 163, 131),
            gules: Color::Rgb(200, 106, 97),
        }
    }

    /// 256-color fallbacks from the brand spec.
    const fn indexed() -> Self {
        Self {
            or: Color::Indexed(179),
            azure: Color::Indexed(110),
            vert: Color::Indexed(65),
            gules: Color::Indexed(131),
        }
    }

    /// Dim slate style for secondary text and uppercase labels.
    #[expect(clippy::unused_self, reason = "keeps the palette API uniform with the tincture styles")]
    pub(crate) fn label(self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }

    /// Gold accent style.
    pub(crate) fn accent(self) -> Style {
        Style::default().fg(self.or)
    }

    /// Azure identifier style.
    pub(crate) fn ident(self) -> Style {
        Style::default().fg(self.azure)
    }

    /// Success-verb style.
    pub(crate) fn held(self) -> Style {
        Style::default().fg(self.vert)
    }

    /// Error-verb style.
    pub(crate) fn not_held(self) -> Style {
        Style::default().fg(self.gules)
    }
}
