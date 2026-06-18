use scheduler::Instant;
use std::time::Duration;

#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {
    fn CACurrentMediaTime() -> f64;
}

pub(crate) fn media_time_to_instant(media_time: f64) -> Instant {
    let now = Instant::now();
    let now_media_time = unsafe { CACurrentMediaTime() };
    let delta = media_time - now_media_time;

    if delta >= 0.0 {
        duration_from_seconds(delta)
            .map(|duration| now + duration)
            .unwrap_or(now)
    } else {
        duration_from_seconds(-delta)
            .and_then(|duration| now.checked_sub(duration))
            .unwrap_or(now)
    }
}

fn duration_from_seconds(seconds: f64) -> Option<Duration> {
    if seconds.is_finite() {
        Duration::try_from_secs_f64(seconds).ok()
    } else {
        None
    }
}
