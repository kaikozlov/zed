use anyhow::Result;
use core_graphics::display::CGDirectDisplayID;
use dispatch2::{
    _dispatch_source_type_data_add, DispatchObject, DispatchQueue, DispatchRetained, DispatchSource,
};
use gpui::{BeginFrameArgs, BeginFrameId, PossibleDeadline, PossibleDeadlines};
use gpui_util::ResultExt;
use mach2::mach_time::{mach_absolute_time, mach_timebase_info, mach_timebase_info_data_t};
use parking_lot::Mutex;
use scheduler::Instant;
use std::{
    ffi::c_void,
    sync::{Arc, OnceLock},
    time::Duration,
};

/// VSync parameters captured atomically from the `CVDisplayLink` output
/// callback, mirroring Chromium's `VSyncParamsMac`
/// (`ui/display/mac/display_link_mac.h:27-39`).
///
/// Chromium distinguishes between *callback* timing (when the display-link
/// fired) and *display* timing (when the frame will actually be shown):
/// - `callback_timebase` / `callback_interval` — the raw callback arrival
///   time and inter-callback interval (`VSyncParamsMac.callback_timebase`,
///   `display_link_mac.h:30-31`).
/// - `display_timebase` / `display_interval` — the predicted display time
///   and refresh period (`VSyncParamsMac.display_timebase`,
///   `display_link_mac.h:35-36`).
///
/// In Zed's current code these are spread across three separate atomics
/// (`latest_output_host_time`, `latest_frame_interval_ns`,
/// `latest_sequence_number`), which creates torn-read risk: the display-link
/// thread could update one field (e.g. `host_time`) before another (e.g.
/// `sequence_number`), so `latest_timing()` could read a host time from tick
/// N+1 with a sequence number from tick N. Encapsulating all params in a
/// single `Mutex<VSyncParams>` snapshot eliminates that risk, matching how
/// Chromium's `DisplayLinkMacSharedState` publishes a consistent snapshot to
/// all registered callbacks.
#[derive(Clone, Default)]
struct VSyncParams {
    /// Monotonic sequence counter, incremented on each vsync tick. Matches
    /// Chromium's internal frame counter that feeds `BeginFrameId`.
    sequence_number: u64,
    /// The host time of the `CVTimeStamp` when the callback fired
    /// (`current_time` parameter of `CVDisplayLinkOutputCallback`), or `0`
    /// when the callback didn't carry a valid host time. Mirrors
    /// `VSyncParamsMac.callback_timebase` (`display_link_mac.h:30`).
    callback_host_time: u64,
    /// The host time of the `CVTimeStamp` indicating when the frame will be
    /// displayed (`output_time` parameter), or `0` when invalid. Mirrors
    /// `VSyncParamsMac.display_timebase` (`display_link_mac.h:35`).
    display_host_time: u64,
    /// The display refresh period in nanoseconds, or `0` when the callback
    /// didn't carry a valid `video_refresh_period`. Mirrors
    /// `VSyncParamsMac.display_interval` (`display_link_mac.h:36`).
    interval_ns: u64,
}

impl VSyncParams {
    fn is_valid(&self) -> bool {
        self.display_host_time != 0
    }

    fn interval(&self) -> Option<Duration> {
        match self.interval_ns {
            0 => None,
            ns => Some(Duration::from_nanos(ns)),
        }
    }
}

pub struct DisplayLink {
    display_link: Option<sys::DisplayLink>,
    frame_requests: DispatchRetained<DispatchSource>,
    source_id: u64,
    latest_params: Arc<Mutex<VSyncParams>>,
    _callback_context: Box<DisplayLinkCallbackContext>,
}

struct DisplayLinkCallbackContext {
    frame_requests: *const DispatchSource,
    latest_params: Arc<Mutex<VSyncParams>>,
}

#[derive(Clone, Debug)]
pub struct DisplayLinkTiming {
    pub begin_frame: BeginFrameArgs,
    pub predicted_display_time: Instant,
    pub frame_interval: Option<Duration>,
    pub frame_deadline: Instant,
}

impl DisplayLink {
    pub fn new(
        display_id: CGDirectDisplayID,
        data: *mut c_void,
        callback: extern "C" fn(*mut c_void),
        initial_sequence_number: u64,
    ) -> Result<DisplayLink> {
        unsafe extern "C" fn display_link_callback(
            _display_link_out: *mut sys::CVDisplayLink,
            current_time: *const sys::CVTimeStamp,
            output_time: *const sys::CVTimeStamp,
            _flags_in: i64,
            _flags_out: *mut i64,
            callback_context: *mut c_void,
        ) -> i32 {
            unsafe {
                let callback_context = &*(callback_context as *const DisplayLinkCallbackContext);

                // Capture the callback host time (when the display-link fired).
                // Maps to `VSyncParamsMac.callback_timebase`
                // (`display_link_mac.h:30`). The `current_time` parameter
                // carries the callback arrival timestamp.
                let callback_host_time = current_time
                    .as_ref()
                    .filter(|ts| ts.flags & sys::kCVTimeStampHostTimeValid != 0)
                    .map(|ts| ts.host_time)
                    .unwrap_or(0);

                // Capture the display host time (when the frame will be shown).
                // Maps to `VSyncParamsMac.display_timebase`
                // (`display_link_mac.h:35`).
                let display_host_time = output_time
                    .as_ref()
                    .filter(|ts| ts.flags & sys::kCVTimeStampHostTimeValid != 0)
                    .map(|ts| ts.host_time)
                    .unwrap_or(0);

                // Capture the display refresh period.
                // Maps to `VSyncParamsMac.display_interval`
                // (`display_link_mac.h:36`).
                let interval_ns = output_time
                    .as_ref()
                    .filter(|ts| {
                        ts.flags & sys::kCVTimeStampVideoRefreshPeriodValid != 0
                            && ts.video_time_scale > 0
                            && ts.video_refresh_period > 0
                    })
                    .map(|ts| {
                        let interval_ns = u128::try_from(ts.video_refresh_period)
                            .unwrap_or_default()
                            * 1_000_000_000u128
                            / u128::try_from(ts.video_time_scale).unwrap_or(1);
                        interval_ns.min(u128::from(u64::MAX)) as u64
                    })
                    .unwrap_or(0);

                // Publish a single consistent snapshot. All fields are
                // captured under one lock so `latest_timing()` on the main
                // thread reads a tear-free view from a single vsync tick.
                let mut params = callback_context.latest_params.lock();
                params.sequence_number = params.sequence_number.wrapping_add(1);
                params.callback_host_time = callback_host_time;
                params.display_host_time = display_host_time;
                params.interval_ns = interval_ns;
                drop(params);

                (*callback_context.frame_requests).merge_data(1);
                0
            }
        }

        unsafe {
            let frame_requests = DispatchSource::new(
                &raw const _dispatch_source_type_data_add as *mut _,
                0,
                0,
                Some(DispatchQueue::main()),
            );
            frame_requests.set_context(data);
            frame_requests.set_event_handler_f(callback);
            frame_requests.resume();

            let source_id = u64::from(display_id);
            let initial_params = VSyncParams {
                sequence_number: initial_sequence_number,
                callback_host_time: 0,
                display_host_time: 0,
                interval_ns: 0,
            };
            let latest_params = Arc::new(Mutex::new(initial_params));
            let callback_context = Box::new(DisplayLinkCallbackContext {
                frame_requests: &*frame_requests as *const DispatchSource,
                latest_params: latest_params.clone(),
            });
            let display_link = sys::DisplayLink::new(
                display_id,
                display_link_callback,
                &*callback_context as *const DisplayLinkCallbackContext as *mut c_void,
            )?;

            Ok(Self {
                display_link: Some(display_link),
                frame_requests,
                source_id,
                latest_params,
                _callback_context: callback_context,
            })
        }
    }

    /// Returns the predicted display time from the latest vsync tick, or
    /// `None` if no valid timing has been received yet. Kept as a public
    /// accessor for consumers that only need the display time without the
    /// full `DisplayLinkTiming` construction.
    #[allow(dead_code)]
    pub fn latest_output_time(&self) -> Option<Instant> {
        let params = self.latest_params.lock();
        if !params.is_valid() {
            return None;
        }
        host_time_to_instant(params.display_host_time)
    }

    pub fn source_id(&self) -> u64 {
        self.source_id
    }

    pub fn latest_timing(&self) -> Option<DisplayLinkTiming> {
        let params = self.latest_params.lock();
        if !params.is_valid() {
            return None;
        }
        let sequence_number = params.sequence_number;
        let interval = params.interval().unwrap_or(Duration::from_micros(16667));
        let predicted_display_time = host_time_to_instant(params.display_host_time)?;

        // Derive frame_time from the display-link timebase rather than
        // back-computing from `predicted_display_time - interval`. When the
        // callback provides `callback_host_time` (callback_timebase), use it
        // as the frame_time — this is when the display-link actually fired,
        // and the delta from callback to display is the real first-present
        // delta, not an assumed one-interval. Falls back to
        // `predicted_display_time - interval` when callback timing is
        // unavailable.
        let (frame_time, first_present_delta) = if params.callback_host_time != 0
            && params.callback_host_time < params.display_host_time
        {
            let callback_time =
                host_time_to_instant(params.callback_host_time).unwrap_or(predicted_display_time);
            let delta = predicted_display_time.saturating_duration_since(callback_time);
            (callback_time, delta)
        } else {
            let frame_time = predicted_display_time
                .checked_sub(interval)
                .unwrap_or(predicted_display_time);
            (frame_time, interval)
        };

        let begin_frame = BeginFrameArgs {
            id: BeginFrameId {
                source_id: self.source_id,
                sequence_number,
            },
            frame_time,
            deadline: predicted_display_time,
            interval,
            missed: false,
            possible_deadlines: Some(possible_deadlines_for_frame(
                sequence_number,
                interval,
                first_present_delta,
            )),
        };
        Some(DisplayLinkTiming {
            begin_frame,
            predicted_display_time,
            frame_interval: params.interval(),
            frame_deadline: predicted_display_time,
        })
    }

    pub fn start(&mut self) -> Result<()> {
        unsafe {
            self.display_link.as_mut().unwrap().start()?;
        }
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        unsafe {
            self.display_link.as_mut().unwrap().stop()?;
        }
        Ok(())
    }
}

fn possible_deadlines_for_frame(
    sequence_number: u64,
    interval: Duration,
    first_present_delta: Duration,
) -> PossibleDeadlines {
    const FORWARD_VSYNC_CANDIDATES: u32 = 3;

    let deadlines = (0..FORWARD_VSYNC_CANDIDATES)
        .map(|candidate| {
            let present_delta = first_present_delta + interval.saturating_mul(candidate);
            PossibleDeadline {
                vsync_id: i64::try_from(sequence_number.saturating_add(u64::from(candidate)))
                    .unwrap_or(i64::MAX),
                latch_delta: present_delta,
                present_delta,
            }
        })
        .collect();

    PossibleDeadlines {
        os_preferred_index: 0,
        deadlines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn possible_deadlines_include_os_preferred_and_forward_vsync_candidates() {
        let interval = Duration::from_millis(8);
        let deadlines = possible_deadlines_for_frame(42, interval, Duration::from_millis(10));

        assert_eq!(deadlines.os_preferred_index, 0);
        assert_eq!(
            deadlines.deadlines,
            vec![
                PossibleDeadline {
                    vsync_id: 42,
                    latch_delta: Duration::from_millis(10),
                    present_delta: Duration::from_millis(10),
                },
                PossibleDeadline {
                    vsync_id: 43,
                    latch_delta: Duration::from_millis(18),
                    present_delta: Duration::from_millis(18),
                },
                PossibleDeadline {
                    vsync_id: 44,
                    latch_delta: Duration::from_millis(26),
                    present_delta: Duration::from_millis(26),
                },
            ]
        );
        assert_eq!(
            deadlines.os_preferred_deadline(),
            Some(&PossibleDeadline {
                vsync_id: 42,
                latch_delta: Duration::from_millis(10),
                present_delta: Duration::from_millis(10),
            })
        );
    }

    #[test]
    fn possible_deadlines_clamp_vsync_id_on_overflow() {
        let deadlines = possible_deadlines_for_frame(
            u64::MAX,
            Duration::from_millis(8),
            Duration::from_millis(8),
        );

        assert_eq!(deadlines.deadlines[0].vsync_id, i64::MAX);
        assert_eq!(deadlines.deadlines[1].vsync_id, i64::MAX);
    }
}

impl Drop for DisplayLink {
    fn drop(&mut self) {
        self.stop().log_err();
        // We see occasional segfaults on the CVDisplayLink thread.
        //
        // It seems possible that this happens because CVDisplayLinkRelease releases the CVDisplayLink
        // on the main thread immediately, but the background thread that CVDisplayLink uses for timers
        // is still accessing it.
        //
        // We might also want to upgrade to CADisplayLink, but that requires dropping old macOS support.
        std::mem::forget(self.display_link.take());
        self.frame_requests.cancel();
    }
}

fn host_time_to_instant(host_time: u64) -> Option<Instant> {
    if host_time == 0 {
        return None;
    }

    let now = Instant::now();
    let now_host_time = unsafe { mach_absolute_time() };
    if host_time >= now_host_time {
        Some(now + mach_duration(host_time - now_host_time))
    } else {
        Some(
            now.checked_sub(mach_duration(now_host_time - host_time))
                .unwrap_or(now),
        )
    }
}

fn mach_duration(ticks: u64) -> Duration {
    static TIMEBASE: OnceLock<(u64, u64)> = OnceLock::new();
    let (numerator, denominator) = *TIMEBASE.get_or_init(|| unsafe {
        let mut info = mach_timebase_info_data_t { numer: 0, denom: 0 };
        mach_timebase_info(&mut info);
        (u64::from(info.numer), u64::from(info.denom.max(1)))
    });

    let nanos = u128::from(ticks) * u128::from(numerator) / u128::from(denominator);
    Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64)
}

mod sys {
    //! Derived from display-link crate under the following license:
    //! <https://github.com/BrainiumLLC/display-link/blob/master/LICENSE-MIT>
    //! Apple docs: [CVDisplayLink](https://developer.apple.com/documentation/corevideo/cvdisplaylinkoutputcallback?language=objc)
    #![allow(dead_code, non_upper_case_globals)]

    use anyhow::Result;
    use core_graphics::display::CGDirectDisplayID;
    use foreign_types::{ForeignType, foreign_type};
    use std::{
        ffi::c_void,
        fmt::{self, Debug, Formatter},
    };

    #[derive(Debug)]
    pub enum CVDisplayLink {}

    foreign_type! {
        pub unsafe type DisplayLink {
            type CType = CVDisplayLink;
            fn drop = CVDisplayLinkRelease;
            fn clone = CVDisplayLinkRetain;
        }
    }

    impl Debug for DisplayLink {
        fn fmt(&self, formatter: &mut Formatter) -> fmt::Result {
            formatter
                .debug_tuple("DisplayLink")
                .field(&self.as_ptr())
                .finish()
        }
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub(crate) struct CVTimeStamp {
        pub version: u32,
        pub video_time_scale: i32,
        pub video_time: i64,
        pub host_time: u64,
        pub rate_scalar: f64,
        pub video_refresh_period: i64,
        pub smpte_time: CVSMPTETime,
        pub flags: u64,
        pub reserved: u64,
    }

    pub type CVTimeStampFlags = u64;

    pub const kCVTimeStampVideoTimeValid: CVTimeStampFlags = 1 << 0;
    pub const kCVTimeStampHostTimeValid: CVTimeStampFlags = 1 << 1;
    pub const kCVTimeStampSMPTETimeValid: CVTimeStampFlags = 1 << 2;
    pub const kCVTimeStampVideoRefreshPeriodValid: CVTimeStampFlags = 1 << 3;
    pub const kCVTimeStampRateScalarValid: CVTimeStampFlags = 1 << 4;
    pub const kCVTimeStampTopField: CVTimeStampFlags = 1 << 16;
    pub const kCVTimeStampBottomField: CVTimeStampFlags = 1 << 17;
    pub const kCVTimeStampVideoHostTimeValid: CVTimeStampFlags =
        kCVTimeStampVideoTimeValid | kCVTimeStampHostTimeValid;
    pub const kCVTimeStampIsInterlaced: CVTimeStampFlags =
        kCVTimeStampTopField | kCVTimeStampBottomField;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    pub(crate) struct CVSMPTETime {
        pub subframes: i16,
        pub subframe_divisor: i16,
        pub counter: u32,
        pub time_type: u32,
        pub flags: u32,
        pub hours: i16,
        pub minutes: i16,
        pub seconds: i16,
        pub frames: i16,
    }

    pub type CVSMPTETimeType = u32;

    pub const kCVSMPTETimeType24: CVSMPTETimeType = 0;
    pub const kCVSMPTETimeType25: CVSMPTETimeType = 1;
    pub const kCVSMPTETimeType30Drop: CVSMPTETimeType = 2;
    pub const kCVSMPTETimeType30: CVSMPTETimeType = 3;
    pub const kCVSMPTETimeType2997: CVSMPTETimeType = 4;
    pub const kCVSMPTETimeType2997Drop: CVSMPTETimeType = 5;
    pub const kCVSMPTETimeType60: CVSMPTETimeType = 6;
    pub const kCVSMPTETimeType5994: CVSMPTETimeType = 7;

    pub type CVSMPTETimeFlags = u32;

    pub const kCVSMPTETimeValid: CVSMPTETimeFlags = 1 << 0;
    pub const kCVSMPTETimeRunning: CVSMPTETimeFlags = 1 << 1;

    pub type CVDisplayLinkOutputCallback = unsafe extern "C" fn(
        display_link_out: *mut CVDisplayLink,
        // A pointer to the current timestamp. This represents the timestamp when the callback is called.
        current_time: *const CVTimeStamp,
        // A pointer to the output timestamp. This represents the timestamp for when the frame will be displayed.
        output_time: *const CVTimeStamp,
        // Unused
        flags_in: i64,
        // Unused
        flags_out: *mut i64,
        // A pointer to app-defined data.
        display_link_context: *mut c_void,
    ) -> i32;

    #[link(name = "CoreFoundation", kind = "framework")]
    #[link(name = "CoreVideo", kind = "framework")]
    #[allow(improper_ctypes, unknown_lints, clippy::duplicated_attributes)]
    unsafe extern "C" {
        pub fn CVDisplayLinkCreateWithActiveCGDisplays(
            display_link_out: *mut *mut CVDisplayLink,
        ) -> i32;
        pub fn CVDisplayLinkSetCurrentCGDisplay(
            display_link: &mut DisplayLinkRef,
            display_id: u32,
        ) -> i32;
        pub fn CVDisplayLinkSetOutputCallback(
            display_link: &mut DisplayLinkRef,
            callback: CVDisplayLinkOutputCallback,
            user_info: *mut c_void,
        ) -> i32;
        pub fn CVDisplayLinkStart(display_link: &mut DisplayLinkRef) -> i32;
        pub fn CVDisplayLinkStop(display_link: &mut DisplayLinkRef) -> i32;
        pub fn CVDisplayLinkRelease(display_link: *mut CVDisplayLink);
        pub fn CVDisplayLinkRetain(display_link: *mut CVDisplayLink) -> *mut CVDisplayLink;
    }

    impl DisplayLink {
        /// Apple docs: [CVDisplayLinkCreateWithCGDisplay](https://developer.apple.com/documentation/corevideo/1456981-cvdisplaylinkcreatewithcgdisplay?language=objc)
        pub unsafe fn new(
            display_id: CGDirectDisplayID,
            callback: CVDisplayLinkOutputCallback,
            user_info: *mut c_void,
        ) -> Result<Self> {
            unsafe {
                let mut display_link: *mut CVDisplayLink = 0 as _;

                let code = CVDisplayLinkCreateWithActiveCGDisplays(&mut display_link);
                anyhow::ensure!(code == 0, "could not create display link, code: {}", code);

                let mut display_link = DisplayLink::from_ptr(display_link);

                let code = CVDisplayLinkSetOutputCallback(&mut display_link, callback, user_info);
                anyhow::ensure!(code == 0, "could not set output callback, code: {}", code);

                let code = CVDisplayLinkSetCurrentCGDisplay(&mut display_link, display_id);
                anyhow::ensure!(
                    code == 0,
                    "could not assign display to display link, code: {}",
                    code
                );

                Ok(display_link)
            }
        }
    }

    impl DisplayLinkRef {
        /// Apple docs: [CVDisplayLinkStart](https://developer.apple.com/documentation/corevideo/1457193-cvdisplaylinkstart?language=objc)
        pub unsafe fn start(&mut self) -> Result<()> {
            unsafe {
                let code = CVDisplayLinkStart(self);
                anyhow::ensure!(code == 0, "could not start display link, code: {}", code);
                Ok(())
            }
        }

        /// Apple docs: [CVDisplayLinkStop](https://developer.apple.com/documentation/corevideo/1457281-cvdisplaylinkstop?language=objc)
        pub unsafe fn stop(&mut self) -> Result<()> {
            unsafe {
                let code = CVDisplayLinkStop(self);
                anyhow::ensure!(code == 0, "could not stop display link, code: {}", code);
                Ok(())
            }
        }
    }
}
