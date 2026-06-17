# research - zed render pipeline

Investigation into macOS input-to-pixel latency in Zed. Two distinct bugs
became visible during this work; they are unrelated and must not be
conflated.

## Context: two separate bugs

1. **#9307 — Low-power-mode performance throttle.** OS-side throttling
   surfaced/triggered by the linked SDK version. Fixed by patching the
   Mach-O `LC_BUILD_VERSION` SDK field. *Not* a render-pipeline issue.
2. **#26900 — Many frames of latency between input and render.** Endogenous
   to Zed's Metal present path / frame scheduling. *This* is the render
   pipeline issue, and it becomes far more apparent once #9307 is patched
   (removing the throttle exposes the buffering floor underneath).

---

## #9307 — Low-power-mode throttle (SDK version)

- **Issue:** https://github.com/zed-industries/zed/issues/9307
- **Symptom:** Under low power mode (LPM), Zed shows severe input lag /
  jittery scrolling / throttled multithreading.
- **Root cause (correlation, not fully proven mechanism):** Release binaries
  are linked against macOS **SDK 15.0**. Patching the `LC_BUILD_VERSION`
  SDK field to **26.0+** removes the hitching. Tested range: `15.0`–`15.5`
  broken; `26.0` and up fixed. `minos` stays at `11.0` (app still runs on
  older macOS).
- **Why it matters here:** This patch is what unmasked #26900.

### Patch (no rebuild): `patch-zed-perf.sh`

A thin script that patches `/Applications/Zed.app` in place:

1. `cp` not needed — operates in place.
2. `vtool -set-build-version macos 11.0 27.0 -replace` on
   `Contents/MacOS/zed`.
3. `xattr -cr` to strip quarantine.
4. Re-sign with a stable local identity (`AltTab Dev Signing`) so Gatekeeper
   only prompts once, preserving hardened runtime (`--options runtime`) and
   the repo's entitlements (`crates/zed/resources/zed.entitlements`).
   - No `--timestamp`: Apple's TSA rejects self-signed certs.
   - A stable identity is what makes the whole exercise worthwhile — ad-hoc
     (`-`) signing would re-prompt Gatekeeper on every re-run.

Defaults: SDK `27.0`, identity `AltTab Dev Signing`, in place. Re-run after
each Zed auto-update (update replaces the patched binary). See
`patch-zed-perf.sh` and the more general-purpose `patch-zed-sdk-version.sh`.

**Honest caveat:** The SDK bump is a *correlation*, not a proven mechanism.
Plausible candidates for what actually changes: display-link / ProMotion
pacing rewrite, thread QoS / scheduler hints, Metal driver path. None of
that is proven.

---

## #26900 — Render-pipeline latency (the buffering issue)

- **Issue:** https://github.com/zed-industries/zed/issues/26900
  "Many frames of latency between input and render"
- **Labels:** `area:performance`, `platform:macOS`, `state:needs repro`,
  `frequency:common`, `priority:P2`.
- **Symptom:** Reporter counts ≥4 frames between input and render on
  sweep-selection; typing feels sluggish. Pointer (composited by the window
  server) visibly outruns the app-rendered selection.
- **Hardware stratification:** Reports cluster on **Intel Macs / integrated
  GPUs** (Iris Plus, UHD 630). M-series Zed devs struggle to reproduce
  because their GPUs rarely let the pipeline back up. LPM on an M-series
  does to the GPU/display pipeline roughly what an Intel iGPU does all the
  time → same bottleneck, induced differently. This is why our patched
  (de-throttled) M-series setup now exposes #26900.
- **Measurement tooling (requested by `probably-neb`):** `typometer`
  (https://github.com/frarees/typometer). j-c-m posted quantified numbers
  with it; it is the right tool to attach hard data to the issue.

### Zed's macOS present path

All references in `crates/gpui_macos/`.

**Triple-buffered swap chain** — `src/metal_renderer.rs:161`
```rust
layer.set_maximum_drawable_count(3);
```
`CAMetalLayer` with 3 drawables = triple buffering. Keeps the GPU fed
(throughput) but allows up to ~2 rendered frames in flight before scanout.
This is the classic "selection trails the cursor" symptom. Highest-leverage
single knob, but — see Chromium comparison — *not* the parity lever.

**Present path** — `src/metal_renderer.rs:461, 491-498`
```rust
let drawable = layer.next_drawable();        // :461
...
if self.presents_with_transaction {
    command_buffer.commit();
    command_buffer.wait_until_scheduled();
    drawable.present();                       // high-latency sync path
} else {
    command_buffer.present_drawable(drawable); // vsync-aligned, used by default
    command_buffer.commit();
}
```
Default steady-state uses the low-latency `present_drawable` branch. The
high-latency `presents_with_transaction` path is only flipped on transiently
for window activation (`window.rs:2528-2531`) and resize / `display_layer`
(`window.rs:2636-2645`), then flipped back. Not the steady-state culprit.

**Frame cadence** — `src/display_link.rs` + `src/window.rs`
- `CVDisplayLink` fires → `dispatch_source_data_add` onto the main queue →
  `step()` (`window.rs:2648`) → `request_frame_callback`.
- Rendering *is* vsync-paced (not a fixed timer) — good.
- Known caveat already flagged in `display_link.rs` (Drop impl): `CVDisplayLink`
  has more jitter than `CADisplayLink`, and the main-queue hop adds scheduling
  indirection; "we might want to upgrade to CADisplayLink, but that requires
  dropping old macOS support." Minor contributor next to the present model.

---

## The VS Code comparison (apples to apples)

VS Code = Electron = Chromium's default macOS path, **untuned**. It still
beats Zed by ~1-2 frames. That fact is the most diagnostic signal in the
whole investigation, and reading Chromium's source (sparse-cloned into
`REFERENCE/chromium/`, cones `ui/accelerated_widget_mac` + `ui/gl`) shows
*why*.

### Chromium's macOS present path

- GPU process renders into an **`IOSurface`**.
- `CARendererLayerTree::ScheduleCALayer` → `CommitScheduledCALayers`
  (`REFERENCE/chromium/ui/accelerated_widget_mac/ca_renderer_layer_tree.mm:326-340`)
  hands the IOSurface to a **`CALayer`** and lets **CoreAnimation composite +
  present on vsync**.
- **Grep across all of `ui/accelerated_widget_mac` + `ui/gl`** for
  `maximumDrawableCount`, `framebufferOnly`, `presentsWithTransaction`,
  `presentDrawable`, `nextDrawable`: **zero hits.** Chromium does **not**
  drive its own Metal swap chain in the window/widget path at all.
- Presentation feedback: `ca_layer_tree_coordinator.mm:281-289` returns a
  `gfx::PresentationFeedback` with flags `kHWCompletion | kVSync` plus
  `ready_timestamp` / `latch_timestamp` / `display_time` into `viz::Display`.
  That feedback loop is what lets the `BeginFrame` scheduler keep input
  tight to the imminent scanout.

### Why that means VS Code is inherently lower-latency

This is not "Chromium tuned the constant better." It is a **different present
model**:

- **IOSurface → CoreAnimation (Chromium):** hand the window server the newest
  pixels and walk away. CoreAnimation can **late-latch** — right up until
  scanout, a fresher IOSurface (one including the just-arrived keystroke)
  replaces the one queued to show. No app-side queue fills up.
- **`present_drawable` into a 3-deep queue (Zed):** once committed, that
  drawable is queued behind up to two others. You cannot late-latch. Under
  LPM / Intel-iGPU conditions where the producer stays ahead, the drawables
  pile up and the selection trails.

**Conclusion:** An untuned Electron app wins *specifically because* it
delegates presentation to CoreAnimation rather than driving Metal itself.
Zed's `maximumDrawableCount=3` is a real, measurable contributor but is **not
the parity lever**. Parity would require either:

- (a) moving to an IOSurface → CALayer handoff (large GPUI change), or
- (b) adding a late-latch + presentation-feedback loop (the Chromium
  `BeginFrame` + `EventLatency` design).

Dropping to `maximumDrawable_count=2` would shave a frame; it would not
close the gap to VS Code.

---

## Where the latency lives (revised picture)

| Stage | Zed | Chromium/VS Code |
|---|---|---|
| Swap chain | `maximumDrawableCount=3`, app-driven `present_drawable` | No app swap chain; IOSurface → CoreAnimation |
| Present model | Queued drawables, no late-latch | CoreAnimation late-latches newest IOSurface |
| Vsync source | `CVDisplayLink` → dispatch_source → main queue | CoreAnimation-driven + `PresentationFeedback` |
| Input→frame coupling | `cx.notify()` → next scheduled display-link tick | `BeginFrame` scheduler ties input to imminent scanout |

The extra frame(s) in Zed live in: (1) the app-driven swap queue, and (2)
the decoupling of "input landed" from "next frame incorporates it" via the
display-link-tick + dispatch hop.

---

## Open questions / next steps

- **Measure.** Run `typometer` on patched Zed vs VS Code at 60Hz + LPM.
  A number beats every theory; attach to #26900.
- **Drawable-count A/B.** Local one-line patch `metal_renderer.rs:161`
  (`3` → `2`), debug build, measure. Expect a small win, not parity —
  validates the "not the parity lever" claim with data.
- **Input coupling.** Trace `cx.notify()` → frame-request in
  `crates/gpui/src/window.rs` / the scheduler to see whether input arrival
  is already on the critical path to the next frame, or whether there's an
  explicit vsync wait that's the real culprit.
- **Optional: pull `cc/scheduler` (BeginFrame) + `viz/`** from the Chromium
  reference to ground the input-coupling side as firmly as the present side
  is now.

---

## Key references

**Zed (this repo):**
- `crates/gpui_macos/src/metal_renderer.rs:161` — `set_maximum_drawable_count(3)`
- `crates/gpui_macos/src/metal_renderer.rs:461, 491-498` — drawable acquire + present path
- `crates/gpui_macos/src/display_link.rs` — `CVDisplayLink` → dispatch_source
- `crates/gpui_macos/src/window.rs:2648` — `step()` frame callback
- `crates/gpui_macos/src/window.rs:2528-2531, 2636-2645` — transient `presents_with_transaction`
- `crates/zed/resources/zed.entitlements` — entitlements used by the patch script

**Chromium reference (`REFERENCE/chromium/`):**
- `ui/accelerated_widget_mac/ca_renderer_layer_tree.mm:326-340` — IOSurface → CALayer commit
- `ui/accelerated_widget_mac/ca_layer_tree_coordinator.mm:281-289` — `PresentationFeedback` (kHWCompletion | kVSync)
- `ui/accelerated_widget_mac/display_ca_layer_tree.mm` — viz → CA bridge

**Issues:**
- https://github.com/zed-industries/zed/issues/9307 — LPM throttle (SDK)
- https://github.com/zed-industries/zed/issues/26900 — input/render latency (buffering)
- https://github.com/frarees/typometer — measurement tool
