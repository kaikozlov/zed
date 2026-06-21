mod app_menu;
mod keyboard;
mod keystroke;

#[cfg(all(target_os = "linux", feature = "wayland"))]
#[expect(missing_docs)]
pub mod layer_shell;

#[cfg(any(test, feature = "bench"))]
mod bench_dispatcher;

#[cfg(any(test, feature = "test-support"))]
mod test;

#[cfg(all(target_os = "macos", any(test, feature = "test-support")))]
mod visual_test;

#[cfg(all(
    feature = "screen-capture",
    any(target_os = "windows", target_os = "linux", target_os = "freebsd",)
))]
pub mod scap_screen_capture;

#[cfg(all(
    any(target_os = "windows", target_os = "linux"),
    feature = "screen-capture"
))]
pub(crate) type PlatformScreenCaptureFrame = scap::frame::Frame;
#[cfg(not(feature = "screen-capture"))]
pub(crate) type PlatformScreenCaptureFrame = ();
#[cfg(all(target_os = "macos", feature = "screen-capture"))]
pub(crate) type PlatformScreenCaptureFrame = core_video::image_buffer::CVImageBuffer;

use crate::{
    Action, AnyWindowHandle, App, AsyncWindowContext, BackgroundExecutor, Bounds,
    DEFAULT_WINDOW_SIZE, DevicePixels, DispatchEventResult, Font, FontId, FontMetrics, FontRun,
    ForegroundExecutor, GlyphId, GpuSpecs, Hsla, ImageSource, Keymap, LineLayout, Pixels,
    PlatformInput, Point, Priority, RenderGlyphParams, RenderImage, RenderImageParams,
    RenderSvgParams, Scene, ShapedGlyph, ShapedRun, SharedString, Size, SvgRenderer,
    SystemWindowTab, Task, Window, WindowControlArea, hash, point, px, size,
};
use anyhow::Result;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
use anyhow::bail;
use async_task::Runnable;
use futures::channel::oneshot;
#[cfg(any(test, feature = "test-support"))]
use image::RgbaImage;
use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder as _, Frame};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use scheduler::Instant;
pub use scheduler::RunnableMeta;
use schemars::JsonSchema;
use seahash::SeaHasher;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::ops;
use std::time::Duration;
use std::{
    fmt::{self, Debug},
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};
use strum::EnumIter;
use uuid::Uuid;

pub use app_menu::*;
pub use keyboard::*;
pub use keystroke::*;

#[cfg(any(test, feature = "test-support"))]
pub(crate) use test::*;

#[cfg(any(test, feature = "test-support"))]
pub use test::{TestDispatcher, TestScreenCaptureSource, TestScreenCaptureStream};

#[cfg(any(test, feature = "bench"))]
pub use bench_dispatcher::BenchDispatcher;

#[cfg(all(target_os = "macos", any(test, feature = "test-support")))]
pub use visual_test::VisualTestPlatform;

// TODO(jk): return an enum instead of a string
/// Return which compositor we're guessing we'll use.
/// Does not attempt to connect to the given compositor.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub fn guess_compositor() -> &'static str {
    if std::env::var_os("ZED_HEADLESS").is_some() {
        return "Headless";
    }

    #[cfg(feature = "wayland")]
    let wayland_display = std::env::var_os("WAYLAND_DISPLAY");
    #[cfg(not(feature = "wayland"))]
    let wayland_display: Option<std::ffi::OsString> = None;

    #[cfg(feature = "x11")]
    let x11_display = std::env::var_os("DISPLAY");
    #[cfg(not(feature = "x11"))]
    let x11_display: Option<std::ffi::OsString> = None;

    let use_wayland = wayland_display.is_some_and(|display| !display.is_empty());
    let use_x11 = x11_display.is_some_and(|display| !display.is_empty());

    if use_wayland {
        "Wayland"
    } else if use_x11 {
        "X11"
    } else {
        "Headless"
    }
}

#[expect(missing_docs)]
pub trait Platform: 'static {
    fn background_executor(&self) -> BackgroundExecutor;
    fn foreground_executor(&self) -> ForegroundExecutor;
    fn text_system(&self) -> Arc<dyn PlatformTextSystem>;

    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>);
    fn quit(&self);
    fn restart(&self, binary_path: Option<PathBuf>);
    fn activate(&self, ignoring_other_apps: bool);
    fn hide(&self);
    fn hide_other_apps(&self);
    fn unhide_other_apps(&self);

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>>;
    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>>;
    fn active_window(&self) -> Option<AnyWindowHandle>;
    fn window_stack(&self) -> Option<Vec<AnyWindowHandle>> {
        None
    }

    fn is_screen_capture_supported(&self) -> bool {
        false
    }

    fn screen_capture_sources(
        &self,
    ) -> oneshot::Receiver<anyhow::Result<Vec<Rc<dyn ScreenCaptureSource>>>> {
        let (sources_tx, sources_rx) = oneshot::channel();
        sources_tx
            .send(Err(anyhow::anyhow!(
                "gpui was compiled without the screen-capture feature"
            )))
            .ok();
        sources_rx
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>>;

    /// Returns the appearance of the application's windows.
    fn window_appearance(&self) -> WindowAppearance;

    /// Returns the window button layout configuration when supported.
    fn button_layout(&self) -> Option<WindowButtonLayout> {
        None
    }

    fn open_url(&self, url: &str);
    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>);
    fn register_url_scheme(&self, url: &str) -> Task<Result<()>>;

    fn prompt_for_paths(
        &self,
        options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>>;
    fn prompt_for_new_path(
        &self,
        directory: &Path,
        suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>>;
    fn can_select_mixed_files_and_dirs(&self) -> bool;
    fn reveal_path(&self, path: &Path);
    fn open_with_system(&self, path: &Path);

    fn on_quit(&self, callback: Box<dyn FnMut()>);
    fn on_reopen(&self, callback: Box<dyn FnMut()>);

    fn set_menus(&self, menus: Vec<Menu>, keymap: &Keymap);
    fn get_menus(&self) -> Option<Vec<OwnedMenu>> {
        None
    }

    fn set_dock_menu(&self, menu: Vec<MenuItem>, keymap: &Keymap);
    fn perform_dock_menu_action(&self, _action: usize) {}
    fn add_recent_document(&self, _path: &Path) {}
    fn update_jump_list(
        &self,
        _menus: Vec<MenuItem>,
        _entries: Vec<SmallVec<[PathBuf; 2]>>,
    ) -> Task<Vec<SmallVec<[PathBuf; 2]>>> {
        Task::ready(Vec::new())
    }
    fn on_app_menu_action(&self, callback: Box<dyn FnMut(&dyn Action)>);
    fn on_will_open_app_menu(&self, callback: Box<dyn FnMut()>);
    fn on_validate_app_menu_command(&self, callback: Box<dyn FnMut(&dyn Action) -> bool>);

    fn thermal_state(&self) -> ThermalState;
    fn on_thermal_state_change(&self, callback: Box<dyn FnMut()>);

    fn compositor_name(&self) -> &'static str {
        ""
    }
    fn app_path(&self) -> Result<PathBuf>;
    fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf>;

    fn set_cursor_style(&self, style: CursorStyle);

    /// Hides the mouse cursor until the user moves the mouse over one of
    /// this application's windows.
    fn hide_cursor_until_mouse_moves(&self);

    /// Returns whether the mouse cursor is currently visible.
    fn is_cursor_visible(&self) -> bool;

    fn should_auto_hide_scrollbars(&self) -> bool;

    fn read_from_clipboard(&self) -> Option<ClipboardItem>;
    fn write_to_clipboard(&self, item: ClipboardItem);

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn read_from_primary(&self) -> Option<ClipboardItem>;
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn write_to_primary(&self, item: ClipboardItem);

    #[cfg(target_os = "macos")]
    fn read_from_find_pasteboard(&self) -> Option<ClipboardItem>;
    #[cfg(target_os = "macos")]
    fn write_to_find_pasteboard(&self, item: ClipboardItem);

    fn write_credentials(&self, url: &str, username: &str, password: &[u8]) -> Task<Result<()>>;
    fn read_credentials(&self, url: &str) -> Task<Result<Option<(String, Vec<u8>)>>>;
    fn delete_credentials(&self, url: &str) -> Task<Result<()>>;

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout>;
    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper>;
    fn on_keyboard_layout_change(&self, callback: Box<dyn FnMut()>);
}

/// A handle to a platform's display, e.g. a monitor or laptop screen.
pub trait PlatformDisplay: Debug {
    /// Get the ID for this display
    fn id(&self) -> DisplayId;

    /// Returns a stable identifier for this display that can be persisted and used
    /// across system restarts.
    fn uuid(&self) -> Result<Uuid>;

    /// Get the bounds for this display
    fn bounds(&self) -> Bounds<Pixels>;

    /// Get the visible bounds for this display, excluding taskbar/dock areas.
    /// This is the usable area where windows can be placed without being obscured.
    /// Defaults to the full display bounds if not overridden.
    fn visible_bounds(&self) -> Bounds<Pixels> {
        self.bounds()
    }

    /// Get the default bounds for this display to place a window
    fn default_bounds(&self) -> Bounds<Pixels> {
        let bounds = self.bounds();
        let center = bounds.center();
        let clipped_window_size = DEFAULT_WINDOW_SIZE.min(&bounds.size);

        let offset = clipped_window_size / 2.0;
        let origin = point(center.x - offset.width, center.y - offset.height);
        Bounds::new(origin, clipped_window_size)
    }
}

/// Thermal state of the system
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalState {
    /// System has no thermal constraints
    Nominal,
    /// System is slightly constrained, reduce discretionary work
    Fair,
    /// System is moderately constrained, reduce CPU/GPU intensive work
    Serious,
    /// System is critically constrained, minimize all resource usage
    Critical,
}

/// Metadata for a given [ScreenCaptureSource]
#[derive(Clone)]
pub struct SourceMetadata {
    /// Opaque identifier of this screen.
    pub id: u64,
    /// Human-readable label for this source.
    pub label: Option<SharedString>,
    /// Whether this source is the main display.
    pub is_main: Option<bool>,
    /// Video resolution of this source.
    pub resolution: Size<DevicePixels>,
}

/// A source of on-screen video content that can be captured.
pub trait ScreenCaptureSource {
    /// Returns metadata for this source.
    fn metadata(&self) -> Result<SourceMetadata>;

    /// Start capture video from this source, invoking the given callback
    /// with each frame.
    fn stream(
        &self,
        foreground_executor: &ForegroundExecutor,
        frame_callback: Box<dyn Fn(ScreenCaptureFrame) + Send>,
    ) -> oneshot::Receiver<Result<Box<dyn ScreenCaptureStream>>>;
}

/// A video stream captured from a screen.
pub trait ScreenCaptureStream {
    /// Returns metadata for this source.
    fn metadata(&self) -> Result<SourceMetadata>;
}

/// A frame of video captured from a screen.
pub struct ScreenCaptureFrame(pub PlatformScreenCaptureFrame);

/// An opaque identifier for a hardware display
#[derive(PartialEq, Eq, Hash, Copy, Clone)]
pub struct DisplayId(pub(crate) u64);

impl DisplayId {
    /// Create a new `DisplayId` from a raw platform display identifier.
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

impl From<u64> for DisplayId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<DisplayId> for u64 {
    fn from(id: DisplayId) -> Self {
        id.0
    }
}

impl Debug for DisplayId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DisplayId({})", self.0)
    }
}

/// Which part of the window to resize
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeEdge {
    /// The top edge
    Top,
    /// The top right corner
    TopRight,
    /// The right edge
    Right,
    /// The bottom right corner
    BottomRight,
    /// The bottom edge
    Bottom,
    /// The bottom left corner
    BottomLeft,
    /// The left edge
    Left,
    /// The top left corner
    TopLeft,
}

/// A type to describe the appearance of a window
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Default)]
pub enum WindowDecorations {
    #[default]
    /// Server side decorations
    Server,
    /// Client side decorations
    Client,
}

/// A type to describe how this window is currently configured
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Default)]
pub enum Decorations {
    /// The window is configured to use server side decorations
    #[default]
    Server,
    /// The window is configured to use client side decorations
    Client {
        /// The edge tiling state
        tiling: Tiling,
    },
}

/// What window controls this platform supports
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
pub struct WindowControls {
    /// Whether this platform supports fullscreen
    pub fullscreen: bool,
    /// Whether this platform supports maximize
    pub maximize: bool,
    /// Whether this platform supports minimize
    pub minimize: bool,
    /// Whether this platform supports a window menu
    pub window_menu: bool,
}

impl Default for WindowControls {
    fn default() -> Self {
        // Assume that we can do anything, unless told otherwise
        Self {
            fullscreen: true,
            maximize: true,
            minimize: true,
            window_menu: true,
        }
    }
}

/// A window control button type used in [`WindowButtonLayout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WindowButton {
    /// The minimize button
    Minimize,
    /// The maximize button
    Maximize,
    /// The close button
    Close,
}

impl WindowButton {
    /// Returns a stable element ID for rendering this button.
    pub fn id(&self) -> &'static str {
        match self {
            WindowButton::Minimize => "minimize",
            WindowButton::Maximize => "maximize",
            WindowButton::Close => "close",
        }
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn index(&self) -> usize {
        match self {
            WindowButton::Minimize => 0,
            WindowButton::Maximize => 1,
            WindowButton::Close => 2,
        }
    }
}

/// Maximum number of [`WindowButton`]s per side in the titlebar.
pub const MAX_BUTTONS_PER_SIDE: usize = 3;

/// Describes which [`WindowButton`]s appear on each side of the titlebar.
///
/// On Linux, this is read from the desktop environment's configuration
/// (e.g. GNOME's `gtk-decoration-layout` gsetting) via [`WindowButtonLayout::parse`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowButtonLayout {
    /// Buttons on the left side of the titlebar.
    pub left: [Option<WindowButton>; MAX_BUTTONS_PER_SIDE],
    /// Buttons on the right side of the titlebar.
    pub right: [Option<WindowButton>; MAX_BUTTONS_PER_SIDE],
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
impl WindowButtonLayout {
    /// Returns Zed's built-in fallback button layout for Linux titlebars.
    pub fn linux_default() -> Self {
        Self {
            left: [None; MAX_BUTTONS_PER_SIDE],
            right: [
                Some(WindowButton::Minimize),
                Some(WindowButton::Maximize),
                Some(WindowButton::Close),
            ],
        }
    }

    /// Parses a GNOME-style `button-layout` string (e.g. `"close,minimize:maximize"`).
    pub fn parse(layout_string: &str) -> Result<Self> {
        fn parse_side(
            s: &str,
            seen_buttons: &mut [bool; MAX_BUTTONS_PER_SIDE],
            unrecognized: &mut Vec<String>,
        ) -> [Option<WindowButton>; MAX_BUTTONS_PER_SIDE] {
            let mut result = [None; MAX_BUTTONS_PER_SIDE];
            let mut i = 0;
            for name in s.split(',') {
                let trimmed = name.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let button = match trimmed {
                    "minimize" => Some(WindowButton::Minimize),
                    "maximize" => Some(WindowButton::Maximize),
                    "close" => Some(WindowButton::Close),
                    other => {
                        unrecognized.push(other.to_string());
                        None
                    }
                };
                if let Some(button) = button {
                    if seen_buttons[button.index()] {
                        continue;
                    }
                    if let Some(slot) = result.get_mut(i) {
                        *slot = Some(button);
                        seen_buttons[button.index()] = true;
                        i += 1;
                    }
                }
            }
            result
        }

        let (left_str, right_str) = layout_string.split_once(':').unwrap_or(("", layout_string));
        let mut unrecognized = Vec::new();
        let mut seen_buttons = [false; MAX_BUTTONS_PER_SIDE];
        let layout = Self {
            left: parse_side(left_str, &mut seen_buttons, &mut unrecognized),
            right: parse_side(right_str, &mut seen_buttons, &mut unrecognized),
        };

        if !unrecognized.is_empty()
            && layout.left.iter().all(Option::is_none)
            && layout.right.iter().all(Option::is_none)
        {
            bail!(
                "button layout string {:?} contains no valid buttons (unrecognized: {})",
                layout_string,
                unrecognized.join(", ")
            );
        }

        Ok(layout)
    }

    /// Formats the layout back into a GNOME-style `button-layout` string.
    #[cfg(test)]
    pub fn format(&self) -> String {
        fn format_side(buttons: &[Option<WindowButton>; MAX_BUTTONS_PER_SIDE]) -> String {
            buttons
                .iter()
                .flatten()
                .map(|button| match button {
                    WindowButton::Minimize => "minimize",
                    WindowButton::Maximize => "maximize",
                    WindowButton::Close => "close",
                })
                .collect::<Vec<_>>()
                .join(",")
        }

        format!("{}:{}", format_side(&self.left), format_side(&self.right))
    }
}

/// A type to describe which sides of the window are currently tiled in some way
#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Default)]
pub struct Tiling {
    /// Whether the top edge is tiled
    pub top: bool,
    /// Whether the left edge is tiled
    pub left: bool,
    /// Whether the right edge is tiled
    pub right: bool,
    /// Whether the bottom edge is tiled
    pub bottom: bool,
}

impl Tiling {
    /// Initializes a [`Tiling`] type with all sides tiled
    pub fn tiled() -> Self {
        Self {
            top: true,
            left: true,
            right: true,
            bottom: true,
        }
    }

    /// Whether any edge is tiled
    pub fn is_tiled(&self) -> bool {
        self.top || self.left || self.right || self.bottom
    }
}

/// Callbacks for the accessibility adapter.
pub struct A11yCallbacks {
    /// Called when the adapter is activated (a screen reader connects).
    pub activation: Box<dyn Fn() -> Option<accesskit::TreeUpdate> + Send + 'static>,
    /// Called when an action is requested by the screen reader.
    pub action: Box<dyn Fn(accesskit::ActionRequest) + Send + 'static>,
    /// Called when the adapter is deactivated (screen reader disconnects).
    pub deactivation: Box<dyn Fn() + Send + 'static>,
}

#[derive(Debug, Clone, Eq, PartialEq, Default)]
#[expect(missing_docs)]
pub struct RequestFrameOptions {
    /// Platform BeginFrame metadata for this frame, when the platform frame source provides it.
    pub begin_frame: Option<BeginFrameArgs>,
    /// Whether this request came directly from the platform BeginFrame source.
    pub source_begin_frame: bool,
    /// Whether a presentation is required.
    pub require_presentation: bool,
    /// Force refresh of all rendering states when true.
    pub force_render: bool,
    /// Predicted display time for this frame, when the platform frame source provides it.
    pub predicted_display_time: Option<Instant>,
    /// Platform-provided frame interval, when known.
    pub frame_interval: Option<Duration>,
    /// Deadline for producing this frame, when known.
    pub frame_deadline: Option<Instant>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash)]
#[expect(missing_docs)]
pub struct BeginFrameId {
    pub source_id: u64,
    pub sequence_number: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
#[expect(missing_docs)]
pub struct PossibleDeadline {
    pub vsync_id: i64,
    pub latch_delta: Duration,
    pub present_delta: Duration,
}

#[derive(Debug, Clone, Eq, PartialEq)]
#[expect(missing_docs)]
pub struct PossibleDeadlines {
    pub os_preferred_index: usize,
    pub deadlines: Vec<PossibleDeadline>,
}

impl PossibleDeadlines {
    /// Returns the platform-preferred deadline candidate.
    pub fn os_preferred_deadline(&self) -> Option<&PossibleDeadline> {
        self.deadlines.get(self.os_preferred_index)
    }
}

/// The latency threshold above which input-to-pixel delay becomes perceptible.
///
/// Mirrors `FrameDeadlineDecider::kPerceptibleLatencyThreshold`
/// (`frame_deadline_decider.h:18`).
const PERCEPTIBLE_LATENCY_THRESHOLD: Duration = Duration::from_millis(100);

/// Selects the best deadline from a [`PossibleDeadlines`] set.
///
/// Ported from `viz::FrameDeadlineDecider`
/// (`components/viz/service/display/frame_deadline_decider.h/.cc`).
///
/// The decider implements three behaviors in `select_deadline`:
/// 1. **Platform-preferred short-circuit** — when `use_platform_preferred_deadlines`
///    is true, always return the OS-preferred index (`frame_deadline_decider.cc:58`).
/// 2. **In-sequence stickiness** (`find_closest_deadline_by_presentation`) — once a
///    sequence starts with a target present delta, subsequent frames lock to the
///    closest matching deadline until `on_go_idle` resets the sequence
///    (`frame_deadline_decider.cc:63`).
/// 3. **Input-aware perceptible-latency cap** — when `earliest_input_time` is known,
///    clamp the target present delta so total input-to-pixel latency stays below
///    [`PERCEPTIBLE_LATENCY_THRESHOLD`] (`frame_deadline_decider.cc:84-100`).
#[derive(Debug, Clone)]
pub struct FrameDeadlineDecider {
    in_frame_sequence: bool,
    curr_sequence_present_delta: Duration,
    curr_sequence_deadline_index: usize,
    use_platform_preferred_deadlines: bool,
}

impl FrameDeadlineDecider {
    /// Creates a decider. When `use_platform_preferred_deadlines` is true,
    /// `select_deadline` always returns the OS-preferred index.
    pub fn new(use_platform_preferred_deadlines: bool) -> Self {
        Self {
            in_frame_sequence: false,
            curr_sequence_present_delta: Duration::ZERO,
            curr_sequence_deadline_index: 0,
            use_platform_preferred_deadlines,
        }
    }

    /// Selects the deadline index. Mirrors `SelectDeadline`
    /// (`frame_deadline_decider.cc:24`).
    pub fn select_deadline(
        &mut self,
        possible_deadlines: &PossibleDeadlines,
        vsync_interval: Duration,
        max_allowed_buffers: u32,
        frame_time: Instant,
        earliest_input_time: Option<Instant>,
    ) -> usize {
        let deadlines = &possible_deadlines.deadlines;
        assert!(!deadlines.is_empty());

        if self.use_platform_preferred_deadlines {
            self.record_sequence_state(possible_deadlines, possible_deadlines.os_preferred_index);
            return possible_deadlines.os_preferred_index;
        }

        if self.in_frame_sequence {
            let index = self.find_closest_deadline_by_presentation(possible_deadlines);
            self.record_sequence_state(possible_deadlines, index);
            return index;
        }

        self.in_frame_sequence = true;

        // target_present_delta = max_allowed_buffers * vsync_interval
        // (no presentation_offset on non-Android; `frame_deadline_decider.cc:79`).
        let mut target_present_delta = vsync_interval.saturating_mul(max_allowed_buffers);

        // Input-aware perceptible-latency cap (`frame_deadline_decider.cc:84-100`).
        if let Some(earliest_input) = earliest_input_time {
            let input_delta = frame_time.saturating_duration_since(earliest_input);
            let latency_cap = PERCEPTIBLE_LATENCY_THRESHOLD
                .saturating_sub(vsync_interval)
                .saturating_sub(vsync_interval / 4);
            let max_present_delta = latency_cap.saturating_sub(input_delta);
            if max_present_delta < target_present_delta {
                target_present_delta = max_present_delta;
            }
        }

        // Binary search for the last deadline with present_delta <= target
        // (C++ `std::upper_bound` then decrement; `frame_deadline_decider.cc:102-114`).
        let upper_bound = deadlines.partition_point(|d| d.present_delta <= target_present_delta);
        let chrome_preferred_index = upper_bound.saturating_sub(1).min(deadlines.len() - 1);
        let chrome_preferred_deadline = &deadlines[chrome_preferred_index];

        // Fallback: Chrome preferred exceeds target → use OS preferred
        // (`frame_deadline_decider.cc:117-119`).
        if chrome_preferred_deadline.present_delta > target_present_delta {
            let result = possible_deadlines.os_preferred_index;
            self.record_sequence_state(possible_deadlines, result);
            return result;
        }

        // Fallback: Chrome preferred is below OS preferred → don't reduce below OS
        // (`frame_deadline_decider.cc:122-127`).
        if let Some(os_preferred) = possible_deadlines.os_preferred_deadline()
            && chrome_preferred_deadline.present_delta < os_preferred.present_delta
        {
            let result = possible_deadlines.os_preferred_index;
            self.record_sequence_state(possible_deadlines, result);
            return result;
        }

        self.record_sequence_state(possible_deadlines, chrome_preferred_index);
        chrome_preferred_index
    }

    /// Records the selected deadline's present delta and index for in-sequence
    /// stickiness. Mirrors the `absl::Cleanup` in `SelectDeadline`
    /// (`frame_deadline_decider.cc:40-56`).
    fn record_sequence_state(&mut self, possible_deadlines: &PossibleDeadlines, index: usize) {
        self.curr_sequence_deadline_index = index;
        if let Some(deadline) = possible_deadlines.deadlines.get(index) {
            self.curr_sequence_present_delta = deadline.present_delta;
        }
    }

    /// Resets sequence state. Mirrors `OnGoIdle`
    /// (`frame_deadline_decider.cc:134`).
    pub fn on_go_idle(&mut self) {
        self.in_frame_sequence = false;
        self.curr_sequence_present_delta = Duration::ZERO;
        self.curr_sequence_deadline_index = 0;
    }

    /// Finds the deadline closest to the sequence's current present delta.
    /// Mirrors `FindClosestDeadlineByPresentation`
    /// (`frame_deadline_decider.cc:142`).
    fn find_closest_deadline_by_presentation(
        &self,
        possible_deadlines: &PossibleDeadlines,
    ) -> usize {
        let deadlines = &possible_deadlines.deadlines;

        if self.curr_sequence_deadline_index < deadlines.len() {
            let cached = &deadlines[self.curr_sequence_deadline_index];
            let diff = cached
                .present_delta
                .abs_diff(self.curr_sequence_present_delta);
            if diff <= Duration::from_millis(1) {
                return self.curr_sequence_deadline_index;
            }
        }

        let mut best_index = 0;
        let mut min_diff = deadlines[0]
            .present_delta
            .abs_diff(self.curr_sequence_present_delta);
        for (i, deadline) in deadlines.iter().enumerate().skip(1) {
            let diff = deadline
                .present_delta
                .abs_diff(self.curr_sequence_present_delta);
            if diff < min_diff {
                min_diff = diff;
                best_index = i;
            }
        }
        best_index
    }
}

/// Derives the per-deadline buffer cap from the selected `present_delta`.
///
/// Mirrors `DisplayScheduler::MaxPendingSwapsForDeadline`
/// (`display_scheduler.cc:479`). The `0.8` constant biases rounding up so the
/// buffer count covers frames whose present time is not an exact multiple of the
/// interval.
pub fn max_pending_swaps_for_deadline(present_delta: Duration, interval: Duration) -> u32 {
    if interval.is_zero() {
        return 1;
    }
    let total_ns = present_delta.as_nanos() as f64;
    let interval_ns = interval.as_nanos() as f64;
    ((total_ns + 0.8 * interval_ns) / interval_ns) as u32
}

/// Optional per-refresh-rate swap caps, mirroring `viz::PendingSwapParams`
/// (`components/viz/service/display/pending_swap_params.h`). Each field, when
/// set, overrides `max_pending_swaps` at that refresh-rate tier.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct RefreshRateSwapCaps {
    /// The default cap, used when no tier-specific cap applies.
    pub max_pending_swaps: u32,
    /// Overrides for ≥120 Hz. `None` falls back to `max_pending_swaps`.
    pub max_pending_swaps_120hz: Option<u32>,
    /// Overrides for ≥90 Hz. `None` falls back to `max_pending_swaps`.
    pub max_pending_swaps_90hz: Option<u32>,
    /// Overrides for ≥72 Hz. `None` falls back to `max_pending_swaps`.
    pub max_pending_swaps_72hz: Option<u32>,
}

/// Selects the pending-swap cap based on the current refresh-rate interval.
///
/// Ports `DisplayScheduler::MaxPendingSwapsForRefreshRate`
/// (`display_scheduler.cc:455`). The thresholds use the same margins as
/// Chromium: 14ms (72Hz), 11.5ms (90Hz), 8.5ms (120Hz).
pub fn max_pending_swaps_for_refresh_rate(interval: Duration, caps: &RefreshRateSwapCaps) -> u32 {
    const HZ_72_INTERVAL: Duration = Duration::from_micros(14000);
    const HZ_90_INTERVAL: Duration = Duration::from_micros(11500);
    const HZ_120_INTERVAL: Duration = Duration::from_micros(8500);

    if interval < HZ_120_INTERVAL {
        caps.max_pending_swaps_120hz
            .unwrap_or(caps.max_pending_swaps)
    } else if interval < HZ_90_INTERVAL {
        caps.max_pending_swaps_90hz
            .unwrap_or(caps.max_pending_swaps)
    } else if interval < HZ_72_INTERVAL {
        caps.max_pending_swaps_72hz
            .unwrap_or(caps.max_pending_swaps)
    } else {
        caps.max_pending_swaps
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
#[expect(missing_docs)]
pub struct BeginFrameArgs {
    pub id: BeginFrameId,
    pub frame_time: Instant,
    pub deadline: Instant,
    pub interval: Duration,
    pub missed: bool,
    pub possible_deadlines: Option<PossibleDeadlines>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[expect(missing_docs)]
pub struct BeginFrameAck {
    pub frame_id: BeginFrameId,
    pub frame_time: Instant,
    pub has_damage: bool,
}

#[expect(missing_docs)]
pub trait BeginFrameObserver {
    fn on_begin_frame(&mut self, args: BeginFrameArgs) -> bool;
    fn last_used_begin_frame(&self) -> Option<BeginFrameArgs>;
    fn on_begin_frame_source_paused_changed(&mut self, _paused: bool) {}
    fn wants_animate_only_begin_frames(&self) -> bool {
        false
    }
}

#[expect(missing_docs)]
pub trait BeginFrameSource {
    type ObserverId: Copy + Eq;

    fn add_observer(&mut self, observer_id: Self::ObserverId);
    fn remove_observer(&mut self, observer_id: Self::ObserverId);
    fn did_finish_frame(&mut self, observer_id: Self::ObserverId, ack: Option<BeginFrameAck>);
}

/// Tracks per-observer BeginFrame continuity.
///
/// This is the GPUI equivalent of Chromium's `BeginFrameObserverBase` state:
/// observers record the last BeginFrame they actually used, and the source or
/// observer can reject duplicated or non-forward BeginFrames before they reach
/// scheduler/input logic.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct BeginFrameObserverState {
    last_used_begin_frame: Option<BeginFrameArgs>,
    dropped_begin_frame_args: u64,
}

impl BeginFrameObserverState {
    /// Returns whether `begin_frame` is forward-continuous with the observer's
    /// last used BeginFrame. This mirrors Chromium's
    /// `CheckBeginFrameContinuity`.
    pub fn should_issue_begin_frame(
        &self,
        begin_frame: &BeginFrameArgs,
        allow_missed_retry_for_current_frame: bool,
    ) -> bool {
        begin_frame_follows_last_used(
            begin_frame,
            self.last_used_begin_frame.as_ref(),
            allow_missed_retry_for_current_frame,
        )
    }

    /// Records whether an observer used or dropped a delivered BeginFrame.
    pub fn record_begin_frame_result(&mut self, begin_frame: BeginFrameArgs, used: bool) {
        if used {
            self.last_used_begin_frame = Some(begin_frame);
        } else {
            self.dropped_begin_frame_args = self.dropped_begin_frame_args.saturating_add(1);
        }
    }

    /// Returns the last BeginFrame used by the observer.
    pub fn last_used_begin_frame(&self) -> Option<BeginFrameArgs> {
        self.last_used_begin_frame.clone()
    }

    /// Returns the count of delivered BeginFrames the observer dropped.
    pub fn dropped_begin_frame_args(&self) -> u64 {
        self.dropped_begin_frame_args
    }
}

/// Returns whether `begin_frame` is newer than an observer's last used
/// BeginFrame.
pub fn begin_frame_follows_last_used(
    begin_frame: &BeginFrameArgs,
    last_used_begin_frame: Option<&BeginFrameArgs>,
    allow_missed_retry_for_current_frame: bool,
) -> bool {
    let Some(last_used_begin_frame) = last_used_begin_frame else {
        return true;
    };

    if allow_missed_retry_for_current_frame
        && begin_frame.missed
        && begin_frame.id == last_used_begin_frame.id
    {
        return true;
    }

    begin_frame.frame_time > last_used_begin_frame.frame_time
        && (begin_frame.id.source_id != last_used_begin_frame.id.source_id
            || begin_frame.id.sequence_number > last_used_begin_frame.id.sequence_number)
}

/// Returns whether `begin_frame` is newer than an observer's last completed
/// BeginFrame ack.
pub fn begin_frame_follows_ack(
    begin_frame: &BeginFrameArgs,
    last_ack: Option<BeginFrameAck>,
) -> bool {
    let Some(last_ack) = last_ack else {
        return true;
    };

    begin_frame.frame_time > last_ack.frame_time
        && (begin_frame.id.source_id != last_ack.frame_id.source_id
            || begin_frame.id.sequence_number > last_ack.frame_id.sequence_number)
}

/// Identifies a subscriber to a platform window's [`BeginFrameSource`].
///
/// Mirrors the observer list on a `viz::BeginFrameSource`
/// (`components/viz/common/frame_sinks/begin_frame_source.h`): the source issues
/// [`BeginFrameArgs`] to the input observer before the scheduler observer. The
/// two delivery paths are `IssueBeginFrameToInputClient` /
/// `IssueBeginFrameToSchedulerClient` (`begin_frame_source.cc:238-245`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BeginFrameObserverKind {
    /// The scheduler client. Consumes full frame requests — source begin frames,
    /// missed-frame replays, and scheduler-driven reschedules.
    Scheduler,
    /// The input client. Consumes source-driven [`BeginFrameArgs`] for input-rate
    /// tracking (`IssueBeginFrameToInputClient`).
    Input,
}

/// The dispatch closure a begin-frame observer uses to receive frames. The
/// variant carries the [`BeginFrameObserverKind`] it is registered for.
pub enum BeginFrameObserverDispatch {
    /// Scheduler observer dispatch (`IssueBeginFrameToSchedulerClient`). Broader
    /// than a single begin frame: GPUI routes missed-frame replays, resize, and
    /// GPU-available wakeups through the same path.
    Scheduler(Box<dyn FnMut(RequestFrameOptions)>),
    /// Input observer dispatch (`IssueBeginFrameToInputClient`).
    Input(Box<dyn FnMut(BeginFrameArgs)>),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[expect(missing_docs)]
pub struct PresentationFeedback {
    pub ready_time: Instant,
    pub latch_time: Instant,
    pub display_time: Instant,
    pub target_latch_time: Option<Instant>,
    pub interval: Option<Duration>,
    pub presented: bool,
    pub vsync: bool,
    pub hardware_completion: bool,
    /// Monotonically increasing swap id stamped by the presenter, analog of
    /// `display.h` `swap_n`. Used by the scheduler to match feedback to the
    /// correct pending presentation group (`display.cc:802-812`). `None` on
    /// platforms that do not stamp a swap id.
    pub swap_id: Option<u64>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[expect(missing_docs)]
pub struct SwapCompletionFeedback {
    pub ready_time: Instant,
    pub latch_time: Instant,
    pub result: SwapCompletionResult,
    pub presented: bool,
    /// Swap id matching [`PresentationFeedback::swap_id`].
    pub swap_id: Option<u64>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[expect(missing_docs)]
pub enum SwapCompletionResult {
    Ack,
    Skipped,
    Failed,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[expect(missing_docs)]
pub enum PlatformDrawResult {
    /// The frame was submitted for presentation. The inner value is the swap id
    /// stamped by the presenter (analog of `display.h` `swap_n`), or `None` on
    /// platforms that do not stamp one.
    Submitted(Option<u64>),
    Deferred,
    Skipped,
}

#[expect(missing_docs)]
pub trait PlatformWindow: HasWindowHandle + HasDisplayHandle {
    fn bounds(&self) -> Bounds<Pixels>;
    fn is_maximized(&self) -> bool;
    fn window_bounds(&self) -> WindowBounds;
    fn content_size(&self) -> Size<Pixels>;
    fn resize(&mut self, size: Size<Pixels>);
    fn scale_factor(&self) -> f32;
    fn appearance(&self) -> WindowAppearance;
    fn display(&self) -> Option<Rc<dyn PlatformDisplay>>;
    fn mouse_position(&self) -> Point<Pixels>;
    fn modifiers(&self) -> Modifiers;
    fn capslock(&self) -> Capslock;
    fn set_input_handler(&mut self, input_handler: PlatformInputHandler);
    fn take_input_handler(&mut self) -> Option<PlatformInputHandler>;
    fn prompt(
        &self,
        level: PromptLevel,
        msg: &str,
        detail: Option<&str>,
        answers: &[PromptButton],
    ) -> Option<oneshot::Receiver<usize>>;
    fn activate(&self);
    fn is_active(&self) -> bool;
    fn is_visible(&self) -> bool {
        true
    }
    fn is_hovered(&self) -> bool;
    fn background_appearance(&self) -> WindowBackgroundAppearance;
    fn set_title(&mut self, title: &str);
    fn set_background_appearance(&self, background_appearance: WindowBackgroundAppearance);
    fn minimize(&self);
    fn zoom(&self);
    fn toggle_fullscreen(&self);
    fn is_fullscreen(&self) -> bool;
    /// Installs the dispatch closure for a begin-frame observer. The dispatch
    /// variant carries the observer's [`BeginFrameObserverKind`]; the scheduler
    /// observer is toggled on/off with [`Self::add_begin_frame_observer`] /
    /// [`Self::remove_begin_frame_observer`], while the input observer is
    /// registered implicitly by installing its dispatch (it stays subscribed for
    /// the window's lifetime).
    ///
    /// Mirrors wiring a `BeginFrameObserver` onto a `viz::BeginFrameSource`
    /// (`components/viz/common/frame_sinks/begin_frame_source.h`): the source
    /// issues [`BeginFrameArgs`] to the input client before the scheduler client
    /// (`IssueBeginFrameToInputClient` / `IssueBeginFrameToSchedulerClient`,
    /// `begin_frame_source.cc:238-245`). On platforms without a structured
    /// begin-frame source the scheduler variant is the window's frame callback
    /// and the input variant is ignored.
    fn set_begin_frame_observer(&self, dispatch: BeginFrameObserverDispatch);
    /// Adds an observer of `kind` to the begin-frame source. On registering the
    /// scheduler observer the source replays any missed [`BeginFrameArgs`]
    /// (`viz::BeginFrameSource::AddObserver`, `begin_frame_source.h:228` /
    /// `:490`). Re-adding an already-registered observer is a no-op.
    ///
    /// Default no-op for platforms without a structured begin-frame source.
    fn add_begin_frame_observer(&self, _kind: BeginFrameObserverKind) {}
    /// Removes an observer of `kind` from the begin-frame source
    /// (`viz::BeginFrameSource::RemoveObserver`, `begin_frame_source.h:229`).
    ///
    /// Default no-op for platforms without a structured begin-frame source.
    fn remove_begin_frame_observer(&self, _kind: BeginFrameObserverKind) {}
    fn request_frame(&self, _options: RequestFrameOptions) {}
    fn request_begin_frame(&self) {}
    fn supports_delayed_begin_frame_scheduling(&self) -> bool {
        true
    }
    fn supports_swap_completion_feedback(&self) -> bool {
        false
    }
    /// Returns the current pending-swap cap, optionally keyed to the display's
    /// refresh-rate interval (`MaxPendingSwapsForRefreshRate`,
    /// `display_scheduler.cc:455`). Platforms without structured swap completion
    /// return `None`.
    fn max_pending_swaps(&self, _interval: Option<Duration>) -> Option<u32> {
        None
    }
    fn on_swap_completion(&self, _callback: Box<dyn FnMut(SwapCompletionFeedback)>) {}
    fn on_presentation_feedback(&self, _callback: Box<dyn FnMut(PresentationFeedback)>) {}
    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>);
    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>);
    fn on_visibility_change(&self, _callback: Box<dyn FnMut(bool)>) {}
    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>);
    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>);
    fn on_moved(&self, callback: Box<dyn FnMut()>);
    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>);
    fn on_hit_test_window_control(&self, callback: Box<dyn FnMut() -> Option<WindowControlArea>>);
    fn on_close(&self, callback: Box<dyn FnOnce()>);
    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>);
    fn on_button_layout_changed(&self, _callback: Box<dyn FnMut()>) {}
    fn draw(&self, scene: &Scene) -> PlatformDrawResult;
    fn completed_frame(&self, _ack: Option<BeginFrameAck>) {}
    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas>;
    fn is_subpixel_rendering_supported(&self) -> bool;

    // macOS specific methods
    fn get_title(&self) -> String {
        String::new()
    }
    fn tabbed_windows(&self) -> Option<Vec<SystemWindowTab>> {
        None
    }
    fn tab_bar_visible(&self) -> bool {
        false
    }
    fn set_edited(&mut self, _edited: bool) {}
    fn set_document_path(&self, _path: Option<&std::path::Path>) {}
    #[cfg(target_os = "macos")]
    fn set_traffic_light_position(&self, _position: Point<Pixels>) {}
    fn show_character_palette(&self) {}
    fn titlebar_double_click(&self) {}
    fn on_move_tab_to_new_window(&self, _callback: Box<dyn FnMut()>) {}
    fn on_merge_all_windows(&self, _callback: Box<dyn FnMut()>) {}
    fn on_select_previous_tab(&self, _callback: Box<dyn FnMut()>) {}
    fn on_select_next_tab(&self, _callback: Box<dyn FnMut()>) {}
    fn on_toggle_tab_bar(&self, _callback: Box<dyn FnMut()>) {}
    fn merge_all_windows(&self) {}
    fn move_tab_to_new_window(&self) {}
    fn toggle_window_tab_overview(&self) {}
    fn set_tabbing_identifier(&self, _identifier: Option<String>) {}

    #[cfg(target_os = "windows")]
    fn get_raw_handle(&self) -> windows::Win32::Foundation::HWND;

    // Linux specific methods
    fn inner_window_bounds(&self) -> WindowBounds {
        self.window_bounds()
    }
    fn request_decorations(&self, _decorations: WindowDecorations) {}
    fn show_window_menu(&self, _position: Point<Pixels>) {}
    fn start_window_move(&self) {}
    fn start_window_resize(&self, _edge: ResizeEdge) {}
    fn window_decorations(&self) -> Decorations {
        Decorations::Server
    }
    fn set_app_id(&mut self, _app_id: &str) {}
    fn map_window(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
    fn window_controls(&self) -> WindowControls {
        WindowControls::default()
    }
    fn set_client_inset(&self, _inset: Pixels) {}
    fn gpu_specs(&self) -> Option<GpuSpecs>;

    fn update_ime_position(&self, _bounds: Bounds<Pixels>);

    fn play_system_bell(&self) {}

    /// Initialize the accessibility adapter with callbacks.
    fn a11y_init(&self, _callbacks: A11yCallbacks) {}

    /// Provide a TreeUpdate to the accessibility adapter.
    fn a11y_tree_update(&self, _tree_update: accesskit::TreeUpdate) {}

    /// Inform the adapter of updated window bounds.
    fn a11y_update_window_bounds(&self) {}

    #[cfg(any(test, feature = "test-support"))]
    fn as_test(&mut self) -> Option<&mut TestWindow> {
        None
    }

    /// Renders the given scene to a texture and returns the pixel data as an RGBA image.
    /// This does not present the frame to screen - useful for visual testing where we want
    /// to capture what would be rendered without displaying it or requiring the window to be visible.
    #[cfg(any(test, feature = "test-support"))]
    fn render_to_image(&self, _scene: &Scene) -> Result<RgbaImage> {
        anyhow::bail!("render_to_image not implemented for this platform")
    }
}

/// A renderer for headless windows that can produce real rendered output.
#[cfg(any(test, feature = "test-support"))]
pub trait PlatformHeadlessRenderer {
    /// Render a scene and return the result as an RGBA image.
    fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> Result<RgbaImage>;

    /// Render a scene to an offscreen target without reading the result back.
    ///
    /// This is the headless analogue of presenting a frame: it performs the
    /// same CPU-side scene encoding and GPU submission as drawing to a real
    /// window, but doesn't block on GPU completion or copy pixels back.
    fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> Result<()>;

    /// Returns the sprite atlas used by this renderer.
    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas>;
}

/// Type alias for runnables with metadata.
/// Previously an enum with a single variant, now simplified to a direct type alias.
#[doc(hidden)]
pub type RunnableVariant = Runnable<RunnableMeta>;

#[doc(hidden)]
pub type TimerResolutionGuard = gpui_util::Deferred<Box<dyn FnOnce() + Send>>;

#[doc(hidden)]
pub enum TasksIncluded {
    OnlyCompleted,
    CompletedAndRunning,
}

/// This type is public so that our test macro can generate and use it, but it should not
/// be considered part of our public API.
#[doc(hidden)]
pub trait PlatformDispatcher: Send + Sync {
    fn is_main_thread(&self) -> bool;
    fn dispatch(&self, runnable: RunnableVariant, priority: Priority);
    fn dispatch_on_main_thread(&self, runnable: RunnableVariant, priority: Priority);
    fn dispatch_after(&self, duration: Duration, runnable: RunnableVariant);

    fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>);

    fn now(&self) -> Instant {
        Instant::now()
    }

    fn increase_timer_resolution(&self) -> TimerResolutionGuard {
        gpui_util::defer(Box::new(|| {}))
    }

    #[cfg(any(test, feature = "test-support"))]
    fn as_test(&self) -> Option<&TestDispatcher> {
        None
    }

    // This cfg must match the `bench_dispatcher` module's, which implements
    // this method whenever it compiles.
    #[cfg(any(test, feature = "bench"))]
    fn as_bench(&self) -> Option<&BenchDispatcher> {
        None
    }
}

#[expect(missing_docs)]
pub trait PlatformTextSystem: Send + Sync {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()>;
    /// Get all available font names.
    fn all_font_names(&self) -> Vec<String>;
    /// Get the font ID for a font descriptor.
    fn font_id(&self, descriptor: &Font) -> Result<FontId>;
    /// Get metrics for a font.
    fn font_metrics(&self, font_id: FontId) -> FontMetrics;
    /// Get typographic bounds for a glyph.
    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Bounds<f32>>;
    /// Get the advance width for a glyph.
    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>>;
    /// Get the glyph ID for a character.
    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId>;
    /// Get raster bounds for a glyph.
    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>>;
    /// Rasterize a glyph.
    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)>;
    /// Layout a line of text with the given font runs.
    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout;
    /// Returns the recommended text rendering mode for the given font and size.
    fn recommended_rendering_mode(&self, _font_id: FontId, _font_size: Pixels)
    -> TextRenderingMode;
    /// Returns the dilation level to use for a glyph painted in the given color.
    fn glyph_dilation_for_color(&self, _color: Hsla) -> u8 {
        0
    }
}

#[expect(missing_docs)]
pub struct NoopTextSystem;

#[expect(missing_docs)]
impl NoopTextSystem {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self
    }
}

impl PlatformTextSystem for NoopTextSystem {
    fn add_fonts(&self, _fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        Ok(())
    }

    fn all_font_names(&self) -> Vec<String> {
        Vec::new()
    }

    fn font_id(&self, _descriptor: &Font) -> Result<FontId> {
        Ok(FontId(1))
    }

    fn font_metrics(&self, _font_id: FontId) -> FontMetrics {
        FontMetrics {
            units_per_em: 1000,
            ascent: 1025.0,
            descent: -275.0,
            line_gap: 0.0,
            underline_position: -95.0,
            underline_thickness: 60.0,
            cap_height: 698.0,
            x_height: 516.0,
            bounding_box: Bounds {
                origin: Point {
                    x: -260.0,
                    y: -245.0,
                },
                size: Size {
                    width: 1501.0,
                    height: 1364.0,
                },
            },
        }
    }

    fn typographic_bounds(&self, _font_id: FontId, _glyph_id: GlyphId) -> Result<Bounds<f32>> {
        Ok(Bounds {
            origin: Point { x: 54.0, y: 0.0 },
            size: size(392.0, 528.0),
        })
    }

    fn advance(&self, _font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        Ok(size(600.0 * glyph_id.0 as f32, 0.0))
    }

    fn glyph_for_char(&self, _font_id: FontId, ch: char) -> Option<GlyphId> {
        Some(GlyphId(ch.len_utf16() as u32))
    }

    fn glyph_raster_bounds(&self, _params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        Ok(Default::default())
    }

    fn rasterize_glyph(
        &self,
        _params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        Ok((raster_bounds.size, Vec::new()))
    }

    fn layout_line(&self, text: &str, font_size: Pixels, _runs: &[FontRun]) -> LineLayout {
        let mut position = px(0.);
        let metrics = self.font_metrics(FontId(0));
        let em_width = font_size
            * self
                .advance(FontId(0), self.glyph_for_char(FontId(0), 'm').unwrap())
                .unwrap()
                .width
            / metrics.units_per_em as f32;
        let mut glyphs = Vec::new();
        for (ix, c) in text.char_indices() {
            if let Some(glyph) = self.glyph_for_char(FontId(0), c) {
                glyphs.push(ShapedGlyph {
                    id: glyph,
                    position: point(position, px(0.)),
                    index: ix,
                    is_emoji: glyph.0 == 2,
                });
                if glyph.0 == 2 {
                    position += em_width * 2.0;
                } else {
                    position += em_width;
                }
            } else {
                position += em_width
            }
        }
        let mut runs = Vec::default();
        if !glyphs.is_empty() {
            runs.push(ShapedRun {
                font_id: FontId(0),
                glyphs,
            });
        } else {
            position = px(0.);
        }

        LineLayout {
            font_size,
            width: position,
            ascent: font_size * (metrics.ascent / metrics.units_per_em as f32),
            descent: font_size * (metrics.descent / metrics.units_per_em as f32),
            runs,
            len: text.len(),
        }
    }

    fn recommended_rendering_mode(
        &self,
        _font_id: FontId,
        _font_size: Pixels,
    ) -> TextRenderingMode {
        TextRenderingMode::Grayscale
    }
}

// Adapted from https://github.com/microsoft/terminal/blob/1283c0f5b99a2961673249fa77c6b986efb5086c/src/renderer/atlas/dwrite.cpp
// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.
/// Compute gamma correction ratios for subpixel text rendering.
#[allow(dead_code)]
pub fn get_gamma_correction_ratios(gamma: f32) -> [f32; 4] {
    const GAMMA_INCORRECT_TARGET_RATIOS: [[f32; 4]; 13] = [
        [0.0000 / 4.0, 0.0000 / 4.0, 0.0000 / 4.0, 0.0000 / 4.0], // gamma = 1.0
        [0.0166 / 4.0, -0.0807 / 4.0, 0.2227 / 4.0, -0.0751 / 4.0], // gamma = 1.1
        [0.0350 / 4.0, -0.1760 / 4.0, 0.4325 / 4.0, -0.1370 / 4.0], // gamma = 1.2
        [0.0543 / 4.0, -0.2821 / 4.0, 0.6302 / 4.0, -0.1876 / 4.0], // gamma = 1.3
        [0.0739 / 4.0, -0.3963 / 4.0, 0.8167 / 4.0, -0.2287 / 4.0], // gamma = 1.4
        [0.0933 / 4.0, -0.5161 / 4.0, 0.9926 / 4.0, -0.2616 / 4.0], // gamma = 1.5
        [0.1121 / 4.0, -0.6395 / 4.0, 1.1588 / 4.0, -0.2877 / 4.0], // gamma = 1.6
        [0.1300 / 4.0, -0.7649 / 4.0, 1.3159 / 4.0, -0.3080 / 4.0], // gamma = 1.7
        [0.1469 / 4.0, -0.8911 / 4.0, 1.4644 / 4.0, -0.3234 / 4.0], // gamma = 1.8
        [0.1627 / 4.0, -1.0170 / 4.0, 1.6051 / 4.0, -0.3347 / 4.0], // gamma = 1.9
        [0.1773 / 4.0, -1.1420 / 4.0, 1.7385 / 4.0, -0.3426 / 4.0], // gamma = 2.0
        [0.1908 / 4.0, -1.2652 / 4.0, 1.8650 / 4.0, -0.3476 / 4.0], // gamma = 2.1
        [0.2031 / 4.0, -1.3864 / 4.0, 1.9851 / 4.0, -0.3501 / 4.0], // gamma = 2.2
    ];

    const NORM13: f32 = ((0x10000 as f64) / (255.0 * 255.0) * 4.0) as f32;
    const NORM24: f32 = ((0x100 as f64) / (255.0) * 4.0) as f32;

    let index = ((gamma * 10.0).round() as usize).clamp(10, 22) - 10;
    let ratios = GAMMA_INCORRECT_TARGET_RATIOS[index];

    [
        ratios[0] * NORM13,
        ratios[1] * NORM24,
        ratios[2] * NORM13,
        ratios[3] * NORM24,
    ]
}

#[derive(PartialEq, Eq, Hash, Clone)]
#[expect(missing_docs)]
pub enum AtlasKey {
    Glyph(RenderGlyphParams),
    Svg(RenderSvgParams),
    Image(RenderImageParams),
}

impl AtlasKey {
    #[cfg_attr(
        all(
            any(target_os = "linux", target_os = "freebsd"),
            not(any(feature = "x11", feature = "wayland"))
        ),
        allow(dead_code)
    )]
    /// Returns the texture kind for this atlas key.
    pub fn texture_kind(&self) -> AtlasTextureKind {
        match self {
            AtlasKey::Glyph(params) => {
                if params.is_emoji {
                    AtlasTextureKind::Polychrome
                } else if params.subpixel_rendering {
                    AtlasTextureKind::Subpixel
                } else {
                    AtlasTextureKind::Monochrome
                }
            }
            AtlasKey::Svg(_) => AtlasTextureKind::Monochrome,
            AtlasKey::Image(_) => AtlasTextureKind::Polychrome,
        }
    }
}

impl From<RenderGlyphParams> for AtlasKey {
    fn from(params: RenderGlyphParams) -> Self {
        Self::Glyph(params)
    }
}

impl From<RenderSvgParams> for AtlasKey {
    fn from(params: RenderSvgParams) -> Self {
        Self::Svg(params)
    }
}

impl From<RenderImageParams> for AtlasKey {
    fn from(params: RenderImageParams) -> Self {
        Self::Image(params)
    }
}

#[expect(missing_docs)]
pub trait PlatformAtlas {
    fn get_or_insert_with<'a>(
        &self,
        key: &AtlasKey,
        build: &mut dyn FnMut() -> Result<Option<(Size<DevicePixels>, Cow<'a, [u8]>)>>,
    ) -> Result<Option<AtlasTile>>;
    fn remove(&self, key: &AtlasKey);
}

#[doc(hidden)]
pub struct AtlasTextureList<T> {
    pub textures: Vec<Option<T>>,
    pub free_list: Vec<usize>,
}

impl<T> Default for AtlasTextureList<T> {
    fn default() -> Self {
        Self {
            textures: Vec::default(),
            free_list: Vec::default(),
        }
    }
}

impl<T> ops::Index<usize> for AtlasTextureList<T> {
    type Output = Option<T>;

    fn index(&self, index: usize) -> &Self::Output {
        &self.textures[index]
    }
}

impl<T> AtlasTextureList<T> {
    #[allow(unused)]
    pub fn drain(&mut self) -> std::vec::Drain<'_, Option<T>> {
        self.free_list.clear();
        self.textures.drain(..)
    }

    #[allow(dead_code)]
    pub fn iter_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut T> {
        self.textures.iter_mut().flatten()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
#[expect(missing_docs)]
pub struct AtlasTile {
    /// The texture this tile belongs to.
    pub texture_id: AtlasTextureId,
    /// The unique ID of this tile within its texture.
    pub tile_id: TileId,
    /// Padding around the tile content in pixels.
    pub padding: u32,
    /// The bounds of this tile within the texture.
    pub bounds: Bounds<DevicePixels>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(C)]
#[expect(missing_docs)]
pub struct AtlasTextureId {
    // We use u32 instead of usize for Metal Shader Language compatibility
    /// The index of this texture in the atlas.
    pub index: u32,
    /// The kind of content stored in this texture.
    pub kind: AtlasTextureKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(C)]
#[cfg_attr(
    all(
        any(target_os = "linux", target_os = "freebsd"),
        not(any(feature = "x11", feature = "wayland"))
    ),
    allow(dead_code)
)]
#[expect(missing_docs)]
pub enum AtlasTextureKind {
    Monochrome = 0,
    Polychrome = 1,
    Subpixel = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(C)]
#[expect(missing_docs)]
pub struct TileId(pub u32);

impl From<etagere::AllocId> for TileId {
    fn from(id: etagere::AllocId) -> Self {
        Self(id.serialize())
    }
}

impl From<TileId> for etagere::AllocId {
    fn from(id: TileId) -> Self {
        Self::deserialize(id.0)
    }
}

#[expect(missing_docs)]
pub struct PlatformInputHandler {
    cx: AsyncWindowContext,
    handler: Box<dyn InputHandler>,
}

#[expect(missing_docs)]
#[cfg_attr(
    all(
        any(target_os = "linux", target_os = "freebsd"),
        not(any(feature = "x11", feature = "wayland"))
    ),
    allow(dead_code)
)]
impl PlatformInputHandler {
    pub fn new(cx: AsyncWindowContext, handler: Box<dyn InputHandler>) -> Self {
        Self { cx, handler }
    }

    pub fn selected_text_range(&mut self, ignore_disabled_input: bool) -> Option<UTF16Selection> {
        self.cx
            .update(|window, cx| {
                self.handler
                    .selected_text_range(ignore_disabled_input, window, cx)
            })
            .ok()
            .flatten()
    }

    #[cfg_attr(target_os = "windows", allow(dead_code))]
    pub fn marked_text_range(&mut self) -> Option<Range<usize>> {
        self.cx
            .update(|window, cx| self.handler.marked_text_range(window, cx))
            .ok()
            .flatten()
    }

    #[cfg_attr(
        any(target_os = "linux", target_os = "freebsd", target_os = "windows"),
        allow(dead_code)
    )]
    pub fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
    ) -> Option<String> {
        self.cx
            .update(|window, cx| {
                self.handler
                    .text_for_range(range_utf16, adjusted, window, cx)
            })
            .ok()
            .flatten()
    }

    pub fn replace_text_in_range(&mut self, replacement_range: Option<Range<usize>>, text: &str) {
        self.cx
            .update(|window, cx| {
                self.handler
                    .replace_text_in_range(replacement_range, text, window, cx);
            })
            .ok();
    }

    pub fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
    ) {
        self.cx
            .update(|window, cx| {
                self.handler.replace_and_mark_text_in_range(
                    range_utf16,
                    new_text,
                    new_selected_range,
                    window,
                    cx,
                )
            })
            .ok();
    }

    #[cfg_attr(target_os = "windows", allow(dead_code))]
    pub fn unmark_text(&mut self) {
        self.cx
            .update(|window, cx| self.handler.unmark_text(window, cx))
            .ok();
    }

    pub fn bounds_for_range(&mut self, range_utf16: Range<usize>) -> Option<Bounds<Pixels>> {
        self.cx
            .update(|window, cx| self.handler.bounds_for_range(range_utf16, window, cx))
            .ok()
            .flatten()
    }

    #[allow(dead_code)]
    pub fn apple_press_and_hold_enabled(&mut self) -> bool {
        self.handler.apple_press_and_hold_enabled()
    }

    pub fn dispatch_input(&mut self, input: &str, window: &mut Window, cx: &mut App) {
        self.handler.replace_text_in_range(None, input, window, cx);
    }

    pub fn compute_ime_candidate_bounds(
        marked_range: Option<Range<usize>>,
        selection: &UTF16Selection,
        mut bounds_for_range: impl FnMut(Range<usize>) -> Option<Bounds<Pixels>>,
    ) -> Option<Bounds<Pixels>> {
        if let Some(marked_range) = marked_range {
            // Default to the start of the marked (composing) range.
            let mut line_start = marked_range.start;

            // Walk backward from the caret looking for a line break. A change in
            // the Y coordinate means we crossed into the previous visual line, so
            // the line start is one position after the break point.
            let caret = selection.range.end;
            if let Some(caret_bounds) = bounds_for_range(caret..caret) {
                for i in (marked_range.start..caret).rev() {
                    if let Some(b) = bounds_for_range(i..i) {
                        if (b.origin.y - caret_bounds.origin.y).abs() > px(0.1) {
                            line_start = i + 1;
                            break;
                        }
                    }
                }
            }
            bounds_for_range(line_start..line_start)
        } else {
            // No active composition — use the selection endpoint.
            let offset = if selection.reversed {
                selection.range.start
            } else {
                selection.range.end
            };
            bounds_for_range(offset..offset)
        }
    }

    pub fn selected_bounds(&mut self, window: &mut Window, cx: &mut App) -> Option<Bounds<Pixels>> {
        let marked_range = self.handler.marked_text_range(window, cx);
        let selection = self.handler.selected_text_range(true, window, cx)?;
        Self::compute_ime_candidate_bounds(marked_range, &selection, |range| {
            self.handler.bounds_for_range(range, window, cx)
        })
    }

    pub fn ime_candidate_bounds(&mut self) -> Option<Bounds<Pixels>> {
        let marked_range = self.marked_text_range();
        let selection = self.selected_text_range(true)?;
        Self::compute_ime_candidate_bounds(marked_range, &selection, |range| {
            self.bounds_for_range(range)
        })
    }

    #[allow(unused)]
    pub fn character_index_for_point(&mut self, point: Point<Pixels>) -> Option<usize> {
        self.cx
            .update(|window, cx| self.handler.character_index_for_point(point, window, cx))
            .ok()
            .flatten()
    }

    #[allow(dead_code)]
    pub fn accepts_text_input(&mut self, window: &mut Window, cx: &mut App) -> bool {
        self.handler.accepts_text_input(window, cx)
    }

    #[allow(dead_code)]
    pub fn query_accepts_text_input(&mut self) -> bool {
        self.cx
            .update(|window, cx| self.handler.accepts_text_input(window, cx))
            .unwrap_or(true)
    }

    #[allow(dead_code)]
    pub fn query_prefers_ime_for_printable_keys(&mut self) -> bool {
        self.cx
            .update(|window, cx| self.handler.prefers_ime_for_printable_keys(window, cx))
            .unwrap_or(false)
    }
}

/// A struct representing a selection in a text buffer, in UTF16 characters.
/// This is different from a range because the head may be before the tail.
#[derive(Debug)]
pub struct UTF16Selection {
    /// The range of text in the document this selection corresponds to
    /// in UTF16 characters.
    pub range: Range<usize>,
    /// Whether the head of this selection is at the start (true), or end (false)
    /// of the range
    pub reversed: bool,
}

/// Zed's interface for handling text input from the platform's IME system
/// This is currently a 1:1 exposure of the NSTextInputClient API:
///
/// <https://developer.apple.com/documentation/appkit/nstextinputclient>
pub trait InputHandler: 'static {
    /// Get the range of the user's currently selected text, if any
    /// Corresponds to [selectedRange()](https://developer.apple.com/documentation/appkit/nstextinputclient/1438242-selectedrange)
    ///
    /// Return value is in terms of UTF-16 characters, from 0 to the length of the document
    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<UTF16Selection>;

    /// Get the range of the currently marked text, if any
    /// Corresponds to [markedRange()](https://developer.apple.com/documentation/appkit/nstextinputclient/1438250-markedrange)
    ///
    /// Return value is in terms of UTF-16 characters, from 0 to the length of the document
    fn marked_text_range(&mut self, window: &mut Window, cx: &mut App) -> Option<Range<usize>>;

    /// Get the text for the given document range in UTF-16 characters
    /// Corresponds to [attributedSubstring(forProposedRange: actualRange:)](https://developer.apple.com/documentation/appkit/nstextinputclient/1438238-attributedsubstring)
    ///
    /// range_utf16 is in terms of UTF-16 characters
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<String>;

    /// Replace the text in the given document range with the given text
    /// Corresponds to [insertText(_:replacementRange:)](https://developer.apple.com/documentation/appkit/nstextinputclient/1438258-inserttext)
    ///
    /// replacement_range is in terms of UTF-16 characters
    fn replace_text_in_range(
        &mut self,
        replacement_range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut App,
    );

    /// Replace the text in the given document range with the given text,
    /// and mark the given text as part of an IME 'composing' state
    /// Corresponds to [setMarkedText(_:selectedRange:replacementRange:)](https://developer.apple.com/documentation/appkit/nstextinputclient/1438246-setmarkedtext)
    ///
    /// range_utf16 is in terms of UTF-16 characters
    /// new_selected_range is in terms of UTF-16 characters
    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut App,
    );

    /// Remove the IME 'composing' state from the document
    /// Corresponds to [unmarkText()](https://developer.apple.com/documentation/appkit/nstextinputclient/1438239-unmarktext)
    fn unmark_text(&mut self, window: &mut Window, cx: &mut App);

    /// Get the bounds of the given document range in screen coordinates
    /// Corresponds to [firstRect(forCharacterRange:actualRange:)](https://developer.apple.com/documentation/appkit/nstextinputclient/1438240-firstrect)
    ///
    /// This is used for positioning the IME candidate window
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Bounds<Pixels>>;

    /// Get the character offset for the given point in terms of UTF16 characters
    ///
    /// Corresponds to [characterIndexForPoint:](https://developer.apple.com/documentation/appkit/nstextinputclient/characterindex(for:))
    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<usize>;

    /// Allows a given input context to opt into getting raw key repeats instead of
    /// sending these to the platform.
    /// TODO: Ideally we should be able to set ApplePressAndHoldEnabled in NSUserDefaults
    /// (which is how iTerm does it) but it doesn't seem to work for me.
    #[allow(dead_code)]
    fn apple_press_and_hold_enabled(&mut self) -> bool {
        true
    }

    /// Returns whether this handler is accepting text input to be inserted.
    fn accepts_text_input(&mut self, _window: &mut Window, _cx: &mut App) -> bool {
        true
    }

    /// Returns whether printable keys should be routed to the IME before keybinding
    /// matching when a non-ASCII input source (e.g. Japanese, Korean, Chinese IME)
    /// is active. This prevents multi-stroke keybindings like `jj` from intercepting
    /// keys that the IME should compose.
    ///
    /// Defaults to `false`. The editor overrides this based on whether it expects
    /// character input (e.g. Vim insert mode returns `true`, normal mode returns `false`).
    /// The terminal keeps the default `false` so that raw keys reach the terminal process.
    fn prefers_ime_for_printable_keys(&mut self, _window: &mut Window, _cx: &mut App) -> bool {
        false
    }
}

/// The variables that can be configured when creating a new window
#[derive(Debug)]
pub struct WindowOptions {
    /// Specifies the state and bounds of the window in screen coordinates.
    /// - `None`: Inherit the bounds.
    /// - `Some(WindowBounds)`: Open a window with corresponding state and its restore size.
    pub window_bounds: Option<WindowBounds>,

    /// The titlebar configuration of the window
    pub titlebar: Option<TitlebarOptions>,

    /// Whether the window should be focused when created
    pub focus: bool,

    /// Whether the window should be shown when created
    pub show: bool,

    /// The kind of window to create
    pub kind: WindowKind,

    /// Whether the window should be movable by the user
    pub is_movable: bool,

    /// Whether the window should be resizable by the user
    pub is_resizable: bool,

    /// Whether the window should be minimized by the user
    pub is_minimizable: bool,

    /// The display to create the window on, if this is None,
    /// the window will be created on the main display
    pub display_id: Option<DisplayId>,

    /// The appearance of the window background.
    pub window_background: WindowBackgroundAppearance,

    /// Application identifier of the window. Can by used by desktop environments to group applications together.
    pub app_id: Option<String>,

    /// Window minimum size
    pub window_min_size: Option<Size<Pixels>>,

    /// Whether to use client or server side decorations. Wayland only
    /// Note that this may be ignored.
    pub window_decorations: Option<WindowDecorations>,

    /// Icon image (X11 only)
    pub icon: Option<Arc<image::RgbaImage>>,

    /// Tab group name, allows opening the window as a native tab on macOS 10.12+. Windows with the same tabbing identifier will be grouped together.
    pub tabbing_identifier: Option<String>,
}

/// The variables that can be configured when creating a new window
#[derive(Debug)]
#[cfg_attr(
    all(
        any(target_os = "linux", target_os = "freebsd"),
        not(any(feature = "x11", feature = "wayland"))
    ),
    allow(dead_code)
)]
#[allow(missing_docs)]
pub struct WindowParams {
    pub bounds: Bounds<Pixels>,

    /// The titlebar configuration of the window
    #[cfg_attr(feature = "wayland", allow(dead_code))]
    pub titlebar: Option<TitlebarOptions>,

    /// The kind of window to create
    #[cfg_attr(any(target_os = "linux", target_os = "freebsd"), allow(dead_code))]
    pub kind: WindowKind,

    /// Whether the window should be movable by the user
    #[cfg_attr(any(target_os = "linux", target_os = "freebsd"), allow(dead_code))]
    pub is_movable: bool,

    /// Whether the window should be resizable by the user
    #[cfg_attr(any(target_os = "linux", target_os = "freebsd"), allow(dead_code))]
    pub is_resizable: bool,

    /// Whether the window should be minimized by the user
    #[cfg_attr(any(target_os = "linux", target_os = "freebsd"), allow(dead_code))]
    pub is_minimizable: bool,

    #[cfg_attr(
        any(target_os = "linux", target_os = "freebsd", target_os = "windows"),
        allow(dead_code)
    )]
    pub focus: bool,

    #[cfg_attr(any(target_os = "linux", target_os = "freebsd"), allow(dead_code))]
    pub show: bool,

    /// An image to set as the window icon (x11 only)
    #[cfg_attr(feature = "wayland", allow(dead_code))]
    pub icon: Option<Arc<image::RgbaImage>>,

    #[cfg_attr(feature = "wayland", allow(dead_code))]
    pub display_id: Option<DisplayId>,

    pub window_min_size: Option<Size<Pixels>>,
    #[cfg(target_os = "macos")]
    pub tabbing_identifier: Option<String>,
}

/// Represents the status of how a window should be opened.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum WindowBounds {
    /// Indicates that the window should open in a windowed state with the given bounds.
    Windowed(Bounds<Pixels>),
    /// Indicates that the window should open in a maximized state.
    /// The bounds provided here represent the restore size of the window.
    Maximized(Bounds<Pixels>),
    /// Indicates that the window should open in fullscreen mode.
    /// The bounds provided here represent the restore size of the window.
    Fullscreen(Bounds<Pixels>),
}

impl Default for WindowBounds {
    fn default() -> Self {
        WindowBounds::Windowed(Bounds::default())
    }
}

impl WindowBounds {
    /// Retrieve the inner bounds
    pub fn get_bounds(&self) -> Bounds<Pixels> {
        match self {
            WindowBounds::Windowed(bounds) => *bounds,
            WindowBounds::Maximized(bounds) => *bounds,
            WindowBounds::Fullscreen(bounds) => *bounds,
        }
    }

    /// Creates a new window bounds that centers the window on the screen.
    pub fn centered(size: Size<Pixels>, cx: &App) -> Self {
        WindowBounds::Windowed(Bounds::centered(None, size, cx))
    }
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            window_bounds: None,
            titlebar: Some(TitlebarOptions {
                title: Default::default(),
                appears_transparent: Default::default(),
                traffic_light_position: Default::default(),
            }),
            focus: true,
            show: true,
            kind: WindowKind::Normal,
            is_movable: true,
            is_resizable: true,
            is_minimizable: true,
            display_id: None,
            window_background: WindowBackgroundAppearance::default(),
            icon: None,
            app_id: None,
            window_min_size: None,
            window_decorations: None,
            tabbing_identifier: None,
        }
    }
}

/// The options that can be configured for a window's titlebar
#[derive(Debug, Default)]
pub struct TitlebarOptions {
    /// The initial title of the window
    pub title: Option<SharedString>,

    /// Should the default system titlebar be hidden to allow for a custom-drawn titlebar? (macOS and Windows only)
    /// Refer to [`WindowOptions::window_decorations`] on Linux
    pub appears_transparent: bool,

    /// The position of the macOS traffic light buttons
    pub traffic_light_position: Option<Point<Pixels>>,
}

/// The kind of window to create
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowKind {
    /// A normal application window
    Normal,

    /// A window that appears above all other windows, usually used for alerts or popups
    /// use sparingly!
    PopUp,

    /// A floating window that appears on top of its parent window
    Floating,

    /// A Wayland LayerShell window, used to draw overlays or backgrounds for applications such as
    /// docks, notifications or wallpapers.
    #[cfg(all(target_os = "linux", feature = "wayland"))]
    LayerShell(layer_shell::LayerShellOptions),

    /// A window that appears on top of its parent window and blocks interaction with it
    /// until the modal window is closed
    Dialog,
}

/// The appearance of the window, as defined by the operating system.
///
/// On macOS, this corresponds to named [`NSAppearance`](https://developer.apple.com/documentation/appkit/nsappearance)
/// values.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum WindowAppearance {
    /// A light appearance.
    ///
    /// On macOS, this corresponds to the `aqua` appearance.
    #[default]
    Light,

    /// A light appearance with vibrant colors.
    ///
    /// On macOS, this corresponds to the `NSAppearanceNameVibrantLight` appearance.
    VibrantLight,

    /// A dark appearance.
    ///
    /// On macOS, this corresponds to the `darkAqua` appearance.
    Dark,

    /// A dark appearance with vibrant colors.
    ///
    /// On macOS, this corresponds to the `NSAppearanceNameVibrantDark` appearance.
    VibrantDark,
}

/// The appearance of the background of the window itself, when there is
/// no content or the content is transparent.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub enum WindowBackgroundAppearance {
    /// Opaque.
    ///
    /// This lets the window manager know that content behind this
    /// window does not need to be drawn.
    ///
    /// Actual color depends on the system and themes should define a fully
    /// opaque background color instead.
    #[default]
    Opaque,
    /// Plain alpha transparency.
    Transparent,
    /// Transparency, but the contents behind the window are blurred.
    ///
    /// Not always supported.
    Blurred,
    /// The Mica backdrop material, supported on Windows 11.
    MicaBackdrop,
    /// The Mica Alt backdrop material, supported on Windows 11.
    MicaAltBackdrop,
}

/// The text rendering mode to use for drawing glyphs.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum TextRenderingMode {
    /// Use the platform's default text rendering mode.
    #[default]
    PlatformDefault,
    /// Use subpixel (ClearType-style) text rendering.
    Subpixel,
    /// Use grayscale text rendering.
    Grayscale,
}

/// The options that can be configured for a file dialog prompt
#[derive(Clone, Debug)]
pub struct PathPromptOptions {
    /// Should the prompt allow files to be selected?
    pub files: bool,
    /// Should the prompt allow directories to be selected?
    pub directories: bool,
    /// Should the prompt allow multiple files to be selected?
    pub multiple: bool,
    /// The prompt to show to a user when selecting a path
    pub prompt: Option<SharedString>,
}

/// What kind of prompt styling to show
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum PromptLevel {
    /// A prompt that is shown when the user should be notified of something
    Info,

    /// A prompt that is shown when the user needs to be warned of a potential problem
    Warning,

    /// A prompt that is shown when a critical problem has occurred
    Critical,
}

/// Prompt Button
#[derive(Clone, Debug, PartialEq)]
pub enum PromptButton {
    /// Ok button
    Ok(SharedString),
    /// Cancel button
    Cancel(SharedString),
    /// Other button
    Other(SharedString),
}

impl PromptButton {
    /// Create a button with label
    pub fn new(label: impl Into<SharedString>) -> Self {
        PromptButton::Other(label.into())
    }

    /// Create an Ok button
    pub fn ok(label: impl Into<SharedString>) -> Self {
        PromptButton::Ok(label.into())
    }

    /// Create a Cancel button
    pub fn cancel(label: impl Into<SharedString>) -> Self {
        PromptButton::Cancel(label.into())
    }

    /// Returns true if this button is a cancel button.
    #[allow(dead_code)]
    pub fn is_cancel(&self) -> bool {
        matches!(self, PromptButton::Cancel(_))
    }

    /// Returns the label of the button
    pub fn label(&self) -> &SharedString {
        match self {
            PromptButton::Ok(label) => label,
            PromptButton::Cancel(label) => label,
            PromptButton::Other(label) => label,
        }
    }
}

impl From<&str> for PromptButton {
    fn from(value: &str) -> Self {
        match value.to_lowercase().as_str() {
            "ok" => PromptButton::Ok("OK".into()),
            "cancel" => PromptButton::Cancel("Cancel".into()),
            _ => PromptButton::Other(SharedString::from(value.to_owned())),
        }
    }
}

/// The style of the cursor (pointer)
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum CursorStyle {
    /// The default cursor
    #[default]
    Arrow,

    /// A text input cursor
    /// corresponds to the CSS cursor value `text`
    IBeam,

    /// A crosshair cursor
    /// corresponds to the CSS cursor value `crosshair`
    Crosshair,

    /// A closed hand cursor
    /// corresponds to the CSS cursor value `grabbing`
    ClosedHand,

    /// An open hand cursor
    /// corresponds to the CSS cursor value `grab`
    OpenHand,

    /// A pointing hand cursor
    /// corresponds to the CSS cursor value `pointer`
    PointingHand,

    /// A resize left cursor
    /// corresponds to the CSS cursor value `w-resize`
    ResizeLeft,

    /// A resize right cursor
    /// corresponds to the CSS cursor value `e-resize`
    ResizeRight,

    /// A resize cursor to the left and right
    /// corresponds to the CSS cursor value `ew-resize`
    ResizeLeftRight,

    /// A resize up cursor
    /// corresponds to the CSS cursor value `n-resize`
    ResizeUp,

    /// A resize down cursor
    /// corresponds to the CSS cursor value `s-resize`
    ResizeDown,

    /// A resize cursor directing up and down
    /// corresponds to the CSS cursor value `ns-resize`
    ResizeUpDown,

    /// A resize cursor directing up-left and down-right
    /// corresponds to the CSS cursor value `nesw-resize`
    ResizeUpLeftDownRight,

    /// A resize cursor directing up-right and down-left
    /// corresponds to the CSS cursor value `nwse-resize`
    ResizeUpRightDownLeft,

    /// A cursor indicating that the item/column can be resized horizontally.
    /// corresponds to the CSS cursor value `col-resize`
    ResizeColumn,

    /// A cursor indicating that the item/row can be resized vertically.
    /// corresponds to the CSS cursor value `row-resize`
    ResizeRow,

    /// A text input cursor for vertical layout
    /// corresponds to the CSS cursor value `vertical-text`
    IBeamCursorForVerticalLayout,

    /// A cursor indicating that the operation is not allowed
    /// corresponds to the CSS cursor value `not-allowed`
    OperationNotAllowed,

    /// A cursor indicating that the operation will result in a link
    /// corresponds to the CSS cursor value `alias`
    DragLink,

    /// A cursor indicating that the operation will result in a copy
    /// corresponds to the CSS cursor value `copy`
    DragCopy,

    /// A cursor indicating that the operation will result in a context menu
    /// corresponds to the CSS cursor value `context-menu`
    ContextualMenu,
}

/// A clipboard item that should be copied to the clipboard
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardItem {
    /// The entries in this clipboard item.
    pub entries: Vec<ClipboardEntry>,
}

/// Either a ClipboardString or a ClipboardImage
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipboardEntry {
    /// A string entry
    String(ClipboardString),
    /// An image entry
    Image(Image),
    /// A file entry
    ExternalPaths(crate::ExternalPaths),
}

impl ClipboardItem {
    /// Create a new ClipboardItem::String with no associated metadata
    pub fn new_string(text: String) -> Self {
        Self {
            entries: vec![ClipboardEntry::String(ClipboardString::new(text))],
        }
    }

    /// Create a new ClipboardItem::String with the given text and associated metadata
    pub fn new_string_with_metadata(text: String, metadata: String) -> Self {
        Self {
            entries: vec![ClipboardEntry::String(ClipboardString {
                text,
                metadata: Some(metadata),
            })],
        }
    }

    /// Create a new ClipboardItem::String with the given text and associated metadata
    pub fn new_string_with_json_metadata<T: Serialize>(text: String, metadata: T) -> Self {
        Self {
            entries: vec![ClipboardEntry::String(
                ClipboardString::new(text).with_json_metadata(metadata),
            )],
        }
    }

    /// Create a new ClipboardItem::Image with the given image with no associated metadata
    pub fn new_image(image: &Image) -> Self {
        Self {
            entries: vec![ClipboardEntry::Image(image.clone())],
        }
    }

    /// Concatenates together all the ClipboardString entries in the item.
    /// Returns None if there were no ClipboardString entries.
    pub fn text(&self) -> Option<String> {
        let mut answer = String::new();

        for entry in self.entries.iter() {
            if let ClipboardEntry::String(ClipboardString { text, metadata: _ }) = entry {
                answer.push_str(text);
            }
        }

        if answer.is_empty() {
            for entry in self.entries.iter() {
                if let ClipboardEntry::ExternalPaths(paths) = entry {
                    for path in &paths.0 {
                        use std::fmt::Write as _;
                        _ = write!(answer, "{}", path.display());
                    }
                }
            }
        }

        if !answer.is_empty() {
            Some(answer)
        } else {
            None
        }
    }

    /// If this item is one ClipboardEntry::String, returns its metadata.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn metadata(&self) -> Option<&String> {
        match self.entries().first() {
            Some(ClipboardEntry::String(clipboard_string)) if self.entries.len() == 1 => {
                clipboard_string.metadata.as_ref()
            }
            _ => None,
        }
    }

    /// Get the item's entries
    pub fn entries(&self) -> &[ClipboardEntry] {
        &self.entries
    }

    /// Get owned versions of the item's entries
    pub fn into_entries(self) -> impl Iterator<Item = ClipboardEntry> {
        self.entries.into_iter()
    }
}

impl From<ClipboardString> for ClipboardEntry {
    fn from(value: ClipboardString) -> Self {
        Self::String(value)
    }
}

impl From<String> for ClipboardEntry {
    fn from(value: String) -> Self {
        Self::from(ClipboardString::from(value))
    }
}

impl From<Image> for ClipboardEntry {
    fn from(value: Image) -> Self {
        Self::Image(value)
    }
}

impl From<ClipboardEntry> for ClipboardItem {
    fn from(value: ClipboardEntry) -> Self {
        Self {
            entries: vec![value],
        }
    }
}

impl From<String> for ClipboardItem {
    fn from(value: String) -> Self {
        Self::from(ClipboardEntry::from(value))
    }
}

impl From<Image> for ClipboardItem {
    fn from(value: Image) -> Self {
        Self::from(ClipboardEntry::from(value))
    }
}

/// One of the editor's supported image formats (e.g. PNG, JPEG) - used when dealing with images in the clipboard
#[derive(Clone, Copy, Debug, Eq, PartialEq, EnumIter, Hash)]
pub enum ImageFormat {
    // Sorted from most to least likely to be pasted into an editor,
    // which matters when we iterate through them trying to see if
    // clipboard content matches them.
    /// .png
    Png,
    /// .jpeg or .jpg
    Jpeg,
    /// .webp
    Webp,
    /// .gif
    Gif,
    /// .svg
    Svg,
    /// .bmp
    Bmp,
    /// .tif or .tiff
    Tiff,
    /// .ico
    Ico,
    /// Netpbm image formats (.pbm, .ppm, .pgm).
    Pnm,
}

impl ImageFormat {
    /// Returns the mime type for the ImageFormat
    pub const fn mime_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Webp => "image/webp",
            ImageFormat::Gif => "image/gif",
            ImageFormat::Svg => "image/svg+xml",
            ImageFormat::Bmp => "image/bmp",
            ImageFormat::Tiff => "image/tiff",
            ImageFormat::Ico => "image/ico",
            ImageFormat::Pnm => "image/x-portable-anymap",
        }
    }

    /// Returns the ImageFormat for the given mime type, including known aliases.
    pub fn from_mime_type(mime_type: &str) -> Option<Self> {
        use strum::IntoEnumIterator;
        Self::iter()
            .find(|format| format.mime_type() == mime_type)
            .or_else(|| Self::from_mime_type_alias(mime_type))
    }

    /// Non-canonical mime types that some producers use in the wild.
    /// Unlike `mime_type()` which returns the single canonical form,
    /// these are legacy or shortened variants we still need to recognize.
    fn from_mime_type_alias(mime_type: &str) -> Option<Self> {
        match mime_type {
            "image/jpg" => Some(Self::Jpeg),
            "image/tif" => Some(Self::Tiff),
            _ => None,
        }
    }
}

/// An image, with a format and certain bytes
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// The image format the bytes represent (e.g. PNG)
    pub format: ImageFormat,
    /// The raw image bytes
    pub bytes: Vec<u8>,
    /// The unique ID for the image
    pub id: u64,
}

impl Hash for Image {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.id);
    }
}

impl Image {
    /// An empty image containing no data
    pub fn empty() -> Self {
        Self::from_bytes(ImageFormat::Png, Vec::new())
    }

    /// Create an image from a format and bytes
    pub fn from_bytes(format: ImageFormat, bytes: Vec<u8>) -> Self {
        Self {
            id: hash(&bytes),
            format,
            bytes,
        }
    }

    /// Get this image's ID
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Use the GPUI `use_asset` API to make this image renderable
    pub fn use_render_image(
        self: Arc<Self>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<RenderImage>> {
        ImageSource::Image(self)
            .use_data(None, window, cx)
            .and_then(|result| result.ok())
    }

    /// Use the GPUI `get_asset` API to make this image renderable
    pub fn get_render_image(
        self: Arc<Self>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<RenderImage>> {
        ImageSource::Image(self)
            .get_data(None, window, cx)
            .and_then(|result| result.ok())
    }

    /// Use the GPUI `remove_asset` API to drop this image, if possible.
    pub fn remove_asset(self: Arc<Self>, cx: &mut App) {
        ImageSource::Image(self).remove_asset(cx);
    }

    /// Convert the clipboard image to an `ImageData` object.
    pub fn to_image_data(&self, svg_renderer: SvgRenderer) -> Result<Arc<RenderImage>> {
        fn frames_for_image(
            bytes: &[u8],
            format: image::ImageFormat,
        ) -> Result<SmallVec<[Frame; 1]>> {
            let mut data = image::load_from_memory_with_format(bytes, format)?.into_rgba8();

            // Convert from RGBA to BGRA.
            for pixel in data.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }

            Ok(SmallVec::from_elem(Frame::new(data), 1))
        }

        let frames = match self.format {
            ImageFormat::Gif => {
                let decoder = GifDecoder::new(Cursor::new(&self.bytes))?;
                let mut frames = SmallVec::new();

                for frame in decoder.into_frames() {
                    match frame {
                        Ok(mut frame) => {
                            // Convert from RGBA to BGRA.
                            for pixel in frame.buffer_mut().chunks_exact_mut(4) {
                                pixel.swap(0, 2);
                            }
                            frames.push(frame);
                        }
                        Err(err) => {
                            log::debug!("Skipping GIF frame due to decode error: {err}");
                        }
                    }
                }

                if frames.is_empty() {
                    anyhow::bail!("GIF could not be decoded: all frames failed");
                }

                frames
            }
            ImageFormat::Png => frames_for_image(&self.bytes, image::ImageFormat::Png)?,
            ImageFormat::Jpeg => frames_for_image(&self.bytes, image::ImageFormat::Jpeg)?,
            ImageFormat::Webp => frames_for_image(&self.bytes, image::ImageFormat::WebP)?,
            ImageFormat::Bmp => frames_for_image(&self.bytes, image::ImageFormat::Bmp)?,
            ImageFormat::Tiff => frames_for_image(&self.bytes, image::ImageFormat::Tiff)?,
            ImageFormat::Ico => frames_for_image(&self.bytes, image::ImageFormat::Ico)?,
            ImageFormat::Svg => {
                return svg_renderer
                    .render_single_frame(&self.bytes, 1.0)
                    .map_err(Into::into);
            }
            ImageFormat::Pnm => frames_for_image(&self.bytes, image::ImageFormat::Pnm)?,
        };

        Ok(Arc::new(RenderImage::new(frames)))
    }

    /// Get the format of the clipboard image
    pub fn format(&self) -> ImageFormat {
        self.format
    }

    /// Get the raw bytes of the clipboard image
    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }
}

/// A clipboard item that should be copied to the clipboard
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardString {
    /// The text content.
    pub text: String,
    /// Optional metadata associated with this clipboard string.
    pub metadata: Option<String>,
}

impl ClipboardString {
    /// Create a new clipboard string with the given text
    pub fn new(text: String) -> Self {
        Self {
            text,
            metadata: None,
        }
    }

    /// Return a new clipboard item with the metadata replaced by the given metadata,
    /// after serializing it as JSON.
    pub fn with_json_metadata<T: Serialize>(mut self, metadata: T) -> Self {
        self.metadata = Some(serde_json::to_string(&metadata).unwrap());
        self
    }

    /// Get the text of the clipboard string
    pub fn text(&self) -> &String {
        &self.text
    }

    /// Get the owned text of the clipboard string
    pub fn into_text(self) -> String {
        self.text
    }

    /// Get the metadata of the clipboard string, formatted as JSON
    pub fn metadata_json<T>(&self) -> Option<T>
    where
        T: for<'a> Deserialize<'a>,
    {
        self.metadata
            .as_ref()
            .and_then(|m| serde_json::from_str(m).ok())
    }

    #[cfg_attr(any(target_os = "linux", target_os = "freebsd"), allow(dead_code))]
    /// Compute a hash of the given text for clipboard change detection.
    pub fn text_hash(text: &str) -> u64 {
        let mut hasher = SeaHasher::new();
        text.hash(&mut hasher);
        hasher.finish()
    }
}

impl From<String> for ClipboardString {
    fn from(value: String) -> Self {
        Self {
            text: value,
            metadata: None,
        }
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_svg_image_to_image_data_converts_to_bgra() {
        let image = Image::from_bytes(
            ImageFormat::Svg,
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1">
<rect width="1" height="1" fill="#38BDF8"/>
</svg>"##
                .to_vec(),
        );

        let render_image = image.to_image_data(SvgRenderer::new(Arc::new(()))).unwrap();
        let bytes = render_image.as_bytes(0).unwrap();

        for pixel in bytes.chunks_exact(4) {
            assert_eq!(pixel, &[0xF8, 0xBD, 0x38, 0xFF]);
        }
    }
}

#[cfg(test)]
mod begin_frame_tests {
    use super::*;

    #[test]
    fn possible_deadlines_returns_os_preferred_candidate() {
        let possible_deadlines = PossibleDeadlines {
            os_preferred_index: 1,
            deadlines: vec![
                PossibleDeadline {
                    vsync_id: 10,
                    latch_delta: Duration::from_millis(8),
                    present_delta: Duration::from_millis(16),
                },
                PossibleDeadline {
                    vsync_id: 11,
                    latch_delta: Duration::from_millis(24),
                    present_delta: Duration::from_millis(32),
                },
            ],
        };

        assert_eq!(
            possible_deadlines.os_preferred_deadline(),
            Some(&PossibleDeadline {
                vsync_id: 11,
                latch_delta: Duration::from_millis(24),
                present_delta: Duration::from_millis(32),
            })
        );
    }

    #[test]
    fn possible_deadlines_returns_none_for_invalid_os_preferred_index() {
        let possible_deadlines = PossibleDeadlines {
            os_preferred_index: 1,
            deadlines: vec![PossibleDeadline {
                vsync_id: 10,
                latch_delta: Duration::from_millis(8),
                present_delta: Duration::from_millis(16),
            }],
        };

        assert_eq!(possible_deadlines.os_preferred_deadline(), None);
    }

    // --- FrameDeadlineDecider tests ---

    fn deadline(vsync_id: i64, delta_ms: u64) -> PossibleDeadline {
        PossibleDeadline {
            vsync_id,
            latch_delta: Duration::from_millis(delta_ms),
            present_delta: Duration::from_millis(delta_ms),
        }
    }

    fn deadlines(os_pref: usize, deltas_ms: &[u64]) -> PossibleDeadlines {
        PossibleDeadlines {
            os_preferred_index: os_pref,
            deadlines: deltas_ms
                .iter()
                .enumerate()
                .map(|(i, &ms)| deadline(i as i64, ms))
                .collect(),
        }
    }

    #[test]
    fn decider_platform_preferred_short_circuits() {
        let mut decider = FrameDeadlineDecider::new(true);
        let pd = deadlines(2, &[16, 32, 48]);
        let index =
            decider.select_deadline(&pd, Duration::from_millis(16), 3, Instant::now(), None);
        assert_eq!(index, 2);
    }

    #[test]
    fn decider_new_sequence_selects_within_buffer_budget() {
        // 3 buffers * 16ms = 48ms target. Candidates: [16, 32, 48, 64].
        // Upper bound of <=48 is index 3 (48). Chrome preferred = index 2 (48ms).
        // 48 <= 48, so not > target. 48 >= OS preferred (16). Result = 2.
        let mut decider = FrameDeadlineDecider::new(false);
        let pd = deadlines(0, &[16, 32, 48, 64]);
        let now = Instant::now();
        let index = decider.select_deadline(&pd, Duration::from_millis(16), 3, now, None);
        assert_eq!(index, 2);
    }

    #[test]
    fn decider_in_sequence_sticks_to_closest_present_delta() {
        let mut decider = FrameDeadlineDecider::new(false);
        let pd = deadlines(0, &[16, 32, 48, 64]);
        let now = Instant::now();

        // First frame: max_allowed_buffers=3 → target 48ms → index 2 (48ms).
        let first = decider.select_deadline(&pd, Duration::from_millis(16), 3, now, None);
        assert_eq!(first, 2);

        // Second frame in sequence: sticks to 48ms (index 2, within 1ms of 48ms).
        let second = decider.select_deadline(&pd, Duration::from_millis(16), 1, now, None);
        assert_eq!(second, 2);
    }

    #[test]
    fn decider_on_go_idle_resets_sequence() {
        let mut decider = FrameDeadlineDecider::new(false);
        let pd = deadlines(0, &[16, 32, 48, 64]);
        let now = Instant::now();

        decider.select_deadline(&pd, Duration::from_millis(16), 3, now, None);
        decider.on_go_idle();

        // After idle, new sequence with 1 buffer → target 16ms → index 0.
        let index = decider.select_deadline(&pd, Duration::from_millis(16), 1, now, None);
        assert_eq!(index, 0);
    }

    #[test]
    fn decider_input_aware_cap_clamps_target_down() {
        // Without input: 3 buffers * 16ms = 48ms → index 2.
        // With input 80ms before frame_time:
        //   input_delta = 80ms
        //   latency_cap = 100 - 16 - 4 = 80ms
        //   max_present_delta = 80 - 80 = 0ms
        //   target clamped to 0ms → all candidates exceed → OS preferred fallback.
        let mut decider = FrameDeadlineDecider::new(false);
        let pd = deadlines(0, &[16, 32, 48]);
        let frame_time = Instant::now();
        let earliest_input = frame_time - Duration::from_millis(80);

        let index = decider.select_deadline(
            &pd,
            Duration::from_millis(16),
            3,
            frame_time,
            Some(earliest_input),
        );
        assert_eq!(index, 0);
    }

    #[test]
    fn decider_input_aware_cap_partial_clamp() {
        // input_delta = 50ms
        // latency_cap = 100 - 16 - 4 = 80ms
        // max_present_delta = 80 - 50 = 30ms
        // target clamped from 48ms to 30ms → upper_bound of <=30 in [16,32,48] = index 1 (32ms)
        // Wait: 32 > 30, so upper_bound = 1, decrement to 0 (16ms). 16 < 16 (OS pref) → fallback to OS pref.
        // Hmm, that's not a great test. Let me pick different values.
        // Let me use candidates [10, 25, 40, 55] with 16ms interval:
        // input_delta = 50ms, latency_cap = 80ms, max_present = 30ms
        // upper_bound of <=30 in [10,25,40,55] = index 2, decrement to 1 (25ms).
        // 25 <= 30 ✓, 25 >= OS pref (10) ✓ → result = 1.
        let mut decider = FrameDeadlineDecider::new(false);
        let pd = deadlines(0, &[10, 25, 40, 55]);
        let frame_time = Instant::now();
        let earliest_input = frame_time - Duration::from_millis(50);

        let index = decider.select_deadline(
            &pd,
            Duration::from_millis(16),
            3,
            frame_time,
            Some(earliest_input),
        );
        assert_eq!(index, 1);
    }

    #[test]
    fn decider_falls_back_to_os_preferred_when_chrome_exceeds_target() {
        // 1 buffer * 16ms = 16ms target. Candidates: [32, 48, 64] (all > 16).
        // upper_bound = 0, decrement saturates to 0 (32ms). 32 > 16 → OS pref.
        let mut decider = FrameDeadlineDecider::new(false);
        let pd = deadlines(1, &[32, 48, 64]);
        let now = Instant::now();

        let index = decider.select_deadline(&pd, Duration::from_millis(16), 1, now, None);
        assert_eq!(index, 1);
    }

    #[test]
    fn max_pending_swaps_for_deadline_matches_chromium_formula() {
        // present_delta=48ms, interval=16ms:
        // (48_000_000 + 0.8 * 16_000_000) / 16_000_000 = (48_000_000 + 12_800_000) / 16_000_000 = 3.8 → 3
        assert_eq!(
            max_pending_swaps_for_deadline(Duration::from_millis(48), Duration::from_millis(16)),
            3
        );
        // present_delta=32ms, interval=16ms:
        // (32_000_000 + 12_800_000) / 16_000_000 = 2.8 → 2
        assert_eq!(
            max_pending_swaps_for_deadline(Duration::from_millis(32), Duration::from_millis(16)),
            2
        );
        // present_delta=16ms, interval=16ms:
        // (16_000_000 + 12_800_000) / 16_000_000 = 1.8 → 1
        assert_eq!(
            max_pending_swaps_for_deadline(Duration::from_millis(16), Duration::from_millis(16)),
            1
        );
    }

    #[test]
    fn max_pending_swaps_for_deadline_returns_one_for_zero_interval() {
        assert_eq!(
            max_pending_swaps_for_deadline(Duration::from_millis(16), Duration::ZERO),
            1
        );
    }

    // --- RefreshRateSwapCaps tests ---

    #[test]
    fn refresh_rate_swap_caps_uses_120hz_tier_at_8_5ms_interval() {
        // Phase 5 acceptance: "On a 120 Hz display, the pending-swap cap is the
        // 120 Hz tier, not the 60 Hz default."
        let caps = RefreshRateSwapCaps {
            max_pending_swaps: 2,
            max_pending_swaps_120hz: Some(3),
            max_pending_swaps_90hz: None,
            max_pending_swaps_72hz: None,
        };
        // 8.5ms = 8500μs is exactly the k120HzInterval threshold.
        assert_eq!(
            max_pending_swaps_for_refresh_rate(Duration::from_micros(8500), &caps),
            2, // 8500μs is NOT < 8500μs, so this is 90Hz tier → default
        );
        // 8.49ms is < 8.5ms threshold → 120Hz tier.
        assert_eq!(
            max_pending_swaps_for_refresh_rate(Duration::from_micros(8499), &caps),
            3,
        );
    }

    #[test]
    fn refresh_rate_swap_caps_uses_default_below_all_tiers() {
        let caps = RefreshRateSwapCaps {
            max_pending_swaps: 2,
            max_pending_swaps_120hz: Some(3),
            max_pending_swaps_90hz: Some(3),
            max_pending_swaps_72hz: Some(3),
        };
        // 16.67ms (60Hz) is above all thresholds → default.
        assert_eq!(
            max_pending_swaps_for_refresh_rate(Duration::from_micros(16667), &caps),
            2
        );
    }

    #[test]
    fn refresh_rate_swap_caps_falls_back_when_tier_is_none() {
        let caps = RefreshRateSwapCaps {
            max_pending_swaps: 2,
            max_pending_swaps_120hz: None,
            max_pending_swaps_90hz: None,
            max_pending_swaps_72hz: None,
        };
        // All tiers None → always default, regardless of interval.
        assert_eq!(
            max_pending_swaps_for_refresh_rate(Duration::from_micros(8000), &caps),
            2
        );
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "freebsd")))]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_window_button_layout_parse_standard() {
        let layout = WindowButtonLayout::parse("close,minimize:maximize").unwrap();
        assert_eq!(
            layout.left,
            [
                Some(WindowButton::Close),
                Some(WindowButton::Minimize),
                None
            ]
        );
        assert_eq!(layout.right, [Some(WindowButton::Maximize), None, None]);
    }

    #[test]
    fn test_window_button_layout_parse_right_only() {
        let layout = WindowButtonLayout::parse("minimize,maximize,close").unwrap();
        assert_eq!(layout.left, [None, None, None]);
        assert_eq!(
            layout.right,
            [
                Some(WindowButton::Minimize),
                Some(WindowButton::Maximize),
                Some(WindowButton::Close)
            ]
        );
    }

    #[test]
    fn test_window_button_layout_parse_left_only() {
        let layout = WindowButtonLayout::parse("close,minimize,maximize:").unwrap();
        assert_eq!(
            layout.left,
            [
                Some(WindowButton::Close),
                Some(WindowButton::Minimize),
                Some(WindowButton::Maximize)
            ]
        );
        assert_eq!(layout.right, [None, None, None]);
    }

    #[test]
    fn test_window_button_layout_parse_with_whitespace() {
        let layout = WindowButtonLayout::parse(" close , minimize : maximize ").unwrap();
        assert_eq!(
            layout.left,
            [
                Some(WindowButton::Close),
                Some(WindowButton::Minimize),
                None
            ]
        );
        assert_eq!(layout.right, [Some(WindowButton::Maximize), None, None]);
    }

    #[test]
    fn test_window_button_layout_parse_empty() {
        let layout = WindowButtonLayout::parse("").unwrap();
        assert_eq!(layout.left, [None, None, None]);
        assert_eq!(layout.right, [None, None, None]);
    }

    #[test]
    fn test_window_button_layout_parse_intentionally_empty() {
        let layout = WindowButtonLayout::parse(":").unwrap();
        assert_eq!(layout.left, [None, None, None]);
        assert_eq!(layout.right, [None, None, None]);
    }

    #[test]
    fn test_window_button_layout_parse_invalid_buttons() {
        let layout = WindowButtonLayout::parse("close,invalid,minimize:maximize,foo").unwrap();
        assert_eq!(
            layout.left,
            [
                Some(WindowButton::Close),
                Some(WindowButton::Minimize),
                None
            ]
        );
        assert_eq!(layout.right, [Some(WindowButton::Maximize), None, None]);
    }

    #[test]
    fn test_window_button_layout_parse_deduplicates_same_side_buttons() {
        let layout = WindowButtonLayout::parse("close,close,minimize").unwrap();
        assert_eq!(
            layout.right,
            [
                Some(WindowButton::Close),
                Some(WindowButton::Minimize),
                None
            ]
        );
        assert_eq!(layout.format(), ":close,minimize");
    }

    #[test]
    fn test_window_button_layout_parse_deduplicates_buttons_across_sides() {
        let layout = WindowButtonLayout::parse("close:maximize,close,minimize").unwrap();
        assert_eq!(layout.left, [Some(WindowButton::Close), None, None]);
        assert_eq!(
            layout.right,
            [
                Some(WindowButton::Maximize),
                Some(WindowButton::Minimize),
                None
            ]
        );

        let button_ids: Vec<_> = layout
            .left
            .iter()
            .chain(layout.right.iter())
            .flatten()
            .map(WindowButton::id)
            .collect();
        let unique_button_ids = button_ids.iter().copied().collect::<HashSet<_>>();
        assert_eq!(unique_button_ids.len(), button_ids.len());
        assert_eq!(layout.format(), "close:maximize,minimize");
    }

    #[test]
    fn test_window_button_layout_parse_gnome_style() {
        let layout = WindowButtonLayout::parse("close").unwrap();
        assert_eq!(layout.left, [None, None, None]);
        assert_eq!(layout.right, [Some(WindowButton::Close), None, None]);
    }

    #[test]
    fn test_window_button_layout_parse_elementary_style() {
        let layout = WindowButtonLayout::parse("close:maximize").unwrap();
        assert_eq!(layout.left, [Some(WindowButton::Close), None, None]);
        assert_eq!(layout.right, [Some(WindowButton::Maximize), None, None]);
    }

    #[test]
    fn test_window_button_layout_round_trip() {
        let cases = [
            "close:minimize,maximize",
            "minimize,maximize,close:",
            ":close",
            "close:",
            "close:maximize",
            ":",
        ];

        for case in cases {
            let layout = WindowButtonLayout::parse(case).unwrap();
            assert_eq!(layout.format(), case, "Round-trip failed for: {}", case);
        }
    }

    #[test]
    fn test_window_button_layout_linux_default() {
        let layout = WindowButtonLayout::linux_default();
        assert_eq!(layout.left, [None, None, None]);
        assert_eq!(
            layout.right,
            [
                Some(WindowButton::Minimize),
                Some(WindowButton::Maximize),
                Some(WindowButton::Close)
            ]
        );

        let round_tripped = WindowButtonLayout::parse(&layout.format()).unwrap();
        assert_eq!(round_tripped, layout);
    }

    #[test]
    fn test_window_button_layout_parse_all_invalid() {
        assert!(WindowButtonLayout::parse("asdfghjkl").is_err());
    }
}
