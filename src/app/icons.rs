//! The app's icon set: one variant per glyph the UI draws, over a bundle
//! Cadence owns. `file_name` maps a variant to its SVG under
//! `assets/icons`, drawn through gpui-kit's `Icon` so it takes the text
//! colour and size of wherever it sits.

use gpui_kit::{SharedString, component::IconNamed};

/// Every glyph the app draws. A rename or removal fails the build at every
/// call site; a variant missing from the bundle fails `assets::tests`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CadenceIcon {
    ArrowDown,
    ArrowUp,
    Bot,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    CircleCheck,
    Clock,
    Close,
    Copy,
    Ellipsis,
    ExternalLink,
    Folder,
    FolderOpen,
    Heart,
    HeartFill,
    House,
    Key,
    ListMusic,
    LogOut,
    Moon,
    Music,
    Pause,
    Pin,
    PinFill,
    Play,
    Plus,
    Search,
    Settings,
    Shuffle,
    SkipBack,
    SkipForward,
    Sparkles,
    Sun,
    SunMoon,
    User,
    Volume2,
    VolumeX,
}

impl CadenceIcon {
    #[cfg(test)]
    pub(super) const ALL: [Self; 38] = [
        Self::ArrowDown,
        Self::ArrowUp,
        Self::Bot,
        Self::ChevronDown,
        Self::ChevronLeft,
        Self::ChevronRight,
        Self::CircleCheck,
        Self::Clock,
        Self::Close,
        Self::Copy,
        Self::Ellipsis,
        Self::ExternalLink,
        Self::Folder,
        Self::FolderOpen,
        Self::Heart,
        Self::HeartFill,
        Self::House,
        Self::Key,
        Self::ListMusic,
        Self::LogOut,
        Self::Moon,
        Self::Music,
        Self::Pause,
        Self::Pin,
        Self::PinFill,
        Self::Play,
        Self::Plus,
        Self::Search,
        Self::Settings,
        Self::Shuffle,
        Self::SkipBack,
        Self::SkipForward,
        Self::Sparkles,
        Self::Sun,
        Self::SunMoon,
        Self::User,
        Self::Volume2,
        Self::VolumeX,
    ];

    fn file_name(self) -> &'static str {
        match self {
            Self::ArrowDown => "arrow-down",
            Self::ArrowUp => "arrow-up",
            Self::Bot => "bot",
            Self::ChevronDown => "chevron-down",
            Self::ChevronLeft => "chevron-left",
            Self::ChevronRight => "chevron-right",
            Self::CircleCheck => "circle-check",
            Self::Clock => "clock",
            Self::Close => "close",
            Self::Copy => "copy",
            Self::Ellipsis => "ellipsis",
            Self::ExternalLink => "external-link",
            Self::Folder => "folder",
            Self::FolderOpen => "folder-open",
            Self::Heart => "heart",
            Self::HeartFill => "heart-fill",
            Self::House => "house",
            Self::Key => "key",
            Self::ListMusic => "list-music",
            Self::LogOut => "log-out",
            Self::Moon => "moon",
            Self::Music => "music",
            Self::Pause => "pause",
            Self::Pin => "pin",
            Self::PinFill => "pin-fill",
            Self::Play => "play",
            Self::Plus => "plus",
            Self::Search => "search",
            Self::Settings => "settings",
            Self::Shuffle => "shuffle",
            Self::SkipBack => "skip-back",
            Self::SkipForward => "skip-forward",
            Self::Sparkles => "sparkles",
            Self::Sun => "sun",
            Self::SunMoon => "sun-moon",
            Self::User => "user",
            Self::Volume2 => "volume-2",
            Self::VolumeX => "volume-x",
        }
    }
}

impl IconNamed for CadenceIcon {
    fn path(self) -> SharedString {
        format!("icons/{}.svg", self.file_name()).into()
    }
}
