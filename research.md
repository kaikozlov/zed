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

## Current implementation in this branch

This branch now uses a macOS renderer path that implements the
Chromium-style resource/presentation model inside Zed's existing single-process
renderer:

```sh
cargo run -p zed
```

The legacy app-owned `CAMetalLayer` path remains available as an escape hatch:

```sh
ZED_MACOS_LEGACY_METAL_LAYER=1 cargo run -p zed
```

### What this changes

**Backing layer selection** — `crates/gpui_macos/src/metal_renderer.rs`

- Default path: Chromium-style CoreAnimation layer tree. When CA/IOSurface
  initialization fails, Zed falls back to the legacy `CAMetalLayer` path.
- Legacy escape hatch: `ZED_MACOS_LEGACY_METAL_LAYER=1` restores app-driven
  `CAMetalLayer` `present_drawable`.
- The Chromium pipeline path creates a Chromium-style CoreAnimation layer tree.
  When the private API is available, this is a `CAContext` with a root content
  `CALayer`, exposed to the NSView via a `CALayerHost`. If those runtime
  interfaces are unavailable, it falls back to the direct local `CALayer`
  IOSurface path.
- `window.rs` now asks the renderer for a generic backing layer and routes
  contents-scale updates through the renderer, so Retina scale handling works
  for `CAMetalLayer`, direct `CALayer`, and `CALayerHost`.

**IOSurface-backed render targets**

- Allocates a 3-buffer IOSurface queue, matching Chromium's default
  `SkiaOutputDeviceBufferQueue` resource shape.
- Each buffer is BGRA and has a Metal render-target texture created directly
  from the IOSurface via `newTextureWithDescriptor:iosurface:plane:`.
- Rendering still uses GPUI's existing primitive encoder; the target texture is
  swapped from a `CAMetalDrawable` texture to an IOSurface-backed Metal texture.
  There is no CPU readback and no texture copy in the intended path.

**Pending swap cap / backpressure**

- The queue allows at most 2 pending swaps over 3 buffers, matching Chromium's
  "number of buffers minus one" behavior.
- If both pending slots are full, the renderer defers the frame instead of
  taking a third buffer.
- When a pending IOSurface frame completes GPU work and is handed to the
  `CALayer`, the renderer asks GPUI for a forced replacement frame if one was
  deferred.
- GPUI now has a distinct `SwapCompletionFeedback` platform callback, separate
  from `PresentationFeedback`. The macOS IOSurface path emits it immediately
  after the `CALayer.contents` transaction commits, matching Chromium's
  `DidReceiveSwapBuffersAck()` boundary. GPUI reserves pending platform swap
  state before calling platform draw, matching Chromium's `DidSwapBuffers()`
  before `SwapBuffers()` ordering for synchronous-ack safety, and rolls that
  reservation back when the platform reports `Deferred` or `Skipped`. The ack
  decrements submitted swaps. `PlatformDrawResult::Deferred` preserves
  `needs_present` when the IOSurface queue has no available buffer, so a
  backpressured draw does not masquerade as a completed swap.
- GPUI now also mirrors Chromium's GPU-busy gate: the macOS IOSurface presenter
  exposes its max pending swaps (`IOSURFACE_BUFFER_COUNT - 1`), and the
  scheduler blocks dirty/present work while submitted swaps are at that limit,
  but it no longer unsubscribes the platform BeginFrame source while blocked.
  This matches Chromium's split between `SetNeedsBeginFrame` and
  `SetIsGpuBusy`: the source stays active while GPU-busy, incoming BeginFrames
  are throttled, and GPU availability replays the stored pending BeginFrame.
  This gate is checked both when a BeginFrame arrives and when a delayed
  regular/scroll deadline actually fires, matching Chromium's
  `AttemptDrawAndSwap()` check before `DrawAndSwap()`. A swap ack lowers the
  pending count, re-evaluates BeginFrame subscription, and if it clears a
  backpressured state with work still pending, GPUI replays the exact
  `RequestFrameOptions` that were blocked by GPU-busy backpressure. This mirrors
  Chromium's `pending_begin_frame_args_` / `OnGpuNoLongerBusy()` path after
  `DidReceiveSwapBuffersAck()`.
- Important runtime finding: do **not** use `IOSurfaceIsInUse` as the hot-path
  reuse gate in this local process. Without Chromium's full CA/viz presentation
  feedback, that check can report surfaces unavailable long after the local
  renderer can safely rotate to them, starving the 3-buffer queue and producing
  visibly low FPS/choppy presentation. This path now relies on its local
  pending/current-buffer tracking instead.

**Buffer damage tracking**

- The IOSurface queue now tracks accumulated damage per buffer, matching
  Chromium's `BufferQueue` invariant: a buffer starts fully damaged, new frame
  damage is unioned into every non-current buffer, the selected buffer exposes
  its accumulated damage for the draw, and that damage is cleared once the swap
  is queued.
- The Metal pass is ready for partial damage: full damage keeps the previous
  clear-and-redraw behavior, while non-full damage loads existing IOSurface
  contents and applies a Metal scissor rect for the damaged area.
- `Scene` now carries a damage region. Global refreshes and uncertain view
  invalidations remain conservative/full-frame, but cached dirty views damage
  the union of their previous clipped bounds and current clipped bounds.
  Newly painted primitives also contribute their clipped primitive bounds to
  scene damage. Cached paint replay suppresses primitive damage so reused
  content does not spuriously dirty the frame. The macOS IOSurface path encloses
  fractional scaled-pixel damage with floor-origin/ceil-max device-pixel bounds
  before feeding the presenter, so partial redraw scissoring never under-covers
  changed content at Retina scale boundaries.

**Presentation handoff**

- GPU completion dispatches back to the main queue.
- The completed IOSurface is assigned to the content layer's
  `CALayer.contents`. In the CAContext path, that content layer is the root
  layer attached to the `CAContext`; the NSView displays it through
  `CALayerHost.contextId`, matching Chromium's remote-layer topology.
- The IOSurface handoff now checks the completed Metal command buffer status
  before touching CoreAnimation. Failed GPU work produces failed swap and
  presentation feedback, releases backpressure, and requests a replacement
  forced frame instead of latching a stale or partially rendered IOSurface.
- When the drawable size changes, the CAContext/CALayerHost path now marks the
  context as resized and recreates the CAContext immediately before the next
  IOSurface frame handoff. This mirrors Chromium's resize-time CAContext
  replacement so the new-size frame and host context id update are committed
  together. When the private fence-port selectors are available, the old
  CAContext now creates and installs a fence mach port before replacement, and
  Zed retains a small queue of recent ports using Chromium's default cap of
  four. Unlike Chromium's browser/GPU split, the port is retained locally rather
  than sent across a process boundary to a browser-side `DisplayCALayerTree`.
- Resize no longer resets the IOSurface pending-swap count while older GPU
  completions may still be in flight. The presenter now generations each
  IOSurface queue allocation; stale pre-resize completions still acknowledge
  their swap so GPUI backpressure clears, but they do not update the current
  buffer index or overwrite the resized layer contents.
- The contents swap is wrapped in a `CATransaction` with actions disabled so
  CoreAnimation does not implicitly animate or otherwise delay the frame update.
- This moves Zed off the app-owned `CAMetalLayer` drawable queue in the
  Chromium pipeline path, which is the key Chromium-vs-Zed gap identified above.

**Scheduler-visible presentation feedback**

- GPUI now has a `PresentationFeedback` platform callback with Chromium-like
  fields: `ready_time`, `latch_time`, `display_time`, `interval`, `presented`,
  `vsync`, and `hardware_completion`.
- The macOS renderer reports feedback from both paths:
  - legacy `CAMetalLayer`: via `MTLDrawable.addPresentedHandler`, converting
    the drawable's CoreAnimation `presentedTime` into `scheduler::Instant`;
  - Chromium pipeline `CALayer`/IOSurface path: records GPU-ready time when the
    Metal command buffer completes, uses the main-thread `CALayer.contents`
    transaction as latch time, and estimates display time from the latest
    display-link target timestamp using Chromium's 1.5 ms latch-buffer model.
- The IOSurface path now preserves Chromium's callback ordering at the local
  CA layer-tree commit point: swap completion is delivered first, then
  presentation feedback is delivered with ready/latch/display timestamps. This
  matches Chromium's `CALayerTreeCoordinator::CommitPresentedFrameToCA()`
  boundary, where `SwapCompletionCallback` runs after the CALayer tree is
  committed. Holding GPUI backpressure until a later `CATransaction` completion
  block produced visibly choppy pacing because the two-pending-swap gate could
  remain closed after the layer contents had already been committed.
- When the private macOS `+[CATransaction addCommitHandler:forPhase:]` API is
  present, the local IOSurface path now uses the post-commit phase for the
  swap/presentation callbacks. This mirrors Chromium's
  `CATransactionCoordinator` post-commit timing. The implementation keeps an
  immediate fallback after `CATransaction::commit` if the private handler is not
  available or does not consume the frame during commit, so a private-API miss
  cannot leave the IOSurface queue permanently backpressured.
- GPUI frame pacing now switches `last_frame_time` from frame-callback start
  time to presentation feedback time once feedback starts arriving. This is the
  first scheduler-level replacement for Zed's old "draw submitted means frame
  complete" model.

**BeginFrame input and lifecycle**

- `RequestFrameOptions` now carries a Chromium-shaped `BeginFrameArgs` object:
  `source_id`, `sequence_number`, `frame_time`, `deadline`, `interval`, and a
  `missed` bit. The older `predicted_display_time`, `frame_interval`, and
  `frame_deadline` fields remain during the transition.
- The macOS `DisplayLink` wrapper preserves the `CVDisplayLink` `output_time`
  host timestamp, converts it to GPUI's `scheduler::Instant`, assigns a
  per-display source id and monotonically increasing sequence number, and
  passes that through `step()` as the current BeginFrame.
- GPUI tracks the current BeginFrame id and suppresses duplicate draw attempts
  for the same BeginFrame. This adds the first real scheduler lifecycle rule
  from Chromium's `SchedulerStateMachine`: one draw per BeginImplFrame.
- The macOS BeginFrame source keeps its sequence number monotonic across
  demand-driven `CVDisplayLink` stop/start cycles. This is required because the
  source id is the display id; if each recreated `CVDisplayLink` starts at
  sequence zero, GPUI sees new ticks as duplicate/older BeginFrames and blocks
  them, producing visibly choppy low-FPS presentation.
- The macOS BeginFrame source now keeps the `CVDisplayLink` object allocated
  while idle and toggles `CVDisplayLinkStart`/`CVDisplayLinkStop`, recreating it
  only when the window moves to another display. This matches Chromium's
  `CADisplayLink` lifecycle shape more closely (`paused`/`enabled` instead of
  destroy/recreate), preserves source continuity, and avoids repeatedly hitting
  the existing `DisplayLink::Drop` leak workaround during normal frame-idle
  transitions.
- GPUI filters older BeginFrames before they can replace the current scheduler
  state, matching Chromium's `BeginFrameSource` continuity rule that observers
  should only see forward-moving frame times/sequences. This matters when an
  async missed BeginFrame races a normal display-link tick.
- If a newer BeginFrame arrives before a previously scheduled regular/scroll
  deadline fires, GPUI now synchronously flushes the previous deadline before
  accepting the newer BeginFrame. This mirrors Chromium's
  `OnBeginFrameContinuation()` behavior, where the old
  `OnBeginFrameDeadline()` runs before `current_begin_frame_args_` is advanced.
- macOS now retains the latest display-link timing when the BeginFrame source
  goes idle. When frame production is re-enabled after at least one display
  interval has elapsed, it posts an immediate missed BeginFrame using an
  advanced sequence number and `missed = true`, mirroring Chromium's
  `BeginFrameSource::AddObserver()` missed-frame delivery without duplicating
  the last normal BeginFrame.
- GPUI now subtracts a moving draw-duration estimate plus a 1 ms fudge factor
  from the BeginFrame display deadline to produce a draw-start deadline,
  mirroring Chromium's adjusted deadline behavior. It records whether the draw
  missed the original presentation deadline, keeping the "when should drawing
  start?" and "did this frame miss vsync?" decisions separate.
- GPUI also implements a first WAIT_FOR_SCROLL-style deadline mode. During
  active scroll input, if no scroll event has arrived for the current
  BeginFrame, a dirty draw is delayed until `frame_time + interval / 3`,
  matching Chromium's default `scroll_deadline_ratio`. If scroll input arrives
  before then, the delayed draw still uses the same BeginFrame id and the
  one-draw-per-frame guard remains in effect.
- GPUI now routes BeginFrame production through explicit deadline-mode state:
  `NONE`, `BLOCKED`, `IMMEDIATE`, `REGULAR`, `LATE`, and `WAIT_FOR_SCROLL`.
  `REGULAR` dirty frames schedule at the adjusted draw deadline, `IMMEDIATE`
  handles forced or previously-missed frames, `LATE` covers presentation without
  redraw, and `BLOCKED` covers duplicate draw attempts for the same BeginFrame.
- GPUI uses the BeginFrame deadline/frame time as its fallback frame time until
  real presentation feedback arrives. This moves frame pacing closer to
  Chromium's model, where frame production is tied to an intended display
  deadline rather than merely "the main queue woke up."
- GPUI now has an explicit platform `set_needs_begin_frame` subscription hook.
  Dirty windows, pending `on_next_frame` callbacks, pending presentation, and
  sustained high-rate input subscribe to BeginFrames; `complete_frame()` drops
  the subscription once there is no remaining work. On macOS this starts/stops
  `CVDisplayLink` instead of running it continuously just because the window is
  visible. Visibility changes, display changes, activation, and AppKit
  `displayLayer:` callbacks now reapply that subscription state rather than
  forcing the display link on unconditionally.
- Runtime finding from the first built app: dropping the BeginFrame source
  immediately after each input-driven frame made editing feel low-FPS/choppy,
  because bursty invalidations had to restart `CVDisplayLink` one tick at a
  time. GPUI now keeps the BeginFrame source warm briefly after input that
  actually dirties the window, without forcing redraws, so the next invalidation
  has fresh display timing available.

### What this still is not

This is not a full Chromium architecture clone. It does **not** add:

- A separate GPU process.
- A browser-process/GPU-process split. The branch now uses
  `CAContext`/`CALayerHost` when available, but both sides still live in Zed's
  process.
- Full viz `BeginFrame` scheduling. Zed now carries BeginFrame ids, frame time,
  interval, adjusted deadline, one-draw-per-frame enforcement,
  missed-deadline state, explicit deadline-mode outputs, platform BeginFrame
  subscription control, swap-ack backpressure, and a Chromium-style GPU-busy
  gate for the IOSurface queue, but does not yet have Chromium's full
  BeginFrame observer graph or scheduler state machine.
- Remote/cross-process CoreAnimation feedback. The local `CALayer`/IOSurface
  path now reports Chromium-shaped ready/latch/display feedback, but it is still
  estimated from local Metal completion, CAContext/CALayer handoff, and
  display-link timing rather than coming from a browser-process handoff.
- Complete GPUI damage-region submission. The IOSurface presenter now has
  Chromium-style per-buffer damage accumulation and a partial-damage render-pass
  path. GPUI feeds precise old/new clipped bounds for cached dirty views and
  clipped primitive bounds for newly painted content, while cached paint replay
  stays damage-free. Global refreshes and invalidations without reliable
  previous bounds still intentionally submit full-frame damage.

The important practical point: the resource handoff and the scheduler feedback
surface now both exist. The remaining work is tightening timestamp quality and
aligning frame production with that feedback, not just swapping the present
resource.

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

In the current branch, item (1) is replaced by a CALayer/IOSurface handoff by
default. Item (2) is partially addressed by both
`PresentationFeedback` and BeginFrame timing: GPUI now consumes renderer
feedback for frame pacing, carries the display link's intended output time,
refresh interval, frame deadline, and BeginFrame id through the request-frame
callback, prevents duplicate draws for a single BeginFrame, and subscribes the
platform BeginFrame source only while frame production is needed. It still does
not have Chromium's full `BeginFrame` scheduler.

---

## Open questions / next steps

- **Measure the default Chromium-style path.** Run `typometer` on:
  1. patched/default Zed,
  2. `ZED_MACOS_LEGACY_METAL_LAYER=1` Zed,
  3. VS Code.
  Run at 60Hz + LPM if possible. This is the test that determines whether the
  present-resource model plus BeginFrame scheduler work is enough to explain
  the gap.
- **Runtime QA.** Exercise resize, fullscreen, tabbing, activation, multiple
  windows, transparent titlebar, and moving between displays with different
  scale factors. Those are the highest-risk CALayer integration edges.
- **Presentation feedback accuracy.** The legacy `CAMetalLayer` path now uses
  `MTLDrawable.presentedTime`, and the CAContext/CALayerHost IOSurface path now
  reports ready/latch/display/interval feedback using Chromium's display-link
  latch estimate. The remaining gap is cross-process feedback semantics, not
  simply adding a callback field.
- **BeginFrame alignment.** GPUI now has BeginFrame ids, one-draw-per-frame
  tracking, adjusted draw deadlines, missed-deadline state, a
  WAIT_FOR_SCROLL-style scroll deadline, and demand-driven platform BeginFrame
  subscription. It now routes frame production through explicit `NONE` /
  `BLOCKED` / `IMMEDIATE` / `REGULAR` / `LATE` / `WAIT_FOR_SCROLL` modes, but
  still needs Chromium-style observer orchestration and richer scheduler state.
- **Damage regions.** The IOSurface queue now carries Chromium-style per-buffer
  accumulated damage and the Metal pass can load/scissor for partial redraws.
  GPUI now submits precise old/new clipped bounds for cached dirty views and
  primitive bounds for newly painted content, while suppressing damage from
  cached paint replay. The remaining work is reducing conservative full-frame
  fallbacks where no previous bounds are available.
- **Process split.** Chromium uses CAContext/CALayerHost across its GPU/browser
  process boundary. Zed now has the layer topology locally; a true process split
  would be a much larger platform architecture change.

---

## Key references

**Zed (this repo):**
- `crates/gpui_macos/src/metal_renderer.rs` — default Chromium-style CA/IOSurface path and legacy `ZED_MACOS_LEGACY_METAL_LAYER` fallback
- `crates/gpui_macos/src/remote_layer.rs` — local CAContext/CALayerHost layer tree with direct-CALayer fallback
- `crates/gpui_macos/src/metal_renderer.rs` — IOSurface allocation, IOSurface-backed Metal textures, 3-buffer/2-pending queue, per-buffer damage accumulation
- `crates/gpui/src/scene.rs`, `crates/gpui/src/view.rs`, and
  `crates/gpui/src/window.rs` — scene damage transport and cached-view damage
  source
- `crates/gpui_macos/src/window.rs` — generic backing-layer hook and deferred-frame retry callback
- `crates/gpui_macos/src/display_link.rs` — `CVDisplayLink` → dispatch_source
- `crates/gpui/src/window.rs` and `crates/gpui_macos/src/window.rs` —
  demand-driven BeginFrame subscription via `set_needs_begin_frame`
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
