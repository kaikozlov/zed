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
