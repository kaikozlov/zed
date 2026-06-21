# Plan: Implement Chromium's Render Pipeline in Zed (the "real deal")

This plan ports the **latency-relevant** parts of Chromium's render pipeline
into Zed faithfully, replacing today's approximations with the real Chromium
abstractions. It is **not** a multi-process web renderer (see "Explicit
non-goals").

Every work item below carries an explicit reference to the Chromium codebase
in `REFERENCE/chromium/`. File paths are repo-relative; line numbers are
current as of the sparse checkout (commit `7b86777a`).

The motivating bug is [zed#26900](https://github.com/zed-industries/zed/issues/26900)
— input-to-pixel latency. The motivating reference is VS Code/Electron
(Chromium's untuned macOS path) beating Zed by ~1-2 frames.

---

## 0. Scope decision: "the real deal" = goal B

Chromium's pipeline has two distinct halves. Only one belongs in Zed.

**Half A — untrusted multi-process web renderer** (the `viz` service split,
Mojo IPC, sandboxed renderer processes, `SurfaceAggregator` compositing
untrusted compositor frames, browser-side `DisplayCALayerTree`). This exists
because Chromium renders **untrusted web content from separate processes**.
Zed renders its **own trusted UI in one process**. Porting this would *add*
latency (IPC hops) and buy nothing (no untrusted content, no iframes).
**Out of scope.**

**Half B — the latency-bearing scheduling + present + feedback model.** The
`BeginFrame` source/observer graph, the `DisplayScheduler` state machine,
`PossibleDeadlines` / `FrameDeadlineDecider`, `DisplayDamageTracker`,
`PresentationFeedback` with trace-id matching, `EventLatency`, and the macOS
`DisplayLinkMac` + `CALayerTreeCoordinator` present path. This is what makes
VS Code feel snappier, and it ports cleanly to a single process.
**This is the plan.**

Verification that Zed has none of Half A's substrate today: `rg "CompositorFrame|RenderPass|DrawQuad|SurfaceId|FrameSinkId|SurfaceAggregator" crates/` returns zero real hits (the `wgpu` `begin_render_pass` matches are WebGPU, not Chromium), and `crates/remote` is the only process-ish code and it is SSH/WSL/Docker transport, not a render process.

---

## 1. Guiding principle

**Every behavioral change in this plan must trace to a named Chromium type or
method.** When a phase is "done," a reader should be able to open the cited
Chromium file and the new Zed code side by side and see the correspondence.
Where Chromium's design depends on Half A (process boundaries, untrusted
surfaces), the Zed analog is a **single-process local equivalent** and is
called out explicitly — it is never silently dropped.

---

## 2. Current state (what is already done)

This is the baseline each phase builds on. All anchored to the `buffering`
branch.

| Capability | Zed location |
|---|---|
| Chromium-style CA/IOSurface presenter (3-buffer, 2-pending) | `crates/gpui_macos/src/metal_renderer.rs` (`PresentedIosurfaceFrame` ~`:186`, `IosurfaceSubmissionQueue`, `mark_buffer_pending` `:552`) |
| Legacy `CAMetalLayer` escape hatch | `ZED_MACOS_LEGACY_METAL_LAYER=1` |
| Local `CAContext`/`CALayerHost` topology + fence ports | `crates/gpui_macos/src/remote_layer.rs` |
| `BeginFrameArgs`/`BeginFrameAck`/`PresentationFeedback`/`SwapCompletionFeedback` types | `crates/gpui/src/platform.rs:612-807` |
| `BeginFrameObserver`/`BeginFrameSource` traits | `crates/gpui/src/platform.rs:655`, `crates/gpui/src/platform.rs:665` |
| Shared BeginFrame observer continuity | `crates/gpui/src/platform.rs:680` (`BeginFrameObserverState`), `:723` (`begin_frame_follows_last_used`), `:746` (`begin_frame_follows_ack`) |
| Scheduler spine embedded in `Window` | `crates/gpui/src/window.rs`: `FrameSchedulerState:1335`, `FrameSchedulerData:1339`, `BeginFrameScheduler:2168`, `on_begin_frame_continuation:2190`, `schedule_begin_frame_deadline:1833/2308`, `on_begin_frame_deadline:2421`, `attempt_draw_and_swap:2440`, `did_finish_frame:2474`, `on_gpu_available:2530`, `on_output_surface_lost:2432` |
| GPUI BeginFrame observer adapters | `InputRateTracker` implements `BeginFrameObserver` at `crates/gpui/src/window.rs:1219`; `BeginFrameScheduler` implements it at `:2570`; source `BeginFrameArgs` are converted to scheduler requests at `:2589` |
| macOS `DelayBasedBeginFrameSource` analog | `crates/gpui_macos/src/window.rs:485` (`MacBeginFrameSource`), `impl gpui::BeginFrameSource` at `:627`, scheduler observer callback storage at `:488`, input-client delivery at `:604`, source timing request construction at `:3214` |
| Deadline-mode selection (prototype) | `crates/gpui/src/window.rs:1980` (`DesiredBeginFrameDeadlineMode`), `:1988` (`BeginFrameDeadlineMode`), `:1998`/`:2009` (transition inputs), `:2024`/`:2068` (transition matches), `:2043`/`:2089` (scheduler adapters) |
| Scene damage transport | `crates/gpui/src/scene.rs:27` (`damage: Option<Bounds<ScaledPixels>>`, `add_damage:67`) |
| Bounded dirty-view invalidation | `crates/gpui/src/window.rs:4991` (`mark_view_dirty`) |
| macOS frame source (`CVDisplayLink` → dispatch) | `crates/gpui_macos/src/display_link.rs`, `window.rs` `step()` |

The feedback-lifecycle drain (swap-completion Ack/Skipped/Failed, presentation
groups) was audited and verified correct; the one open item is FIFO-vs-trace-id
matching, covered by Phase 4.

---

## 3. The phases

Nine phases. Phases 1-3 are structural prerequisites. Phase V (validation)
is a **gate** that must run before Phases 6-9, because it tells us whether the
structural work is even necessary.

```
Phase 1 (BeginFrame graph)  ──┐
Phase 2 (state machine)    ───┼──►  Phase V (typometer gate)
Phase 3 (PossibleDeadlines) ──┘              │
Phase 4 (feedback trace-id) ──┐              │
Phase 5 (refresh-rate caps) ──┘              │
                                   ┌─────────┴─────────┐
Phase 6 (damage tracker) ◄────────┤ data decides scope │
Phase 7 (EventLatency)  ◄─────────┤ of 6-9             │
Phase 8 (DisplayLinkMac) ◄────────┤                    │
Phase 9 (CALayerTree align) ◄─────┘
```

---

### Phase 1 — Extract the `BeginFrameSource` / `BeginFrameObserver` graph

**Chromium reference.**
- `components/viz/common/frame_sinks/begin_frame_source.h`:
  - `BeginFrameObserver` (`:38`) — `OnBeginFrame(args)` (`:55`),
    `OnBeginFrameSourcePausedChanged` (`:75`).
  - `BeginFrameObserverBase` (`:90`) — the continuity-checking base, with
    `OnBeginFrameDerivedImpl` (`:110`) as the subclass hook.
  - `BeginFrameSource` (`:137`) — `DidFinishFrame` (`:224`), `AddObserver`
    (`:228`), `RemoveObserver` (`:229`), the `IssueBeginFrameToSchedulerClient`
    (`:268`) / `IssueBeginFrameToInputClient` (`:272`) split, and the
    `BeginFrameArgsGenerator` inner class (`:161`).
  - `DelayBasedBeginFrameSource` (`:379`) — the vsync-driven source Zed needs;
    its `AddObserver`/`RemoveObserver`/`DidFinishFrame` (`:393-395`) and
    `IssueBeginFrameToObserver` (`:419`) are the macOS path's direct
    counterpart.
  - `ExternalBeginFrameSource` (`:443`) — for the missed-frame replay path.
- `components/viz/common/frame_sinks/begin_frame_source.cc`:
  - `CheckBeginFrameContinuity` (`:57`) — the forward-only frame filter.
  - `IssueBeginFrameToSchedulerClient` (`:238`) and
    `IssueBeginFrameToInputClient` (`:245`) — the input-before-scheduler
    ordering.
  - `DelayBasedBeginFrameSource::IssueBeginFrameToObserver` (`:476`).

**Zed current state.** The scheduler is still a pile of methods on
`Window`-owned structs: `BeginFrameScheduler` (`window.rs:2071`) and
`FrameSchedulerState` (`window.rs:1335`). The GPUI platform layer now has
`BeginFrameObserver`/`BeginFrameSource` traits (`platform.rs:655`/`:665`) plus
shared observer-base continuity helpers (`platform.rs:680`, `:723`, `:746`), and
macOS has a `DelayBasedBeginFrameSource` analog in `MacBeginFrameSource`
(`gpui_macos/src/window.rs:485`) that owns scheduler observer registration,
the scheduler observer callback, `DidFinishFrame` ack state, missed-frame replay
continuity, and input-client-before-scheduler delivery (`:488`, `:604`, `:627`,
`:3355`). `InputRateTracker` and `BeginFrameScheduler` now implement
`BeginFrameObserver` (`gpui/src/window.rs:1219`, `:2418`), and source
BeginFrames are routed through those observer methods (`:4258`, `:4366`). The
public GPUI platform boundary is now observer-shaped: `PlatformWindow` exposes
`set_begin_frame_observer(BeginFrameObserverDispatch)` (the dispatch enum
carries the `Scheduler`/`Input` kind) plus
`add_begin_frame_observer(BeginFrameObserverKind)` /
`remove_begin_frame_observer(BeginFrameObserverKind)`, replacing the old
`on_request_frame` / `on_begin_frame_for_input` / `set_needs_begin_frame`
callback-registration methods.

**Work items.**
1. **Done.**
   Introduce `trait BeginFrameObserver` with `on_begin_frame(&self, args)` and
   `did_finish_frame(...)` matching `BeginFrameObserver` (`begin_frame_source.h:38`)
   and `BeginFrameObserverBase` (`:90`), including the continuity check from
   `CheckBeginFrameContinuity` (`begin_frame_source.cc:57`) implemented once in
   the base. Current code: `platform.rs:655`, shared continuity state/helpers
   at `platform.rs:680`/`:723`/`:746`, macOS continuity state in
   `MacBeginFrameSource` (`gpui_macos/src/window.rs:485`), and observer impls for
   input and scheduler at `gpui/src/window.rs:1219`/`:2418`.
2. **Done for the macOS scheduler observer.** Introduce `trait BeginFrameSource`
   with `add_observer`/`remove_observer`/
   `did_finish_frame` mirroring `BeginFrameSource` (`begin_frame_source.h:137`,
   `:228-229`, `:224`). Current code: `platform.rs:665`; macOS implements it at
   `gpui_macos/src/window.rs:627`.
3. **Done.** Implement `DelayBasedBeginFrameSource` as a struct wrapping the existing
   macOS `DisplayLink` (`crates/gpui_macos/src/display_link.rs`), exposing
   `IssueBeginFrameToSchedulerClient` + `IssueBeginFrameToInputClient`
   ordering (`begin_frame_source.cc:238-245`). This replaces the direct
   `step()` → callback call. Current code routes `step()` through
   `MacBeginFrameSource` state, input-client delivery, and scheduler observer
   callback storage (`gpui_macos/src/window.rs:3120`, `:3134`, `:3330`).
4. **Done.** Move `FrameSchedulerState` to register itself as the observer (analog of
   `DisplayScheduler::BeginFrameObserver`, see Phase 2). `InputRateTracker`
   (`window.rs:1116`) is a second observer consuming
   `IssueBeginFrameToInputClient`, installed via
   `set_begin_frame_observer(BeginFrameObserverDispatch::Input)` (`window.rs:4258`).
   `BeginFrameScheduler` is the scheduler observer adapter
   (`gpui/src/window.rs:2469`) and `InputRateTracker` the input observer
   (`:1219`); both are wired through the `PlatformWindow` observer API, no
   longer callback registration.
5. **Done.** `set_needs_begin_frame` (`platform.rs:748`, removed) became
   `add_begin_frame_observer` / `remove_begin_frame_observer` of
   `BeginFrameObserverKind` on `PlatformWindow`
   (`begin_frame_source.h:228-229`), including the missed-frame replay on add
   (the "gets missed BeginFrameArgs for the given observer" behavior documented
   at `begin_frame_source.h:490`). macOS maps
   `add_begin_frame_observer(Scheduler)` /
   `remove_begin_frame_observer(Scheduler)` onto
   `MacBeginFrameSource::set_scheduler_observer_registered(true/false)` plus the
   display-link subscription update and missed-frame replay
   (`gpui_macos/src/window.rs:1937`).

**Acceptance.** A single `DelayBasedBeginFrameSource` instance feeds both the
scheduler and the input tracker via the observer list; no production code path
calls the frame callback except through `IssueBeginFrameToSchedulerClient`/
`IssueBeginFrameToInputClient`. Continuity filtering lives in one place.

---

### Phase 2 — Promote the scheduler to a real `DisplayScheduler` state machine

**Chromium reference.**
- `components/viz/service/display/display_scheduler.h`:
  - The `BeginFrameDeadlineMode` enum (`:117-137`) — note Chromium has only
    `kImmediate`/`kRegular`/`kLate`/`kNone`; Zed's extra `Blocked`/`WaitForScroll`
    are documented local additions (see below).
  - State fields (`:171-200`): `visible_`, `output_surface_lost_`,
    `inside_begin_frame_deadline_interval_`, `needs_draw_`,
    `pending_swaps_`, `next_swap_id_`, `observing_begin_frame_source_`,
    `last_targeted_latch_time_`, the `decider_` member (`:203`).
  - Methods: `OnBeginFrameContinuation` (`:103`), `ScheduleBeginFrameDeadline`
    (`:146`), `OnBeginFrameDeadline` (`:148`), `AttemptDrawAndSwap` (`:147`),
    `DrawAndSwap` (`:149`), `ShouldDraw` (`:153`), `DidFinishFrame` (`:154`),
    `SetVisible` (`:54`), `OutputSurfaceLost` (`:59`),
    `ForceImmediateSwapIfPossible` (`:55`), `DidReceiveSwapBuffersAck` (`:59`),
    `OnPresentationFeedback` (`:67`), `OnDisplayDamaged` (`:75`),
    `OnRootFrameMissing` (`:76`), `OnPendingSurfacesChanged` (`:77`),
    `DesiredBeginFrameDeadlineMode` (`:145`),
    `AdjustedBeginFrameDeadlineMode` (`:144`), `SetNeedsOneBeginFrame` /
    `MaybeStartObservingBeginFrames` (`display_scheduler.cc:519`, `:536`).
- `components/viz/service/display/display_scheduler.cc`:
  - `ShouldDraw` (`:569`): `needs_draw_ && !output_surface_lost_ && visible_ && …`.
  - `OnBeginFrameContinuation` (`:430`), `OnBeginFrame` (`:382`),
    `OnPresentationFeedback` (`:280`), `DrawAndSwap` (`:305`),
    `ForceImmediateSwapIfPossible` (`:195`), `OutputSurfaceLost` (`:215`).
- `components/viz/service/display/display_scheduler_base.{h,cc}` — the shared
  base; Zed's analog is the extracted scheduler struct.
- `cc/scheduler/scheduler_state_machine.{h,cc}` — the deeper LTH-side state
  machine; useful for transition-table discipline even though Zed has no
  main-thread/impl-thread split. In particular the `Action` /
  `MajorStateOutput` enum pattern (`cc/scheduler/scheduler_state_machine.h`)
  is the model for making deadline-mode selection an explicit transition
  rather than ad-hoc branches.

**Zed current state.** `FrameSchedulerState` (`window.rs:1335`) is now a
cloneable handle around one owned `FrameSchedulerData` record (`window.rs:1339`)
instead of a bag of per-field `Rc<Cell<…>>` handles. The methods exist
(`on_begin_frame_continuation:2190`, `schedule_begin_frame_deadline:2308`,
`on_begin_frame_deadline:2421`, `attempt_draw_and_swap:2440`,
`did_finish_frame:2474`) and deadline-mode selection is split into
`DesiredBeginFrameDeadlineMode` (`window.rs:1980`,
`desired_begin_frame_deadline_mode_transition:2024`) plus `BeginFrameDeadlineMode`
adjustment (`adjusted_begin_frame_deadline_mode_transition:2068`). `ShouldDraw` is now an
explicit scheduler method (`window.rs:1615`), and `visible`, `needs_draw`,
`output_surface_lost`, `pending_swaps`, and `observing_begin_frame_source` are
scheduler-owned state (`window.rs:1349`, `:1356-1359`, transitions at `:1625`,
`:1633`, `:1651`, `:1655`, `:1660`, `:1699`, `:1709`). `WindowInvalidator`
still owns damage details and profiling metadata until Phase 6.

**Work items.**
1. **Done for the current scheduler scope.** Replace the `Rc<Cell<…>>` bag with an owned struct whose fields mirror
   `display_scheduler.h:171-200` (`visible`, `output_surface_lost`,
   `inside_begin_frame_deadline_interval`, `needs_draw`, `pending_swaps`,
   `observing_begin_frame_source`, `last_targeted_latch_time`). Current code:
   `FrameSchedulerData` owns scheduler-local state (`window.rs:1339`),
   including `needs_draw` (`:1349`), `visible`/`output_surface_lost`/
   `pending_swaps`/`observing_begin_frame_source` (`:1356-1359`), and
   `last_targeted_latch_time` (`:1361`). `WindowInvalidator` remains the damage
   accumulator until Phase 6, but no longer owns the scheduler's draw intent.
2. **Done for the current prototype modes.** Promote `BeginFrameDeadlineMode`
   selection (`window.rs:1980-2105`) into a
   `DesiredBeginFrameDeadlineMode` (`display_scheduler.h:145`) +
   `AdjustedBeginFrameDeadlineMode` (`:144`) pair. Keep Zed's local `Blocked`
   and `WaitForScroll` modes but document them as single-process additions
   (Chromium expresses `Blocked` as `SetIsGpuBusy` + `OnBeginFrameContinuation`
   parking; `WaitForScroll` as the scroll deadline ratio, see Phase 3).
3. **Done for the current single-process damage model.** Implement `ShouldDraw`
   as one method matching `display_scheduler.cc:569`. Current code:
   `FrameSchedulerState::should_draw` (`window.rs:1615`) checks
   `needs_draw && !output_surface_lost && visible`; the missing
   `root_frame_missing` term belongs to Phase 6's damage tracker.
4. **Done for the current single-process scheduler.** Implement `SetVisible`/`OutputSurfaceLost`/`ForceImmediateSwapIfPossible`
   as explicit state transitions with the `display_scheduler.cc:215`/
   `:195`/`:155` semantics. The existing `on_output_surface_lost:2432` and
   resize force path become calls into these. Current code has scheduler-owned
   `set_visible` (`window.rs:1633`), `output_surface_lost_transition` (`:1651`),
   `force_immediate_deadline_request` (`:1872`), and observer start/stop
   transitions (`:1699`, `:1709`), with the resize path routed through
   `Window::force_immediate_swap_if_possible` (`:5849`).
5. **Done.** Encode the legal mode transitions in a table or match (style of
   `cc/scheduler/scheduler_state_machine.cc`) rather than scattered branches,
   so the one-draw-per-BeginFrame, GPU-busy parking, and late-deadline
   behaviors are visibly exhaustive. Current code has explicit transition
   inputs and match tables for desired mode (`window.rs:1998`, `:2024`) and
   adjusted mode (`window.rs:2009`, `:2068`), with direct table tests at
   `window.rs:3718` and `:3778`.

**Acceptance.** The scheduler struct's field set is a 1:1 mirror of
`display_scheduler.h:171-200` (plus the documented local additions). Every
transition has a Chromium citation in its doc comment. The existing scheduler
tests in `window.rs` (`:2497-3457`, `failed_swap_completion_marks_output_surface_lost:3666`,
`output_surface_lost_state_recovers_only_when_requested:3255`, etc.) continue
to pass, and new tests assert the transition table is exhaustive.

---

### Phase 3 — Port `PossibleDeadlines` and `FrameDeadlineDecider`

**Chromium reference.**
- `components/viz/common/frame_sinks/begin_frame_args.h`:
  - `PossibleDeadline` (`:72`) — `vsync_id`, `latch_delta`, `present_delta`.
  - `PossibleDeadlines` (`:100`) — `os_preferred_index` + `deadlines` vector.
  - The `possible_deadlines` field on `BeginFrameArgs` (`:257`).
- `components/viz/service/display/frame_deadline_decider.h` — the whole file.
  `SelectDeadline` (`frame_deadline_decider.cc:25`) is the selection algorithm,
  including: `use_platform_preferred_deadlines_` short-circuit (`:76`),
  in-sequence stickiness via `FindClosestDeadlineByPresentation` (`:143`),
  the `max_allowed_buffers`-driven `target_present_delta` (`:101`), and the
  **perceptible-latency input-aware cap** using
  `kPerceptibleLatencyThreshold` (100 ms, `frame_deadline_decider.h:32`) minus
  `1.25 * vsync_interval` (`frame_deadline_decider.cc:90-100`).
  `OnGoIdle` (`frame_deadline_decider.h`, `:115` of `.cc`) resets sequence
  state.
- `components/viz/service/display/display_scheduler.cc`:
  `MaxPendingSwapsForDeadline` (`:474`) — the per-deadline buffer cap derived
  from `present_delta / interval` with the `0.8` rounding bias.

**Zed current state.** `PossibleDeadline` / `PossibleDeadlines` now exist in
`platform.rs:638` / `platform.rs:646`, and `BeginFrameArgs`
(`platform.rs:660`) carries `possible_deadlines`. The macOS
`CVDisplayLink` source populates OS-preferred plus forward-vsync candidates in
`display_link.rs:187`; because `CVDisplayLink` currently gives us one output
timestamp, `latch_delta` is provisionally the same as `present_delta` until the
ported decider has a richer latch model. Deadline selection is still split
between `desired_begin_frame_deadline_mode` (`window.rs:2051`) and
`adjusted_begin_frame_deadline_mode` (`window.rs:2097`), and it still produces
one deadline rather than a selected candidate index. The "future latch
advancement" for presentation groups
(`window.rs:1271 presentation_group_timing_for_request`) remains the
hand-rolled approximation of `SelectDeadline`.

**Work items.**
1. [x] Add `PossibleDeadline { vsync_id, latch_delta, present_delta }` and
   `PossibleDeadlines { os_preferred_index, deadlines }` to
   `platform.rs`, mirroring `begin_frame_args.h:72/100`. Add
   `possible_deadlines: Option<PossibleDeadlines>` to `BeginFrameArgs`
   (`begin_frame_args.h:257`).
2. [x] Populate `PossibleDeadlines` on the macOS source from `CVDisplayLink`
   timing: generate the OS-preferred deadline plus forward-vsync candidates,
   each carrying its `vsync_id` (the display's vsync counter) and deltas from
   `frame_time`.
3. [x] Port `FrameDeadlineDecider` (`frame_deadline_decider.h/.cc`) as a Rust
   struct (`platform.rs` `FrameDeadlineDecider`). `select_deadline` mirrors
   `SelectDeadline` (`:24`) including the in-sequence stickiness
   (`find_closest_deadline_by_presentation`, `:142`) and the `on_go_idle` reset
   (`:134`). The decider is not yet wired into the scheduler's
   `presentation_group_timing_for_request` future-latch advancement — that
   integration replaces the hand-rolled approximation once the scheduler calls
   `select_deadline` instead of computing a single timestamp.
4. [x] Port the **input-aware perceptible-latency cap**
   (`frame_deadline_decider.cc:84-100`): when `earliest_input_time` is known,
   `select_deadline` clamps `target_present_delta` to
   `100 ms − vsync_interval − 0.25·vsync_interval − input_delta`.
5. [x] Port `MaxPendingSwapsForDeadline` (`display_scheduler.cc:479`) as
   `max_pending_swaps_for_deadline(present_delta, interval)` (`platform.rs`).
   The function is not yet wired into the scheduler's swap cap — that
   integration replaces the static `IOSURFACE_MAX_PENDING_SWAPS` read once the
   scheduler consumes the decider's selected deadline.

**Acceptance.** A frame's selected deadline is an index into a
`PossibleDeadlines` vector chosen by the ported decider, not a single computed
timestamp. The decider's source maps 1:1 to `frame_deadline_decider.cc`.

---

### Phase 4 — Trace-ID presentation-feedback matching

**Chromium reference.**
- `components/viz/common/frame_timing_details.h` — `FrameTimingDetails`
  (`:15`) wrapping `gfx::PresentationFeedback` (`:30`).
- `ui/gfx/presentation_feedback.h` — `gfx::PresentationFeedback`. Fields:
  `timestamp` (`:43`, scan-out begin), `interval` (`:65`), `flags` (`:68`, with
  `kVSync` `:24`, `kHWCompletion` `:33`, `kFailure` `:36`), and the
  **`display_trace_id`** (`:107`, `std::optional<int64_t>`) — the
  trace/matching key. Its doc comment names the exact set-site: "set in
  `viz::Display::DidReceivePresentationFeedback()` … See also
  `gpu::SwapBuffersCompleteParams.swap_trace_id`".
- `components/viz/service/display/display.h` — `DidReceivePresentationFeedback`
  (`:169`); the `swap_trace_id` plumbing at `display.h:257/270/286`.
- `components/viz/service/display/display.cc:802-812` —
  `DidReceivePresentationFeedback` allocates a global `display_trace_id`
  (`:802`), flows it through perfetto (`:805`), and sets it on the feedback
  (`:812`). This is the assignment the doc comment references.
- `components/viz/service/display/display_scheduler.cc:280` —
  `OnPresentationFeedback` consumes the matched feedback.

**Zed current state.** `pending_presentation_groups`
(`window.rs:1359`) is a `VecDeque` drained FIFO by
`take_pending_presentation_group` (`window.rs:1576`). The feedback-lifecycle
audit proved this drains correctly, but identified one soft mis-attribution:
when a newer frame fails (feedback from the Metal completion thread) before an
older frame completes, the older group's metadata is popped for the newer
frame's feedback. Today's `PresentationFeedback` (`platform.rs:789`) carries
no id.

**Work items.**
1. Add a monotonically increasing `swap_id` to `SwapCompletionFeedback` and
   `PresentationFeedback` (`platform.rs:802/789`), analog of `display.h`
   `swap_n`. The macOS presenter already has `next_submission_order`
   (`metal_renderer.rs`); reuse it as the swap id stamped onto both the
   `PresentedIosurfaceFrame` and its emitted feedback.
2. Change `record_pending_presentation_group` (`window.rs:1562`) to key the
   group by swap id, and change `on_presentation_feedback` (`window.rs:1533`)
   to match by id (like `display.h` swap_n matching) instead of FIFO pop.
3. Keep the FIFO drain as a fallback only for platforms that do not stamp a
   swap id (`supports_swap_completion_feedback` false, `platform.rs:892`).
4. Audit every submitted-but-not-presented path (Failed, Skipped,
   Deferred-then-later) still drains, reusing the verification from the prior
   audit but now keyed by id.

**Acceptance.** No presentation-group metadata can be mis-attributed under
out-of-order Metal completion + newer-frame failure. The drain proof still
holds.

---

### Phase 5 — Refresh-rate-dependent swap caps

**Chromium reference.**
- `components/viz/service/display/display_scheduler.cc:455` —
  `MaxPendingSwapsForRefreshRate()`: explicit 72/90/120 Hz tiers
  (`k72HzInterval`, `k90HzInterval`, `k120HzInterval`), each optional via
  `PendingSwapParams` (`display_scheduler.h:38-52`).
- `display_scheduler.cc:491` — `MaxPendingSwaps()` combines the refresh-rate
  cap with the per-deadline cap (`MaxPendingSwapsForDeadline`, `:474`,
  ported in Phase 3) and the allocated-buffer fallback.

**Zed current state.** `max_pending_swaps` (`platform.rs:895`) returns the
static `IOSURFACE_MAX_PENDING_SWAPS`. The scheduler reads it once into
`max_pending_platform_swaps` (`window.rs:1080`). No refresh-rate dependence.

**Work items.**
1. Add `max_pending_swaps_120hz`/`90hz`/`72hz` optional params to the macOS
   presenter config, mirroring `PendingSwapParams` (`display_scheduler.h:38-52`).
2. Port `MaxPendingSwapsForRefreshRate` (`display_scheduler.cc:455`) keyed off
   `current_begin_frame_args_.interval`. Expose it through
   `max_pending_swaps` (`platform.rs:895`) returning the current value rather
   than a static constant.
3. The scheduler re-reads the cap when the BeginFrame interval changes (new
   display, ProMotion throttle), not just at window creation
   (`window.rs:4527`).

**Acceptance.** On a 120 Hz display, the pending-swap cap is the 120 Hz tier,
not the 60 Hz default. Verified with a unit test feeding a 8.5 ms interval.

---

## Phase V — Validation gate (run before 6-9)

This is the fork that decides whether Phases 6-9 are necessary at all.

**Chromium reference for methodology.** Chromium's own latency accounting is
`EventLatency` (Phase 7) and `gfx::PresentationFeedback`; the external analog
is `typometer` (https://github.com/frarees/typometer), already called out in
`research.md` as the tool `probably-neb` requested and `j-c-m` used to attach
hard numbers to [zed#26900](https://github.com/zed-industries/zed/issues/26900).

**Work items.**
1. Run `typometer` on three builds, 60 Hz + LPM where possible:
   - default CA/IOSurface path (this branch),
   - `ZED_MACOS_LEGACY_METAL_LAYER=1` (legacy `CAMetalLayer`),
   - VS Code (Chromium's untuned path — the parity target).
2. Runtime QA matrix: resize (live + ending), fullscreen toggle, window
   tabbing, app activation, multiple windows, transparent titlebar, moving
   between displays with different scale factors. These are the highest-risk
   `CALayer` integration edges (called out in `research.md` "Open questions").
3. Record numbers in `research.md`.

**Decision gate.**
- **Gap closed** → ship after QA; Phases 6-9 become optional polish.
- **Gap partially closed** → Phase 7 (EventLatency) first, because it
  attributes *where* the residual lives; Phases 6/8/9 follow the attribution.
- **Gap unchanged** → the present-model thesis is in question; do **not**
  spend on 6-9 until re-diagnosed.

**Acceptance.** Numbers are recorded and the gate decision is written into
this plan before Phase 6 begins.

---

### Phase 6 — Port `DisplayDamageTracker`

**Chromium reference.**
- `components/viz/service/display/display_damage_tracker.h` — the whole file.
  Key surface: `Delegate::OnDisplayDamaged` (`:34`), `OnRootFrameMissing`
  (`:35`), `OnPendingSurfacesChanged` (`:36`); `DisplayResized` (`:64`),
  `SetNewRootSurface` (`:67`), `SetRootSurfaceDamaged` (`:70`),
  `HasPendingSurfaces` (`:95`), `HasDamageDueToInteraction` (`:99`),
  `GetEarliestInputGenerationTimeOfDamagedSurfaces` (`:103` — feeds Phase 3's
  input-aware deadline cap), `DidFinishFrame` (`:106`),
  `CheckForDisplayDamage` (`:109`), and the resize/interaction fields
  (`:145-147`).
- `components/viz/service/display/display_damage_tracker.cc` — the
  per-surface ack/damage accounting (`SurfaceBeginFrameState`).
- `components/viz/service/display_embedder/buffer_queue.h` — the per-buffer
  damage invariant Zed's IOSurface queue already mirrors:
  `UpdateBufferDamage` (`:123`), `GetCurrentBuffer`/`SwapBuffers`/`SwapBuffersComplete`
  (`:53/71/79`), and the "frames where `SwapBuffers()` was called without
  `GetCurrentBuffer()`" note (`:188`).

**Zed current state.** Per-buffer damage lives in the IOSurface presenter
(`metal_renderer.rs` `update_buffer_damage`/`buffer_damage`/`clear_buffer_damage`,
already matching `BufferQueue`). But damage *propagation* (which invalidations
affect which buffers, interaction damage, resize-expected damage) is spread
across `scene.rs:67` (`add_damage`), `window.rs:4991` (`mark_view_dirty`), and
conservative full-frame fallbacks for global refresh and unbounded
invalidations (`research.md` "Open questions" flags this). There is no
analog of `HasDamageDueToInteraction` or
`GetEarliestInputGenerationTimeOfDamagedSurfaces`, which Phase 3's input-aware
deadline needs.

**Work items.**
1. Introduce a single-process `DisplayDamageTracker` analog. Zed has no
   `SurfaceId`/surface tree (Half A), so the "surface" granularity is the GPUI
   **view** (`window.rs:4991 mark_view_dirty` already keys by `EntityId`).
   The tracker owns: pending-damage-per-view, `root_frame_missing`,
   `expecting_root_surface_damage_because_of_resize`, `has_surface_damage_due_to_interaction`,
   and `earliest_input_timestamp` — direct mirrors of
   `display_damage_tracker.h:140-147`.
2. Route `refresh()`, `mark_view_dirty`, global refresh, and resize through
   `DisplayResized` (`:64`) / `SetRootSurfaceDamaged` (`:70`) /
   `OnDisplayDamaged` (`:34`) equivalents, so the conservative full-frame
   fallbacks in `research.md` are replaced by the tracker's accounting.
3. Expose `HasDamageDueToInteraction` (`:99`) and
   `GetEarliestInputGenerationTimeOfDamagedSurfaces` (`:103`); wire the latter
   into Phase 3's `SelectDeadline` `earliest_input_time` argument
   (`frame_deadline_decider.cc:25`).
4. Keep the existing `BufferQueue`-shape per-buffer damage invariant in
   `metal_renderer.rs` as the leaf; the tracker decides *what* damage a frame
   carries, the presenter decides *how* a buffer stores it.

**Acceptance.** No production invalidation path falls back to full-frame
damage unless `DisplayResized`/`SetRootSurfaceDamaged` semantics demand it.
The input-aware deadline in Phase 3 receives a real
`earliest_input_generation_time`, not `None`.

---

### Phase 7 — Port `EventLatency` (latency attribution)

**Chromium reference.**
- `cc/metrics/event_latency_tracker.h` — `ReportEventLatency`
  (`:53`, takes `viz::BeginFrameArgs`). This is the per-event, per-stage
  attribution API.
- `cc/metrics/event_latency_tracing_recorder.{h,cc}` — the tracing sink.
- `ui/latency/latency_info.h` — `LatencyInfo` (`:96`) and the
  `LatencyComponentType` enum (`:61-93`) which enumerates the ordered pipeline
  stages Zed must mirror: `INPUT_EVENT_LATENCY_ORIGINAL_COMPONENT` (`:69`, input
  generation), `INPUT_EVENT_LATENCY_UI_COMPONENT` (`:71`),
  `INPUT_EVENT_LATENCY_RENDERING_SCHEDULED_IMPL_COMPONENT` (`:79`, BeginFrame
  scheduling), `INPUT_EVENT_LATENCY_RENDERER_SWAP_COMPONENT` (`:81`),
  `INPUT_EVENT_GPU_SWAP_BUFFER_COMPONENT` (`:88`), and
  `INPUT_EVENT_LATENCY_FRAME_SWAP_COMPONENT` (`:91`, present). `LatencyInfo`
  stamps these via `AddLatencyNumberWithTimestamp` (`:156`).
- `components/viz/service/display/display_damage_tracker.h:99` —
  `OnSurfaceDamaged` takes `const std::vector<ui::LatencyInfo>&`, showing where
  latency info is harvested at the damage boundary.

**Zed current state.** There is a feature-gated `input-latency-histogram`
crate path (`window.rs` `input_latency_tracker.record_frame_presented()`),
but it measures end-to-end only — no per-stage attribution, no tie to
`BeginFrameArgs`. There is no `EventLatency` analog.

**Work items.**
1. Define a single-process `LatencyInfo` analog with the stage enum mirroring
   `ui/latency` component types: input generation, dispatch, BeginFrame issue,
   draw start, submit, GPU ready, latch, present.
2. Stamp a `LatencyInfo` onto each input event (`gpui/src/input.rs` /
   platform event path) and carry it through `BeginFrameArgs` into the draw
   path, harvesting timestamps at each stage as `DisplayDamageTracker::OnSurfaceDamaged`
   does (`display_damage_tracker.h:99`).
3. Port `EventLatencyTracker::ReportEventLatency` (`event_latency_tracker.h:53`)
   to emit per-stage histograms keyed by the presented/dropped/skipped outcome
   (matching Chromium's presented-vs-dropped latency split).
4. Feed the reported latencies back as the `earliest_input_time` source for
   Phase 3's deadline decider, closing the loop.

**Acceptance.** A typed input event produces a per-stage latency breakdown on
the next presented frame, and the histogram distinguishes
presented/dropped/skipped outcomes. This is the diagnostic that makes
Phase V's residual-delta actionable.

---

### Phase 8 — Align the macOS display-link source with `DisplayLinkMac`

**Chromium reference.**
- `ui/display/mac/display_link_mac.h` — `DisplayLinkMac::GetForDisplay`
  (`:106`), `RegisterCallback`, `VSyncCallbackMac` (`:42`,
  `callback_for_displaylink_thread_`), `PresentationCallbackMac` (`:88`),
  and the `callback_timebase`/`callback_interval`/`display_interval` fields
  (`:28-36`). Chromium runs the raw vsync callback on a dedicated
  display-link thread and marshals to clients via `VSyncCallbackMac`/
  `PresentationCallbackMac` handles — lower jitter than Zed's
  `CVDisplayLink` → main-queue `dispatch_source` hop.
- `ui/accelerated_widget_mac/ca_transaction_observer.{h,mm}` — Chromium's
  `CATransaction` phase observer (`pre_commit`/`post_commit`) that Zed
  approximates via the private `+[CATransaction addCommitHandler:forPhase:]`
  gate (`metal_renderer.rs` `supports_ca_transaction_phase_handlers`).

**Zed current state.** `crates/gpui_macos/src/display_link.rs` uses
`CVDisplayLink` + a `dispatch_source` onto the main queue. `research.md`
already flags this as a minor contributor ("CVDisplayLink has more jitter than
CADisplayLink, and the main-queue hop adds scheduling indirection").

**Work items.**
1. Drive the `DelayBasedBeginFrameSource` (Phase 1) from a
   `DisplayLinkMac`-style source: keep the `CVDisplayLink` alive while idle
   (already done per `research.md`), but deliver vsync ticks to observers via
   a `VSyncCallbackMac`-style handle rather than re-dispatching onto the main
   queue before observer delivery.
2. Surface `callback_timebase`/`callback_interval`/`display_interval`
   (`display_link_mac.h:28-36`) as the source of `PossibleDeadlines`' vsync
   deltas in Phase 3.
3. Replace the private-API `addCommitHandler:forPhase:` probe with a
   `CATransactionObserver`-style abstraction matching
   `ca_transaction_observer.h`, so the post-commit feedback delivery
   (Phase 4) is structurally Chromium-shaped.

**Acceptance.** The BeginFrame source's jitter is no longer dominated by the
main-queue dispatch hop; `PossibleDeadlines` deltas come directly from the
display-link timebase.

---

### Phase 9 — Align the CA present path with `CALayerTreeCoordinator`

**Chromium reference.**
- `ui/accelerated_widget_mac/ca_layer_tree_coordinator.{h,mm}`:
  - `PresentedFrame` struct (`ca_layer_tree_coordinator.h:38-39`) carrying
    `SwapCompletionCallback` + `PresentationCallback`.
  - `presented_frames_` queue (`:159`) with the `has_committed` front gate.
  - `EnqueueBackpressureFences` (`:112`/`.mm:73`),
    `ApplyBackpressure` (`:102`/`.mm:81` — waits the metal/GL fences of the
    committed front before reusing buffers).
  - `CommitPresentedFrameToCA` (`:106`/`.mm:188`) — the single CA commit
    boundary that swaps the front, applies the CAContext fence port
    (`setFencePort:`, `.mm:217`), and hands the layer tree off.
- `ui/accelerated_widget_mac/display_ca_layer_tree.{h,mm}` — the browser-side
    half (in Chromium this lives across the process boundary; in Zed it is
    the local `remote_layer.rs` `CALayerHost` topology).
- `ui/accelerated_widget_mac/window_resize_helper_mac.{h,cc}` — the
  `WaitForSingleTaskToRun` live-resize drain that Zed approximates with the
  `ResizeFrameWaitHandle` in `metal_renderer.rs`.

**Zed current state.** `metal_renderer.rs` already mirrors most of this:
`PresentedIosurfaceFrame` (`:186`), `IosurfaceSubmissionQueue`,
`commit_iosurface_frame:890`, `complete_iosurface_frame:802`,
`fail_iosurface_frame:857`, the `MTLSharedEvent` backpressure fence, the
`presented_frames` FIFO, and `remote_layer.rs` for the `CAContext`/`CALayerHost`
shape. The remaining gaps are structural alignment, not functionality.

**Work items.**
1. Rename/restructure so the Zed types map 1:1 to the coordinator names:
    `PresentedIosurfaceFrame` → `PresentedFrame` analog with explicit
    `has_committed` front flag (today's queue encodes this implicitly via
    `ready` + FIFO commit).
2. Make `ApplyBackpressure` (`ca_layer_tree_coordinator.mm:81`) the single
    call site that applies the committed-frame `MTLSharedEvent` fence before
    buffer reuse, replacing the inlined `apply_committed_backpressure`
    (`metal_renderer.rs`).
3. Make `CommitPresentedFrameToCA` (`.mm:188`) the single CA-commit boundary,
    consolidating the current `commit_iosurface_frame` + `recreate_ca_context_within_transaction`
    + fence-port path so the structure matches Chromium's commit ordering.
4. Confirm `display_ca_layer_tree` ↔ `remote_layer.rs` topology parity: stable
    root layer, `CALayerHost` child with `kCALayerMaxXMargin | kCALayerMaxYMargin`
    autoresizing mask (already noted in `research.md`), fence-port lifecycle
    on resize.

**Acceptance.** A reader can open `ca_layer_tree_coordinator.mm` and the Zed
coordinator side by side and follow the same call sequence:
`EnqueueBackpressureFences` → `ApplyBackpressure` → `CommitPresentedFrameToCA`
→ swap-completion/presentation callbacks.

---

## 4. Explicit non-goals (Half A — do not build)

Each is cited so future readers do not re-flag them.

- **GPU process + Mojo IPC.** `viz/service/main` + `gpu/ipc` exist in Chromium
  for sandboxing untrusted renderers. Zed's renderer is trusted and in-process.
  A local IPC hop would add latency. `research.md` declares this a non-goal.
- **`SurfaceAggregator` / `CompositorFrame` / `DrawQuad` surface tree.**
  `components/viz/service/display/surface_aggregator.{h,cc}` composites
  multiple independent compositor sources (iframes, OOPIFs, extensions). Zed
  has one scene tree (`gpui/src/scene.rs`). The Phase 6 damage tracker uses
  *view* granularity as the single-process analog.
- **Browser-side `DisplayCALayerTree` across a process boundary.**
  `ui/accelerated_widget_mac/display_ca_layer_tree.mm` is the browser-process
  half; Zed keeps both halves local in `remote_layer.rs`. Verified: `rg` for
  `CompositorFrame|SurfaceId|FrameSinkManager` across `crates/` returns zero
  real hits.
- **Sandboxing / untrusted content.** No web content, no iframes, no plugins.

---

## 5. Sequencing summary

| Order | Phase | Why this order |
|---|---|---|
| 1 | Phase 1 (BeginFrame graph) | Structural base for everything; unblocks clean observer wiring |
| 2 | Phase 2 (state machine) | Needs Phase 1's observer; makes 3-5 expressible |
| 3 | Phase 3 (PossibleDeadlines) | The core latency algorithm; needs 2's deadline-mode shape |
| 4 | Phase 4 (trace-id feedback) | Independent; small; removes the one verified soft edge |
| 5 | Phase 5 (refresh-rate caps) | Independent; small; needs Phase 3's decider |
| — | **Phase V (typometer gate)** | **Runs after 1-5. Output decides 6-9 scope.** |
| 6 | Phase 7 (EventLatency) | The attribution diagnostic; run next if gate shows residual |
| 7 | Phase 6 (damage tracker) | Reduces fallbacks; feeds Phase 3's input-aware cap |
| 8 | Phase 8 (DisplayLinkMac) | Jitter polish; lowest latency payoff |
| 9 | Phase 9 (CALayerTree align) | Structural rename; no behavior change |

Phases 1-5 are the "real deal" core. Phase V is the gate. 6-9 are validated
follow-ups, not assumed necessities.

---

## 6. Definition of done (goal B)

The pipeline is "the real deal" when **all** hold:

1. Phases 1-5 are merged and their acceptance criteria met.
2. Phase V has run and its decision is recorded here.
3. For every scheduler/present/feedback type in Zed, a reviewer can open the
   cited Chromium file and see a structural correspondence — no approximation
   is left undocumented.
4. `typometer` on the default path is at parity with (or better than) VS Code
   on the tested hardware, **or** Phase V has produced a specific residual
   diagnosis with a concrete next item (not "port more scheduler").

Half A (multi-process / surface-tree) remains explicitly unbuilt and is
documented above as out of scope so the goal "implement the whole pipeline" is
interpreted as "the whole **latency-bearing** pipeline," not "rebuild Zed as
Chromium."

All Chromium citations in this plan are line-level verifiable. The active
sparse-checkout cones (`REFERENCE/chromium/.git/info/sparse-checkout`) are:
`cc/` (metrics, scheduler, trees), `components/viz/` (common + service), `gpu/`
(command_buffer service + ipc), `ui/accelerated_widget_mac/`, `ui/display/`
(mac, types), `ui/gl/`, `ui/base/`, `ui/latency/`, and `ui/gfx/`. The
`ui/latency/` and `ui/gfx/` cones were added so Phase 4 (`presentation_feedback.h`)
and Phase 7 (`latency_info.h`) carry the same direct line references as the
other phases.
