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
  `CALayer`, exposed to the NSView via a replaceable `CALayerHost` child under
  a stable local backing layer. If those runtime interfaces are unavailable, it
  falls back to the direct local `CALayer` IOSurface path.
- The CAContext/CALayerHost path now uses Chromium-shaped runtime capability
  gates before enabling remote layers: `CAContext` must support
  `contextWithCGSConnection:options:`, expose the dynamic `contextId` and
  `layer` properties, and `CALayerHost` instances must support both
  `contextId` and `setContextId:`. If any of those private interfaces are
  absent, the renderer uses the direct local `CALayer` IOSurface path instead.
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
  `CALayer`, a renderer-level deferred retry asks GPUI for an explicit missed
  BeginFrame instead of force-rendering outside the BeginFrame scheduler. This
  keeps buffer-availability retries on the same pacing path as normal
  display-link frames.
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
- BeginFrame finish accounting now uses the platform draw result for
  Chromium's `BeginFrameAck.has_damage` semantics. A GPUI draw attempt that
  returns `PlatformDrawResult::Submitted` records `has_damage=true`; `Deferred`
  and `Skipped` still update draw-duration and one-draw-per-BeginFrame state,
  but finish with `has_damage=false` because no compositor frame was submitted.
- Swap completion feedback now carries an explicit Chromium-shaped result:
  `Ack`, `Skipped`, or `Failed`, while preserving the existing `presented`
  boolean for current scheduling code. A committed current-generation IOSurface
  reports `Ack`, a stale generation that is acknowledged without becoming the
  current layer contents reports `Skipped`, and Metal command-buffer failure
  reports `Failed`. GPUI treats `Failed` as fresh refresh work, so restored
  buffer damage from a failed Metal submission is actually redrawn instead of
  waiting for some unrelated invalidation.
- GPUI now also mirrors Chromium's GPU-busy gate: the macOS IOSurface presenter
  exposes its max pending swaps (`IOSURFACE_BUFFER_COUNT - 1`), and the
  scheduler blocks dirty/present work while submitted swaps are at that limit,
  but it no longer unsubscribes the platform BeginFrame source while blocked.
  This matches Chromium's split between `SetNeedsBeginFrame` and
  `SetIsGpuBusy`: the source stays active while GPU-busy, incoming BeginFrames
  are throttled, and GPU availability replays the stored pending BeginFrame.
  This gate is checked both when a BeginFrame arrives and when a delayed
  regular/scroll deadline actually fires, matching Chromium's
  `AttemptDrawAndSwap()` check before `DrawAndSwap()`: if the current
  BeginFrame reaches its deadline while the swap queue is still full, GPUI
  finishes that BeginFrame with no draw instead of parking the same deadline for
  replay. Only later BeginFrames throttled at the source/gpu-busy boundary are
  retained in `pending_gpu_available_frame`. A swap ack lowers the pending
  count, re-evaluates BeginFrame subscription, and if it clears a backpressured
  state with source-throttled work pending, GPUI replays the exact
  `BeginFrameDeadlineRequest` that was held by GPU-busy backpressure, not just
  the raw platform `RequestFrameOptions`. The replay bridge encodes a
  scheduler-computed `needs_present` bit back into `require_presentation`
  because the platform request callback cannot carry private scheduler state.
  This mirrors Chromium's `pending_begin_frame_args_` / `OnGpuNoLongerBusy()`
  path after `DidReceiveSwapBuffersAck()` while preserving present-only intent
  across the GPU-busy boundary.
- Swap ack now re-enters the GPUI `BeginFrameScheduler` instead of directly
  asking the platform for a new BeginFrame. If a frame was parked behind GPU
  busy, GPUI replays that frame first. Otherwise, if the current BeginFrame has
  a delayed deadline pending and the window is visible, GPUI reschedules that
  deadline after any observed swap ack, not only after an ack that crosses the
  backpressured threshold. Only when there is no parked frame, no current
  delayed deadline, and real scheduler work remains does GPUI request a fresh
  BeginFrame. Pending platform swaps by themselves keep the source subscribed
  but do not trigger speculative scheduler wakeups. This matches Chromium's
  `DidReceiveSwapBuffersAck()` ordering: update `pending_swaps_`, clear GPU
  busy, then call
  `ScheduleBeginFrameDeadline()` so the current BeginFrame can move off a late
  backpressure deadline as soon as the swap queue opens.
- Resize now has a local equivalent of Chromium's
  `DisplayScheduler::ForceImmediateSwapIfPossible()`. GPUI's bounds-change path
  marks the window dirty, lets resize observers update layout, and then forces
  any open BeginFrame deadline through `AttemptDrawAndSwap()` /
  `DidFinishFrame()` immediately. A pending rescheduled deadline for the current
  BeginFrame is consumed before the fallback current request, matching
  Chromium's "force the current scheduler interval" behavior before resize
  suppression. The macOS `displayLayer:` callback no longer renders inline when
  the early resize request loses the race; Chromium can pump compositor tasks
  through `WindowResizeHelperMac` during live resize, but Zed's local wait is a
  blocking condition-variable wait. Rendering inline there parks AppKit's
  resize tracking loop and makes the window border lag the cursor, especially
  on corner drags. `displayLayer:` now waits only for an already-submitted
  current-generation frame; otherwise it requests an async native frame and
  returns to AppKit.
- GPUI now carries an explicit local equivalent of Chromium's
  `output_surface_lost_` scheduler bit. A failed IOSurface swap completion marks
  the output surface lost, requests refresh work, and immediately flushes any
  open BeginFrame interval as a no-draw finish. While lost, deadline selection is
  immediate and normal draw/present production is blocked; the next recovery
  frame is forced through `force_render`, which refreshes GPU-facing state and
  clears the lost bit before drawing. This keeps the local single-process
  recovery path aligned with Chromium's `OutputSurfaceLost()` /
  `DesiredBeginFrameDeadlineMode(kImmediate)` / `ShouldDraw() == false`
  sequence without permanently blocking recovery on an external GPU-process
  output-surface recreation.
- GPUI now has explicit Chromium-style one-shot BeginFrame state. `on_next_frame`
  and `request_animation_frame()` set a `needs_one_begin_frame` bit, which keeps
  the platform BeginFrame source subscribed until the next accepted source
  BeginFrame consumes it. This mirrors Chromium's `SetNeedsOneBeginFrame()`:
  callback-only frame requests no longer masquerade as durable dirty work or
  enter the damage-reschedule path for the current BeginFrame interval. The
  platform request metadata now distinguishes source BeginFrames from scheduler
  replays, so GPU-available or damage-deadline re-entry cannot accidentally
  consume a one-shot source request.
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
- If Metal later reports that an IOSurface command buffer failed, Zed restores
  the submitted damage to that buffer before releasing it for reuse. This keeps
  the BufferQueue invariant intact: a failed draw cannot silently turn a dirty
  IOSurface region clean and later expose stale pixels through a partial redraw.
- The Metal pass is ready for partial damage: full damage keeps the previous
  clear-and-redraw behavior, while non-full damage loads existing IOSurface
  contents and applies a Metal scissor rect for the damaged area.
- The CA/IOSurface path now preserves no-damage frames as no-damage. If a
  completed `Scene` has no damage region, the renderer skips taking an
  IOSurface buffer instead of treating missing damage as full viewport damage.
  This matches Chromium's partial-swap/root-render-pass behavior, where an
  empty damage rect can skip drawing rather than manufacturing damage.
- `Scene` now carries a damage region. Global refreshes and uncertain view
  invalidations remain conservative/full-frame, but dirty views no longer
  blindly damage the full viewport. Cached dirty views damage the union of
  their previous clipped bounds and current clipped bounds; non-cached dirty
  views damage their current clipped bounds; and newly painted primitives also
  contribute their clipped primitive bounds to scene damage. Cached paint replay
  suppresses primitive damage so reused content does not spuriously dirty the
  frame.
- Runtime finding from launching the first bounded-damage build: seeding exact
  hitbox damage and then calling `cx.notify(current_view)` was not actually
  enough. `notify` marks the containing view dirty, and `AnyView::prepaint`
  widened that dirty view back to its cached view bounds; repainting the dirty
  view then inserted fresh primitives, which widened scene damage again. Under
  pointer-heavy interaction this looked like low FPS/choppy presentation
  because the renderer was still asked to redraw and upload large regions every
  frame.
- GPUI now has bounded dirty-view invalidation for element-local visual state.
  The invalidator records an optional absolute damage rect per dirty view,
  propagates that bounded damage through dirty ancestors so cached parent views
  rebuild, damages only the bounded rect during prepaint, and suppresses
  automatic primitive damage while painting that bounded dirty view. Local
  `div` hover/group-hover, pressed/clicked, drag-start feedback, scroll-offset
  changes, and interactive text mouse-down/up state now use this path instead
  of `invalidate_bounds(...) + cx.notify(current_view)` or full refreshes.
  These common input feedback frames therefore flow through bounded dirty-view
  damage instead of conservative viewport/view damage. Editor-local mouse/hover
  repaint paths still notify the editor view rather than refreshing the whole
  window. The macOS IOSurface path encloses fractional scaled-pixel damage with
  floor-origin/ceil-max device-pixel bounds before feeding the presenter, so
  partial redraw scissoring never under-covers changed content at Retina scale
  boundaries.

**Presentation handoff**

- GPU completion dispatches back to the main queue.
- The completed IOSurface is assigned to the content layer's
  `CALayer.contents`. In the CAContext path, that content layer is the root
  layer attached to the `CAContext`; the NSView displays it through
  `CALayerHost.contextId`, matching Chromium's remote-layer topology.
- The IOSurface content layer now mirrors Chromium's raw IOSurface fallback
  details: contents gravity is top-left, minification and magnification filters
  are nearest-neighbor like Chromium's `kCAFilterNearest` content layers, and
  if a handoff assigns the same IOSurface object already installed on the
  layer, Zed calls the private `setContentsChanged` selector when available
  instead of relying on a no-op `contents` assignment to invalidate
  CoreAnimation state. If that selector is unavailable, Zed forces a contents
  transition through `nil` and back to the IOSurface inside the disabled
  transaction so same-buffer relatching still reaches CoreAnimation.
- The direct local `CALayer` fallback now also mirrors Chromium's
  `DisplayCALayerTree` topology: a stable flipped backing layer is attached to
  the NSView, and the IOSurface contents live in a separate child content layer
  anchored at the origin. This keeps the non-CAContext fallback structurally
  aligned with Chromium's raw IOSurface path instead of making the IOSurface
  layer itself the view backing layer.
- Core Animation layer-tree mutations now run with actions disabled, matching
  Chromium's `ScopedCAActionDisabler` usage in `DisplayCALayerTree`. Zed
  already disabled actions for the IOSurface contents transaction; the layer
  tree now also disables implicit animations for size/scale/opacity changes,
  CAContext recreation, `CALayerHost` replacement, and teardown.
- The IOSurface handoff now checks the completed Metal command buffer status
  before touching CoreAnimation. Failed GPU work produces failed swap and
  presentation feedback, releases backpressure, restores the submitted damage
  to the failed buffer, and requests a replacement missed BeginFrame instead of
  latching a stale or partially rendered IOSurface.
- IOSurface submissions now move through a Chromium-shaped
  `presented_frames_` queue. Each submitted frame is inserted in FIFO
  submission order; Metal completion only marks that queue entry ready; and the
  CA handoff pops/commits ready frames from the queue front. A later command
  buffer completing first can no longer overtake the older front entry and
  overwrite presentation order. If a queued command buffer fails, its entry is
  removed, failed feedback is emitted, and the next ready front entry can
  commit.
- Queue-front commit cadence now mirrors Chromium's `OnVSyncPresentation()`
  shape locally. A ready frame with no backlog commits immediately. When more
  than one IOSurface submission is pending, completion only marks the frame
  ready; the next display-link timing callback commits one ready queue-front
  frame. If the queue reaches the pending-swap cap, Zed attempts one immediate
  ready-front commit before deferring the draw, matching Chromium's
  "commit before adding one more if too many swaps are pending" escape hatch.
  GPUI also keeps the platform BeginFrame source subscribed while platform
  swaps are pending so delayed CA commits continue to receive display-timing
  ticks even when no new scene work is dirty.
- Runtime finding from launching the first built CA/IOSurface app: the
  display-link callback originally committed a ready IOSurface and then invoked
  GPUI's frame callback in the same call stack, while the swap-completion ack
  from that commit was still queued asynchronously on the main queue. GPUI
  therefore saw the stale "2 pending swaps" state for that display tick,
  classified the tick as GPU-busy, and completed the frame without drawing.
  That creates an every-other-vsync-looking cadence under load. The macOS frame
  request path now detects when `set_display_timing()` actually commits a ready
  IOSurface and posts the normal GPUI frame callback behind the queued
  swap-completion delivery, so the pending-swap count is current before the
  scheduler decides whether to draw. This applies both to normal display-link
  ticks and explicit replay/missed-frame requests; a queued request skips
  reapplying display timing so one request cannot drain several ready IOSurfaces
  for the same tick. Deferred buffer-availability retries use the same async
  missed-BeginFrame path to avoid reentering `MacWindowState` while the
  display-timing commit still holds its lock.
- Runtime finding after launching the CA/IOSurface path: copying Chromium's
  scroll-specific vsync-aligned presentation policy too literally is harmful in
  this local GPUI pipeline. Zed does not yet have Chromium's full viz scheduler
  and compositor-thread CA transaction cadence, so parking a completed
  interaction IOSurface until the next display-link timing callback can create
  an every-other-vsync cadence when GPU completion lands on the wrong side of
  the tick. The local path now commits a lone ready queue-front IOSurface
  immediately for both interaction and non-interaction frames, while still
  deferring when there is a queue backlog.
- When a window moves to another display, Zed now drains all currently ready
  queue-front IOSurface frames before replacing the display-link source. This
  mirrors Chromium's `SetVSyncDisplayID()` rule to commit pending CA frames
  before switching monitors, while preserving Zed's stricter local rule that
  not-yet-ready Metal submissions are not handed to CoreAnimation early.
- Chromium's coordinator also carries Metal shared-event backpressure fences
  through `CALayerTreeCoordinator::EnqueueBackpressureFences()` and waits them
  in `ApplyBackpressure()` so future GPU work does not starve CoreAnimation.
  Zed now mirrors that contract locally with an `MTLSharedEvent`: each
  submitted IOSurface command buffer signals a monotonically increasing event
  value, a successfully committed frame publishes that shared-event fence as
  the committed-frame backpressure fence, and the next IOSurface draw applies
  it before selecting another buffer. Because Zed still marks frames ready only
  after Metal completion, this fence is usually already signaled; the important
  parity point is that the render path now has Chromium's explicit
  committed-frame backpressure boundary without handing unfinished IOSurface
  contents to CoreAnimation.
- When the drawable size changes, the CAContext/CALayerHost path now marks the
  context as resized and recreates the CAContext immediately before the next
  IOSurface frame handoff. This mirrors Chromium's resize-time CAContext
  replacement so the new-size frame and host context id update are committed
  together. When the private fence-port selectors are available, the old
  CAContext now creates and installs a fence mach port before replacement, and
  Zed retains a small queue of recent ports using Chromium's default cap of
  four. Unlike Chromium's browser/GPU split, the port is retained locally rather
  than sent across a process boundary to a browser-side `DisplayCALayerTree`.
  The replacement order also matches Chromium's resize path: create the
  replacement CAContext without a layer, fence and detach the old CAContext,
  then attach the content layer to the new CAContext and update the
  `CALayerHost.contextId`. The host side now mirrors Chromium's
  `DisplayCALayerTree::GotCAContextFrame()` shape locally: CAContext id changes
  create a fresh `CALayerHost` sublayer, add it to a stable root layer, and
  remove the previous host layer instead of mutating a persistent host in place.
  The host sublayer also uses Chromium's `kCALayerMaxXMargin |
  kCALayerMaxYMargin` autoresizing mask, while the stable root remains
  width/height sizable.
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
  fields: `ready_time`, `latch_time`, `display_time`, `target_latch_time`,
  `interval`, `presented`, `vsync`, and `hardware_completion`.
- The macOS renderer reports feedback from both paths:
  - legacy `CAMetalLayer`: via `MTLDrawable.addPresentedHandler`, converting
    the drawable's CoreAnimation `presentedTime` into `scheduler::Instant`;
  - Chromium pipeline `CALayer`/IOSurface path: records GPU-ready time when the
    Metal command buffer completes, uses the main-thread `CALayer.contents`
    transaction as latch time, and estimates display time from the display-link
    timing captured on the IOSurface frame at the CA commit point using
    Chromium's 1.5 ms latch-buffer model.
- IOSurface presentation feedback is now frame-local instead of reading a
  mutable "latest display timing" slot when the callback fires. The display-link
  timing that releases the CA commit is frozen onto the `PresentedIosurfaceFrame`,
  and the feedback's `target_latch_time` is computed from that frame's targeted
  display-link latch window. This closes a scheduler feedback hazard
  where backlog, resize waits, or a later display-link tick could make a
  committed frame look like it missed a different latch window and push GPUI
  into unnecessarily immediate/choppy follow-up scheduling.
- GPUI now has a local equivalent of Chromium's
  `pending_presentation_group_timings_`. When a platform swap is actually
  submitted, the scheduler records the BeginFrame frame time, interval, and
  target latch window for that presentation group. When presentation feedback
  arrives, GPUI pops the oldest pending group and uses it to fill missing
  target-latch or interval metadata before updating scheduler pacing. This
  mirrors Chromium's `Display::PresentationGroupTiming` handoff from
  `DrawAndSwap()` to `DidReceivePresentationFeedback()` and makes the feedback
  loop scheduler-owned instead of relying only on platform callbacks to carry
  every field.
- The pending presentation group selection now mirrors Chromium's
  `kSelectFutureFrameDeadline` shape for the local single-deadline source:
  if a new swap would reuse a latch target that has already been targeted, or
  if the target latch is already in the past, GPUI advances the group by the
  retained refresh interval until it targets the next available future latch.
  This prevents multiple queued swaps from all being accounted against the same
  latch window, which is one of Chromium's explicit protections against
  pipeline backlog turning into misleading presentation feedback.
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
- GPUI now also feeds presentation feedback back into deadline selection. If a
  presented frame's GPU-ready time lands after the platform-provided target
  latch window, the next dirty BeginFrame is treated like a missed deadline and
  takes the `IMMEDIATE` path instead of waiting for the regular delayed draw
  deadline. When the platform cannot provide a target latch timestamp, GPUI
  falls back to `display_time - 1.5 ms`, matching the Chromium mac latch-buffer
  model. This mirrors Chromium's
  `DisplayScheduler::OnPresentationFeedback()` check for
  `ready_timestamp > target_latch_time`, adapted to Zed's local display-link
  display-time estimate.
- GPUI now retains the last valid scheduler frame interval from explicit
  BeginFrame/request metadata and from `PresentationFeedback.interval`, using
  that retained interval as the scheduler fallback when a platform frame request
  does not carry interval metadata. Chromium updates `last_frame_interval_` when
  BeginFrame intervals change, and its mac coordinator fills
  `feedback.interval` from display timing; Zed now preserves the same interval
  signal instead of falling straight back to a hardcoded 16.667 ms interval.
- The same retained frame interval now updates GPUI's high-rate input tracker,
  so proactive presentation sustain is keyed to the current display cadence
  rather than a fixed 60 Hz threshold. This keeps the scheduler's input-rate
  heuristic aligned with Chromium's frame-interval-updated state on high-refresh
  displays.

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
- That continuity filter now requires strictly newer `frame_time`, matching
  Chromium's `CheckBeginFrameContinuity()`. Equal-time BeginFrames are filtered
  even if their sequence number or source id differs, preventing duplicate work
  when AppKit/display-link delivery races synthesize overlapping frame ids.
- For same-source BeginFrames, the filter also requires a strictly increasing
  sequence number, matching Chromium's `ExternalBeginFrameSource::OnBeginFrame`
  guard. This prevents a synthesized or replayed frame from re-entering the
  scheduler merely because its timestamp was nudged forward.
- If a newer BeginFrame arrives before a previously scheduled regular/scroll
  deadline fires, GPUI now synchronously flushes the previous deadline before
  accepting the newer BeginFrame. This mirrors Chromium's
  `OnBeginFrameContinuation()` behavior, where the old
  `OnBeginFrameDeadline()` runs before `current_begin_frame_args_` is advanced.
- macOS now retains the latest display-link timing when the BeginFrame source
  goes idle. When frame production is re-enabled, it can post an immediate
  missed BeginFrame by reissuing the last retained timing with `missed = true`,
  mirroring Chromium's `BeginFrameSource::AddObserver()` behavior. It no longer
  invents an advanced sequence number or timestamp while the display link is
  stopped; GPUI's Chromium-style continuity filter decides whether the missed
  frame is still usable or should be ignored until the next real display-link
  tick.
- macOS now consumes `completed_frame(Some(BeginFrameAck))` at the platform
  BeginFrame source boundary. The last acked BeginFrame id is retained, and
  retained display-link timing for that same id is not reissued as a missed
  BeginFrame after the frame has already been finished. This moves GPUI closer
  to Chromium's `BeginFrameSource::DidFinishFrame()` contract, where the source
  sees explicit observer completion instead of treating every retained tick as
  eligible for replay. Native completions via `completed_frame(None)` no longer
  clear the retained BeginFrame ack; otherwise a stale/older/native completion
  after a real BeginFrame finish could make the retained display-link timing
  look unfinished and eligible for a bogus missed-frame replay.
- GPUI's `BeginFrameAck` now carries the original BeginFrame `frame_time` in
  addition to the id and damage bit. Chromium's BeginFrame source asks the
  observer for `LastUsedBeginFrameArgs()` before issuing missed frames; GPUI's
  macOS source now records the last BeginFrame it actually delivered to the
  request-frame callback, and also retains the last finish ack. Missed-frame
  replay is suppressed when retained timing is not newer than either the
  delivered last-used BeginFrame or the finished ack. This closes the
  delivery-to-finish race where a BeginFrame accepted by GPUI but not yet
  finished could be reissued as a duplicate missed frame.
- macOS frame delivery now distinguishes Chromium-style source BeginFrames from
  scheduler replays and native resize renders. Source deliveries from
  `CVDisplayLink`, `request_begin_frame()`, and missed-frame retry are filtered
  against the source's last-used/acked records before invoking GPUI's callback.
  Scheduler replays, such as damage-deadline reschedules and GPU-available
  re-entry, intentionally bypass that source filter so the scheduler can
  reconsider the current BeginFrame. Native resize render requests remain
  non-BeginFrame fallback work, but active resize-time BeginFrames are now
  flushed through GPUI's scheduler before that fallback is dispatched. This
  moves the local platform boundary closer to Chromium's split between
  `ExternalBeginFrameSource::OnBeginFrame()`, `DisplayScheduler` re-entry, and
  resize-time forced swaps.
- GPUI now has a platform `on_begin_frame_for_input` observer, matching
  Chromium's `BeginFrameSource::IssueBeginFrameToInputClient()` before
  `IssueBeginFrameToSchedulerClient()`. macOS source BeginFrames deliver to
  this input observer before invoking the scheduler request-frame callback;
  scheduler replays and native resize renders do not. The GPUI input tracker
  consumes those source BeginFrames to update its display-cadence threshold, so
  input pacing state is tied to the same source tick stream as frame scheduling
  instead of only being updated from scheduler continuation.
- macOS now drops queued missed-BeginFrame callbacks after
  `set_needs_begin_frame(false)`. This mirrors Chromium's
  `StopObservingBeginFrames()` cancellation of queued missed tasks: the dispatch
  queue callback may still run, but it no longer reissues stale retained timing
  once GPUI has gone idle.
- The continuity filter now has the corresponding missed-frame exception: an
  explicit missed retry for the current BeginFrame is not discarded merely
  because its id/time matches the current frame. The one-draw-per-BeginFrame
  guard still blocks duplicate dirty draws, but present-only retry work can flow
  through the `LATE` path.
- Normal macOS `request_begin_frame()` no longer falls back to
  `last_display_link_timing`. The stale fallback could replay the frame that
  had just completed before `CVDisplayLink` produced a new timing sample, and
  GPUI would then reject it as old, creating visible frame bubbles. Stale timing
  is now reserved for the explicit missed-BeginFrame path.
- GPUI now subtracts a moving draw-duration estimate plus a 1 ms fudge factor
  from the BeginFrame display deadline to produce a draw-start deadline,
  mirroring Chromium's adjusted deadline behavior. It records whether the draw
  missed the original presentation deadline, keeping the "when should drawing
  start?" and "did this frame miss vsync?" decisions separate.
- Runtime finding from the first built CA/IOSurface app: that adjusted
  draw-start deadline cannot be used blindly for this local IOSurface path.
  GPUI's draw-duration estimate measures CPU command encoding and submission,
  but this path does not hand the IOSurface to CoreAnimation until the Metal
  command buffer completes and a main-thread CA transaction commits. Delaying
  drawing until `deadline - estimated_cpu_draw - 1ms` can therefore start the
  frame too late, miss scanout, and look like artificially low FPS. The platform
  contract now lets renderers opt out of delayed BeginFrame scheduling; the
  macOS CA/IOSurface presenter opts out, while the legacy `CAMetalLayer` path
  and other platforms keep the adjusted-deadline behavior.
- GPUI also implements a first WAIT_FOR_SCROLL-style deadline mode. During
  active scroll input, if no scroll event has arrived for the current
  BeginFrame, a dirty draw is delayed until `frame_time + interval / 3`,
  matching Chromium's default `scroll_deadline_ratio`. If scroll input arrives
  before then, the delayed draw still uses the same BeginFrame id and the
  one-draw-per-frame guard remains in effect.
- GPUI now routes BeginFrame production through explicit deadline-mode state:
  `NONE`, `BLOCKED`, `IMMEDIATE`, `REGULAR`, `LATE`, and `WAIT_FOR_SCROLL`.
  `REGULAR` dirty frames schedule at the adjusted draw deadline, `IMMEDIATE`
  handles forced, previously-missed, or delayed-scheduling-unsupported frames,
  `LATE` schedules at `frame_time + interval` for clean, present-only, or
  swap-backpressured BeginFrames on platforms that can safely delay frame
  production, and `BLOCKED` covers duplicate draw attempts for the same
  BeginFrame.
- The frame callback now carries those BeginFrame, deadline, presentation
  feedback, retained interval, and draw-duration fields in a single
  `FrameSchedulerState` object instead of a loose set of callback-local cells.
  This is still not Chromium's full scheduler state machine, but it gives GPUI
  a coherent scheduler state surface for continuing to port Chromium behaviors
  such as pending BeginFrame coalescing, idle drops, and richer deadline
  transitions.
- Delayed BeginFrame deadlines are now tracked as an owned
  `PendingBeginFrameDeadline` record keyed by `BeginFrameId` and desired
  deadline time, not as independent id/options cells. A delayed timer can only
  consume the pending deadline for its own BeginFrame at its own scheduled
  deadline time; if a newer BeginFrame replaces it, or the same BeginFrame is
  rescheduled to a different deadline, the stale timer exits without clearing
  newer pending work. This matches Chromium's "use existing deadline only when
  the task time is unchanged" behavior without requiring a cancellable platform
  timer.
- Pending deadlines now carry the computed frame-production state, not just the
  raw `RequestFrameOptions`. In particular, `needs_present` is preserved across
  delayed timers and synchronous previous-deadline flushes, so a present-only
  `LATE` deadline cannot fire later as an idle no-op. This mirrors Chromium's
  scheduler-state model, where `OnBeginFrameDeadline()` runs against retained
  scheduler state rather than reconstructing presentation intent from the
  BeginFrame args alone.
- GPUI now has a shared local equivalent of Chromium's
  `OnBeginFrameDeadline()`/`AttemptDrawAndSwap()` finish accounting. Immediate
  draws, delayed deadline draws, and synchronously flushed previous deadlines
  all call the same `draw_and_present` operation and the same
  `FrameSchedulerState::finish_begin_frame_*` methods, so draw-duration
  estimates, one-draw-per-BeginFrame state, and main-thread deadline misses are
  updated consistently across every BeginFrame production path. Backpressured
  `AttemptDrawAndSwap` now also flows through `did_finish_frame()` as a no-draw
  result, matching Chromium's `DidFinishFrame(false)` behavior instead of
  completing the platform frame outside scheduler finish accounting.
- GPUI now exposes a platform-level `BeginFrameAck` next to `BeginFrameArgs`.
  `did_finish_frame()` records and forwards an ack for every BeginFrame finish
  through `PlatformWindow::completed_frame(Some(ack))`. The ack carries the
  `BeginFrameId`, original `frame_time`, and Chromium's `has_damage` semantics:
  platform-submitted draw/swap production records `true`, while
  deferred/skipped draw attempts, present-only, idle, throttled, or
  backpressured completions record `false`. Native frame completions that are
  not a Chromium-style BeginFrame finish, such as stale BeginFrame drops or
  duplicate-draw blocked completions, still complete the platform frame with
  `None`.
- The request-frame callback now delegates to a `BeginFrameScheduler` spine with
  Chromium-shaped phases: `on_begin_frame_continuation()`,
  `schedule_begin_frame_deadline()`, `on_begin_frame_deadline()`,
  `attempt_draw_and_swap()`, and `did_finish_frame()`. The platform callback is
  no longer the scheduler; it just passes `RequestFrameOptions` into this spine.
  Continuation classifies incoming BeginFrames as older/current/new, deadline
  scheduling owns delayed tasks, deadline execution performs the draw/swap
  attempt, and finish accounting updates the BeginFrame state.
- `FrameSchedulerState` now tracks Chromium's
  `inside_begin_frame_deadline_interval_` equivalent. Accepting a BeginFrame
  enters the interval, a newer BeginFrame synchronously flushes the previous
  interval before advancing, and deadline execution/finish clears the interval.
  The current `RequestFrameOptions` are retained with the current BeginFrame so
  that a previous interval can be flushed even when it was not represented by a
  delayed timer task.
- GPUI now mirrors Chromium's `OnDisplayDamaged()` call back into
  `ScheduleBeginFrameDeadline()` while a BeginFrame interval is active. The
  shared `FrameSchedulerState` is stored on the `Window`, and dirty transitions
  from `refresh()`, direct view-bounds invalidation, `on_next_frame()`, global
  refresh effects, and App-driven entity notifications ask the current
  BeginFrame to be reconsidered. The request is coalesced per dirty burst, then
  re-enters the normal `on_begin_frame_continuation()` path with the current
  `BeginFrameDeadlineRequest`, preserving the computed `needs_present` bit.
  This closes the choppy-path gap where a clean BeginFrame had already scheduled
  a `LATE` deadline and later damage could otherwise wait until the end of the
  interval instead of moving to Chromium's regular/immediate draw deadline.
- GPUI now also mirrors Chromium's `ForceImmediateSwapIfPossible()` for resize
  pressure. When bounds change during an open BeginFrame interval, GPUI consumes
  the pending/current `BeginFrameDeadlineRequest`, runs production immediately,
  forwards the resulting `BeginFrameAck`, and clears the delayed deadline so a
  stale timer cannot later finish the same BeginFrame again.
- GPUI now mirrors Chromium's `OutputSurfaceLost()` scheduler behavior in the
  local Metal failure path. Failed IOSurface submissions enter an
  output-surface-lost state, force any active interval to finish immediately
  without drawing, and keep subsequent deadlines immediate until a forced
  recovery render clears the state. Non-recovery draws are suppressed while the
  bit is set, matching Chromium's `ShouldDraw()` guard.
- GPUI now models Chromium's `SetNeedsOneBeginFrame()` separately from dirty
  draw scheduling. `on_next_frame` requests a one-shot BeginFrame, the next
  accepted source BeginFrame consumes that request, and the source can go idle
  again after the callback drains if no dirty/present work remains. Scheduler
  replays explicitly clear the source-BeginFrame marker before re-entering the
  callback path.
- `LATE` deadline mode now follows Chromium's
  `DesiredBeginFrameDeadlineTime(kLate)` behavior. Instead of completing
  clean BeginFrames immediately, GPUI waits until the end of the BeginFrame
  interval before finishing with `BeginFrameAck(has_damage=false)`. The same
  late deadline also produces present-only frames and gives swap-backpressured
  frames one interval to clear. If the swap queue is still backpressured at that
  late deadline, GPUI finishes the BeginFrame with
  `BeginFrameAck(has_damage=false)` rather than drawing into the closed queue or
  replaying the stale deadline later. Later incoming BeginFrames are what get
  parked behind the GPU-busy source gate. Renderers that opt out of delayed
  BeginFrame scheduling, such as the CA/IOSurface path, still execute the
  deadline immediately so the scheduler does not reintroduce the previously
  observed late-CPU-start choppiness.
- GPUI now also mirrors Chromium's `BeginFrameSource::SetIsGpuBusy()` response
  state. When pending platform swaps reach the platform limit, the scheduler
  allows one new BeginFrame through, then parks later BeginFrames in
  `pending_gpu_available_frame` without advancing the current BeginFrame or
  emitting a false `BeginFrameAck`. Parked source-throttled work stores a
  `BeginFrameDeadlineRequest`, so the computed `needs_present` decision from
  `OnBeginFrameContinuation()`/`ScheduleBeginFrameDeadline()` survives both the
  delayed-deadline path and the GPU-busy replay path. Current BeginFrames whose
  deadline fires while the swap queue is still full now finish false, matching
  Chromium's `AttemptDrawAndSwap()` ownership: the source holds later
  BeginFrames, not the scheduler's already-expired deadline. When swap
  completion drops the pending swap count below the limit, GPUI resets the
  GPU-busy state and re-enters the scheduler even if the source had only reached
  Chromium's "one BeginFrame after busy sent" state. Parked GPU-available
  frames are replayed first; otherwise a pending current BeginFrame deadline is
  rescheduled on every visible-window swap ack before GPUI asks the platform for
  a fresh BeginFrame. Pending platform swaps are deliberately excluded from that
  fresh BeginFrame wake test, so retained swap completions keep the source alive
  without spinning production when no draw/present work exists. This matches
  Chromium's `DidReceiveSwapBuffersAck()` rule: after `pending_swaps_` is
  decremented, the scheduler observes the updated backpressure state and calls
  `ScheduleBeginFrameDeadline()`/wakes pending BeginFrame work.
- GPUI uses the BeginFrame deadline/frame time as its fallback frame time until
  real presentation feedback arrives. This moves frame pacing closer to
  Chromium's model, where frame production is tied to an intended display
  deadline rather than merely "the main queue woke up."
- GPUI now has an explicit platform `set_needs_begin_frame` subscription hook.
  Dirty windows, one-shot `on_next_frame` requests, pending presentation, and
  sustained high-rate input subscribe to BeginFrames; `complete_frame()` drops
  the subscription once there is no remaining work. On macOS this starts/stops
  `CVDisplayLink` instead of running it continuously just because the window is
  visible. Visibility changes, display changes, activation, and AppKit
  `displayLayer:` callbacks now reapply that subscription state rather than
  forcing the display link on unconditionally. GPUI now also carries platform
  visibility as scheduler state: hidden/occluded windows retain their dirty or
  pending-present work, but `has_frame_work()` returns false until visibility
  returns. This mirrors Chromium's `DisplayScheduler::visible_` gate in
  `ShouldDraw()` instead of leaving occlusion as only a macOS display-link
  start/stop side effect.
- Runtime finding from the first built app: dropping the BeginFrame source
  immediately after each input-driven frame made editing feel low-FPS/choppy,
  because bursty invalidations had to restart `CVDisplayLink` one tick at a
  time. GPUI now keeps the BeginFrame source warm briefly after input that
  actually dirties the window, without forcing redraws, so the next invalidation
  has fresh display timing available.
- GPUI now also mirrors Chromium's proactive post-draw BeginFrame behavior.
  Chromium's `ProactiveBeginFrameWanted()` keeps the source subscribed after
  `did_attempt_draw_in_last_frame_` to avoid negative glitches in
  `SetNeedsBeginFrame` propagation. Zed now records draw attempts and keeps the
  platform BeginFrame source warm for a short bounded window after a produced
  frame, without marking the window dirty or forcing presentation.

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
- Chromium's exact GPU-process display-link callback and cross-process CA frame
  handoff. Zed now preserves FIFO queue-front commit order, local display tick
  cadence, and display-switch ready-frame flushing, but Chromium still has a
  dedicated GPU-process `OnVSyncPresentation()` source and browser-process
  feedback path.
- Remote/cross-process CoreAnimation feedback. The local `CALayer`/IOSurface
  path now reports Chromium-shaped ready/latch/display feedback, but it is still
  estimated from local Metal completion, CAContext/CALayer handoff, and
  display-link timing rather than coming from a browser-process handoff.
- Complete GPUI damage-region submission. The IOSurface presenter now has
  Chromium-style per-buffer damage accumulation and a partial-damage render-pass
  path. GPUI feeds precise old/new clipped bounds for cached dirty views and
  current clipped bounds for non-cached dirty views, plus clipped primitive
  bounds for newly painted content, while cached paint replay stays damage-free.
  Global refreshes and invalidations without reliable bounds still
  intentionally submit full-frame damage.

The important practical point: the resource handoff and the scheduler feedback
surface now both exist, and IOSurface feedback is attached to the frame-local
CA commit timing rather than a mutable latest display tick. The remaining work
is aligning more of frame production with that feedback and validating the
single-process estimates against Chromium/VS Code, not just swapping the
present resource.

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
  reports ready/latch/display/target-latch/interval feedback from frame-local
  CA commit timing using Chromium's display-link latch estimate. The remaining
  gap is cross-process feedback semantics, not simply adding a callback field or
  feeding a shared latest-timing slot into the scheduler.
- **BeginFrame alignment.** GPUI now has BeginFrame ids, one-draw-per-frame
  tracking, adjusted draw deadlines, missed-deadline state, a
  WAIT_FOR_SCROLL-style scroll deadline, and demand-driven platform BeginFrame
  subscription. It now routes frame production through explicit `NONE` /
  `BLOCKED` / `IMMEDIATE` / `REGULAR` / `LATE` / `WAIT_FOR_SCROLL` modes and
  keeps a Chromium-style pending presentation timing queue with future-latch
  target advancement, but still needs the full BeginFrame observer
  orchestration and richer scheduler state.
- **Damage regions.** The IOSurface queue now carries Chromium-style per-buffer
  accumulated damage and the Metal pass can load/scissor for partial redraws.
  GPUI now submits precise old/new clipped bounds for cached dirty views and
  current clipped bounds for non-cached dirty views, plus primitive bounds for
  newly painted content, while suppressing damage from cached paint replay. The
  CA/IOSurface path now skips no-damage scenes instead of inflating them to
  full-frame damage. Element-local pressed/clicked state and scroll-offset
  changes now use view invalidation plus known hitbox damage instead of
  full-window/full-view refresh, and hover/editor-local repaint paths have been
  narrowed similarly where their affected view or hitbox is known. The
  remaining work is reducing conservative full-frame fallbacks for global
  refresh and invalidations without reliable bounds.
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
