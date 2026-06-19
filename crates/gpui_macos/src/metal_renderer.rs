use crate::{ca_time, metal_atlas::MetalAtlas, remote_layer::CoreAnimationLayerTree};
use anyhow::Result;
use block::ConcreteBlock;
use cocoa::{
    base::{NO, YES, id},
    foundation::{NSSize, NSUInteger},
    quartzcore::AutoresizingMask,
};
use gpui::{
    AtlasTextureId, Background, Bounds, ContentMask, DevicePixels, MonochromeSprite, PaintSurface,
    Path, PlatformDrawResult, Point, PolychromeSprite, PresentationFeedback, PrimitiveBatch, Quad,
    ScaledPixels, Scene, Shadow, Size, Surface, SwapCompletionFeedback, SwapCompletionResult,
    Underline, bounds, point, size,
};
#[cfg(any(test, feature = "test-support"))]
use image::RgbaImage;

use core_foundation::{
    base::{CFType, TCFType},
    dictionary::CFDictionary,
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use core_video::{
    metal_texture::CVMetalTextureGetTexture, metal_texture_cache::CVMetalTextureCache,
    pixel_buffer::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use foreign_types::{ForeignType, ForeignTypeRef};
use metal::{
    CommandQueue, MTLCommandBufferStatus, MTLGPUFamily, MTLPixelFormat, MTLResourceOptions,
    MTLScissorRect, NSRange, RenderPassColorAttachmentDescriptorRef, SharedEvent,
};
use objc::{self, class, msg_send, sel, sel_impl};
use parking_lot::Mutex;

use std::{
    cell::Cell,
    collections::VecDeque,
    ffi::c_void,
    mem, ptr,
    rc::Rc,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

// Exported to metal
pub(crate) type PointF = gpui::Point<f32>;

#[cfg(not(feature = "runtime_shaders"))]
const SHADERS_METALLIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders.metallib"));
#[cfg(feature = "runtime_shaders")]
const SHADERS_SOURCE_FILE: &str = include_str!(concat!(env!("OUT_DIR"), "/stitched_shaders.metal"));
// Use 4x MSAA, all devices support it.
// https://developer.apple.com/documentation/metal/mtldevice/1433355-supportstexturesamplecount
const PATH_SAMPLE_COUNT: u32 = 4;
const LEGACY_METAL_LAYER_ENV: &str = "ZED_MACOS_LEGACY_METAL_LAYER";
const IOSURFACE_BUFFER_COUNT: usize = 3;
const IOSURFACE_MAX_PENDING_SWAPS: usize = IOSURFACE_BUFFER_COUNT - 1;
const CA_TRANSACTION_PHASE_POST_COMMIT: usize = 2;
const NO_CURRENT_BUFFER: usize = usize::MAX;
const BGRA_IOSURFACE_PIXEL_FORMAT: i32 = i32::from_be_bytes(*b"BGRA");

pub(crate) type Context = Arc<Mutex<InstanceBufferPool>>;
pub(crate) type Renderer = MetalRenderer;

pub(crate) unsafe fn new_renderer(
    context: self::Context,
    _native_window: *mut c_void,
    _native_view: *mut c_void,
    _bounds: gpui::Size<f32>,
    transparent: bool,
) -> Renderer {
    if std::env::var_os(LEGACY_METAL_LAYER_ENV).is_some() {
        MetalRenderer::new(context, transparent)
    } else {
        MetalRenderer::new_chromium_pipeline(context, transparent, _bounds)
    }
}

pub(crate) struct InstanceBufferPool {
    buffer_size: usize,
    buffers: Vec<metal::Buffer>,
}

impl Default for InstanceBufferPool {
    fn default() -> Self {
        Self {
            buffer_size: 2 * 1024 * 1024,
            buffers: Vec::new(),
        }
    }
}

pub(crate) struct InstanceBuffer {
    metal_buffer: metal::Buffer,
    size: usize,
}

struct CoreAnimationPresenter {
    layer_tree: CoreAnimationLayerTree,
    drawable_size: Size<DevicePixels>,
    buffers: Vec<IosurfaceFrameBuffer>,
    pending_swap_count: Arc<AtomicUsize>,
    current_buffer_index: Arc<AtomicUsize>,
    generation: Arc<AtomicUsize>,
    next_submission_order: Arc<AtomicUsize>,
    backpressure_event: SharedEvent,
    next_backpressure_value: AtomicU64,
    committed_backpressure_fence: Arc<Mutex<Option<IosurfaceBackpressureFence>>>,
    presented_frames: Arc<Mutex<IosurfaceSubmissionQueue<Box<PresentedIosurfaceFrame>>>>,
    deferred_frame_requested: Arc<AtomicBool>,
    deferred_frame_callback: Option<Arc<dyn Fn() + Send + Sync>>,
    has_resized_since_last_swap: bool,
    swap_completion_callback: Option<Arc<dyn Fn(SwapCompletionFeedback) + Send + Sync>>,
    presentation_feedback_callback: Option<Arc<dyn Fn(PresentationFeedback) + Send + Sync>>,
    latest_display_timing: Arc<Mutex<Option<FrameDisplayTiming>>>,
}

#[derive(Clone, Copy)]
struct FrameDisplayTiming {
    next_display_time: scheduler::Instant,
    frame_interval: Option<Duration>,
}

#[allow(deprecated)]
struct IosurfaceFrameBuffer {
    surface: io_surface::IOSurface,
    texture: metal::Texture,
    pending: Arc<AtomicBool>,
    damage: Arc<Mutex<Bounds<DevicePixels>>>,
}

#[allow(deprecated)]
struct PresentedIosurfaceFrame {
    layer: id,
    surface: io_surface::IOSurface,
    buffer_pending: Arc<AtomicBool>,
    pending_swap_count: Arc<AtomicUsize>,
    current_buffer_index: Arc<AtomicUsize>,
    current_generation: Arc<AtomicUsize>,
    generation: usize,
    deferred_frame_requested: Arc<AtomicBool>,
    deferred_frame_callback: Option<Arc<dyn Fn() + Send + Sync>>,
    swap_completion_callback: Option<Arc<dyn Fn(SwapCompletionFeedback) + Send + Sync>>,
    presentation_feedback_callback: Option<Arc<dyn Fn(PresentationFeedback) + Send + Sync>>,
    latest_display_timing: Arc<Mutex<Option<FrameDisplayTiming>>>,
    ready_time: scheduler::Instant,
    buffer_index: usize,
    buffer_damage: Arc<Mutex<Bounds<DevicePixels>>>,
    submitted_damage: Bounds<DevicePixels>,
    backpressure_fence: IosurfaceBackpressureFence,
    committed_backpressure_fence: Arc<Mutex<Option<IosurfaceBackpressureFence>>>,
    delay_until_next_vsync: bool,
}

#[derive(Clone)]
struct IosurfaceBackpressureFence {
    event: SharedEvent,
    value: u64,
}

struct QueuedIosurfaceSubmission<T> {
    order: usize,
    ready: bool,
    frame: T,
}

struct IosurfaceSubmissionQueue<T> {
    frames: VecDeque<QueuedIosurfaceSubmission<T>>,
}

impl<T> Default for IosurfaceSubmissionQueue<T> {
    fn default() -> Self {
        Self {
            frames: VecDeque::new(),
        }
    }
}

impl<T> IosurfaceSubmissionQueue<T> {
    fn push(&mut self, order: usize, frame: T) {
        let queued_frame = QueuedIosurfaceSubmission {
            order,
            ready: false,
            frame,
        };
        let insertion_index = self
            .frames
            .iter()
            .position(|queued_frame| queued_frame.order > order);
        if let Some(insertion_index) = insertion_index {
            self.frames.insert(insertion_index, queued_frame);
        } else {
            self.frames.push_back(queued_frame);
        }
    }

    fn mark_ready_with(&mut self, order: usize, update: impl FnOnce(&mut T)) -> bool {
        let Some(frame) = self
            .frames
            .iter_mut()
            .find(|queued_frame| queued_frame.order == order)
        else {
            return false;
        };
        update(&mut frame.frame);
        frame.ready = true;
        true
    }

    fn remove(&mut self, order: usize) -> Option<T> {
        let index = self
            .frames
            .iter()
            .position(|queued_frame| queued_frame.order == order)?;
        self.frames
            .remove(index)
            .map(|queued_frame| queued_frame.frame)
    }

    fn pop_ready_front(&mut self) -> Option<T> {
        if !self
            .frames
            .front()
            .is_some_and(|queued_frame| queued_frame.ready)
        {
            return None;
        }
        self.frames
            .pop_front()
            .map(|queued_frame| queued_frame.frame)
    }

    fn ready_front(&self) -> Option<&T> {
        self.frames
            .front()
            .filter(|queued_frame| queued_frame.ready)
            .map(|queued_frame| &queued_frame.frame)
    }

    fn ready_front_count(&self) -> usize {
        self.frames
            .iter()
            .take_while(|queued_frame| queued_frame.ready)
            .count()
    }

    fn len(&self) -> usize {
        self.frames.len()
    }
}

struct IosurfaceFrameCompletion {
    presented_frames: Arc<Mutex<IosurfaceSubmissionQueue<Box<PresentedIosurfaceFrame>>>>,
    submission_order: usize,
    status: MTLCommandBufferStatus,
}

impl Drop for PresentedIosurfaceFrame {
    fn drop(&mut self) {
        unsafe {
            let _: () = msg_send![self.layer, release];
        }
    }
}

impl InstanceBufferPool {
    pub(crate) fn reset(&mut self, buffer_size: usize) {
        self.buffer_size = buffer_size;
        self.buffers.clear();
    }

    pub(crate) fn acquire(
        &mut self,
        device: &metal::Device,
        unified_memory: bool,
    ) -> InstanceBuffer {
        let buffer = self.buffers.pop().unwrap_or_else(|| {
            let options = if unified_memory {
                MTLResourceOptions::StorageModeShared
                    // Buffers are write only which can benefit from the combined cache
                    // https://developer.apple.com/documentation/metal/mtlresourceoptions/cpucachemodewritecombined
                    | MTLResourceOptions::CPUCacheModeWriteCombined
            } else {
                MTLResourceOptions::StorageModeManaged
            };

            device.new_buffer(self.buffer_size as u64, options)
        });
        InstanceBuffer {
            metal_buffer: buffer,
            size: self.buffer_size,
        }
    }

    pub(crate) fn release(&mut self, buffer: InstanceBuffer) {
        if buffer.size == self.buffer_size {
            self.buffers.push(buffer.metal_buffer)
        }
    }
}

#[allow(deprecated)]
impl CoreAnimationPresenter {
    fn new(
        device: &metal::Device,
        transparent: bool,
        initial_size: Size<DevicePixels>,
    ) -> Result<Self> {
        let layer_tree = CoreAnimationLayerTree::new(transparent, initial_size);
        log::info!(
            "using Chromium-style {}CA/IOSurface renderer",
            if layer_tree.uses_ca_context() {
                "CAContext/CALayerHost "
            } else {
                ""
            }
        );

        let mut presenter = Self {
            layer_tree,
            drawable_size: size(0.into(), 0.into()),
            buffers: Vec::new(),
            pending_swap_count: Arc::new(AtomicUsize::new(0)),
            current_buffer_index: Arc::new(AtomicUsize::new(NO_CURRENT_BUFFER)),
            generation: Arc::new(AtomicUsize::new(0)),
            next_submission_order: Arc::new(AtomicUsize::new(0)),
            backpressure_event: device.new_shared_event(),
            next_backpressure_value: AtomicU64::new(0),
            committed_backpressure_fence: Arc::new(Mutex::new(None)),
            presented_frames: Arc::new(Mutex::new(IosurfaceSubmissionQueue::default())),
            deferred_frame_requested: Arc::new(AtomicBool::new(false)),
            deferred_frame_callback: None,
            has_resized_since_last_swap: false,
            swap_completion_callback: None,
            presentation_feedback_callback: None,
            latest_display_timing: Arc::new(Mutex::new(None)),
        };
        presenter.update_drawable_size(device, initial_size)?;
        Ok(presenter)
    }

    fn update_drawable_size(
        &mut self,
        device: &metal::Device,
        drawable_size: Size<DevicePixels>,
    ) -> Result<()> {
        if self.drawable_size == drawable_size {
            return Ok(());
        }

        let had_drawable_size = self.drawable_size.width.0 > 0 && self.drawable_size.height.0 > 0;
        self.drawable_size = drawable_size;
        self.layer_tree.set_drawable_size(drawable_size);
        if had_drawable_size && self.layer_tree.uses_ca_context() {
            self.has_resized_since_last_swap = true;
        }
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.current_buffer_index
            .store(NO_CURRENT_BUFFER, Ordering::Release);
        self.buffers.clear();

        if drawable_size.width.0 <= 0 || drawable_size.height.0 <= 0 {
            return Ok(());
        }

        for _ in 0..IOSURFACE_BUFFER_COUNT {
            self.buffers
                .push(IosurfaceFrameBuffer::new(device, drawable_size)?);
        }

        Ok(())
    }

    fn set_contents_scale(&mut self, scale_factor: f64) {
        self.layer_tree.set_contents_scale(scale_factor);
        self.layer_tree.set_drawable_size(self.drawable_size);
    }

    fn set_opaque(&self, opaque: bool) {
        self.layer_tree.set_opaque(opaque);
    }

    fn layer_ptr(&self) -> id {
        self.layer_tree.backing_layer()
    }

    fn content_layer_ptr(&self) -> id {
        self.layer_tree.content_layer()
    }

    fn prepare_to_present(&mut self) {
        if self.has_resized_since_last_swap {
            if self.layer_tree.recreate_ca_context() {
                self.layer_tree.set_drawable_size(self.drawable_size);
            }
            self.has_resized_since_last_swap = false;
        }
    }

    fn full_damage(&self) -> Bounds<DevicePixels> {
        full_texture_bounds(self.drawable_size)
    }

    fn update_buffer_damage(&mut self, damage: Bounds<DevicePixels>) {
        let damage = clamp_damage(damage, self.drawable_size);
        if damage.is_empty() {
            return;
        }

        let current_buffer_index = self.current_buffer_index.load(Ordering::Acquire);
        let full_damage = self.full_damage();
        for (index, buffer) in self.buffers.iter_mut().enumerate() {
            if index != current_buffer_index {
                let mut buffer_damage = buffer.damage.lock();
                *buffer_damage = buffer_damage.union(&damage).intersect(&full_damage);
            }
        }
    }

    fn buffer_damage(&self, buffer_index: usize) -> Bounds<DevicePixels> {
        self.buffers
            .get(buffer_index)
            .map(|buffer| clamp_damage(*buffer.damage.lock(), self.drawable_size))
            .unwrap_or_else(|| self.full_damage())
    }

    fn clear_buffer_damage(&mut self, buffer_index: usize) {
        if let Some(buffer) = self.buffers.get_mut(buffer_index) {
            *buffer.damage.lock() = Bounds::default();
        }
    }

    fn next_buffer_index(&self) -> Option<usize> {
        if self.pending_swap_count.load(Ordering::Acquire) >= IOSURFACE_MAX_PENDING_SWAPS {
            self.deferred_frame_requested.store(true, Ordering::Release);
            return None;
        }

        let current_buffer_index = self.current_buffer_index.load(Ordering::Acquire);
        let next_buffer_index = self
            .buffers
            .iter()
            .enumerate()
            .find(|(index, buffer)| {
                *index != current_buffer_index && !buffer.pending.load(Ordering::Acquire)
            })
            .map(|(index, _)| index);

        if next_buffer_index.is_none() {
            self.deferred_frame_requested.store(true, Ordering::Release);
        }

        next_buffer_index
    }

    fn mark_buffer_pending(&self, buffer_index: usize) {
        if let Some(buffer) = self.buffers.get(buffer_index) {
            buffer.pending.store(true, Ordering::Release);
            self.pending_swap_count.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn next_submission_order(&self) -> usize {
        self.next_submission_order.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn next_backpressure_fence(&self) -> IosurfaceBackpressureFence {
        IosurfaceBackpressureFence {
            event: self.backpressure_event.clone(),
            value: self.next_backpressure_value.fetch_add(1, Ordering::AcqRel) + 1,
        }
    }

    fn apply_committed_backpressure(&self) {
        if let Some(fence) = self.committed_backpressure_fence.lock().take() {
            apply_iosurface_backpressure_fence(&fence);
        }
    }

    fn set_deferred_frame_callback(&mut self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.deferred_frame_callback = callback;
    }

    fn set_swap_completion_callback(
        &mut self,
        callback: Option<Arc<dyn Fn(SwapCompletionFeedback) + Send + Sync>>,
    ) {
        self.swap_completion_callback = callback;
    }

    fn set_presentation_feedback_callback(
        &mut self,
        callback: Option<Arc<dyn Fn(PresentationFeedback) + Send + Sync>>,
    ) {
        self.presentation_feedback_callback = callback;
    }

    fn set_display_timing(
        &self,
        next_display_time: scheduler::Instant,
        frame_interval: Option<Duration>,
    ) -> bool {
        *self.latest_display_timing.lock() = Some(FrameDisplayTiming {
            next_display_time,
            frame_interval,
        });
        commit_ready_iosurface_frame(self.presented_frames.clone())
    }
}

#[allow(deprecated)]
impl IosurfaceFrameBuffer {
    fn new(device: &metal::Device, size: Size<DevicePixels>) -> Result<Self> {
        let surface = new_iosurface(size);
        let texture = new_iosurface_texture(device, &surface, size);

        Ok(Self {
            surface,
            texture,
            pending: Arc::new(AtomicBool::new(false)),
            damage: Arc::new(Mutex::new(full_texture_bounds(size))),
        })
    }
}

fn iosurface_key(reference: CFStringRef) -> CFString {
    unsafe { TCFType::wrap_under_get_rule(reference) }
}

#[allow(deprecated)]
fn new_iosurface(size: Size<DevicePixels>) -> io_surface::IOSurface {
    let width = size.width.0.max(1);
    let height = size.height.0.max(1);
    let bytes_per_row = align_to(width as usize * 4, 256);

    let width_key = iosurface_key(unsafe { io_surface::kIOSurfaceWidth });
    let height_key = iosurface_key(unsafe { io_surface::kIOSurfaceHeight });
    let bytes_per_row_key = iosurface_key(unsafe { io_surface::kIOSurfaceBytesPerRow });
    let bytes_per_element_key = iosurface_key(unsafe { io_surface::kIOSurfaceBytesPerElement });
    let pixel_format_key = iosurface_key(unsafe { io_surface::kIOSurfacePixelFormat });

    let width = CFNumber::from(width);
    let height = CFNumber::from(height);
    let bytes_per_row = CFNumber::from(bytes_per_row as i32);
    let bytes_per_element = CFNumber::from(4);
    let pixel_format = CFNumber::from(BGRA_IOSURFACE_PIXEL_FORMAT);

    let properties = CFDictionary::<CFString, CFType>::from_CFType_pairs(&[
        (width_key, width.as_CFType()),
        (height_key, height.as_CFType()),
        (bytes_per_row_key, bytes_per_row.as_CFType()),
        (bytes_per_element_key, bytes_per_element.as_CFType()),
        (pixel_format_key, pixel_format.as_CFType()),
    ]);

    io_surface::new(&properties)
}

#[allow(deprecated)]
fn new_iosurface_texture(
    device: &metal::Device,
    surface: &io_surface::IOSurface,
    size: Size<DevicePixels>,
) -> metal::Texture {
    let texture_descriptor = metal::TextureDescriptor::new();
    texture_descriptor.set_texture_type(metal::MTLTextureType::D2);
    texture_descriptor.set_width(size.width.0.max(1) as u64);
    texture_descriptor.set_height(size.height.0.max(1) as u64);
    texture_descriptor.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
    texture_descriptor.set_storage_mode(metal::MTLStorageMode::Shared);
    texture_descriptor
        .set_usage(metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead);

    unsafe {
        msg_send![
            device.as_ref(),
            newTextureWithDescriptor: texture_descriptor.as_ref()
            iosurface: surface.as_concrete_TypeRef()
            plane: 0usize
        ]
    }
}

fn align_to(value: usize, alignment: usize) -> usize {
    value.div_ceil(alignment) * alignment
}

fn presentation_feedback_from_ca_time(ca_time: f64) -> PresentationFeedback {
    let display_time = ca_time::media_time_to_instant(ca_time);
    PresentationFeedback {
        ready_time: display_time,
        latch_time: display_time,
        display_time,
        interval: None,
        presented: true,
        vsync: true,
        hardware_completion: true,
    }
}

fn presentation_feedback_for_iosurface_frame(
    ready_time: scheduler::Instant,
    latch_time: scheduler::Instant,
    display_timing: Option<FrameDisplayTiming>,
) -> PresentationFeedback {
    let (display_time, interval) = if let Some(display_timing) = display_timing {
        (
            estimated_display_time_for_latch(display_timing, latch_time),
            display_timing.frame_interval,
        )
    } else {
        (latch_time, None)
    };

    PresentationFeedback {
        ready_time,
        latch_time,
        display_time,
        interval,
        presented: true,
        vsync: true,
        hardware_completion: true,
    }
}

fn estimated_display_time_for_latch(
    display_timing: FrameDisplayTiming,
    latch_time: scheduler::Instant,
) -> scheduler::Instant {
    const LATCH_BUFFER: Duration = Duration::from_micros(1500);

    if let Some(next_latch_deadline) = display_timing.next_display_time.checked_sub(LATCH_BUFFER)
        && latch_time < next_latch_deadline
    {
        return display_timing.next_display_time;
    }

    let Some(frame_interval) = display_timing
        .frame_interval
        .filter(|interval| !interval.is_zero())
    else {
        return latch_time;
    };

    let mut display_time = display_timing.next_display_time;
    for _ in 0..240 {
        let latch_deadline = display_time
            .checked_sub(LATCH_BUFFER)
            .unwrap_or(display_time);
        if latch_time < latch_deadline {
            return display_time;
        }
        display_time += frame_interval;
    }

    latch_time
}

fn decrement_pending_swap_count(pending_swap_count: &AtomicUsize) {
    let mut count = pending_swap_count.load(Ordering::Acquire);
    while count > 0 {
        match pending_swap_count.compare_exchange_weak(
            count,
            count - 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(next_count) => count = next_count,
        }
    }
}

fn restore_failed_buffer_damage(
    buffer_damage: &Mutex<Bounds<DevicePixels>>,
    submitted_damage: Bounds<DevicePixels>,
) {
    if submitted_damage.is_empty() {
        return;
    }

    let mut buffer_damage = buffer_damage.lock();
    *buffer_damage = buffer_damage.union(&submitted_damage);
}

fn swap_completion_result_for_iosurface_frame(presented: bool) -> SwapCompletionResult {
    if presented {
        SwapCompletionResult::Ack
    } else {
        SwapCompletionResult::Skipped
    }
}

fn iosurface_backpressure_fence_is_signaled(signaled_value: u64, fence_value: u64) -> bool {
    signaled_value >= fence_value
}

fn apply_iosurface_backpressure_fence(fence: &IosurfaceBackpressureFence) {
    while !iosurface_backpressure_fence_is_signaled(fence.event.signaled_value(), fence.value) {
        thread::sleep(Duration::from_millis(1));
    }
}

fn complete_iosurface_frame(frame: Box<PresentedIosurfaceFrame>, presented: bool) {
    let latch_time = scheduler::Instant::now();
    if let Some(callback) = &frame.swap_completion_callback {
        callback(SwapCompletionFeedback {
            ready_time: frame.ready_time,
            latch_time,
            result: swap_completion_result_for_iosurface_frame(presented),
            presented,
        });
    }
    if let Some(callback) = &frame.presentation_feedback_callback {
        if presented {
            callback(presentation_feedback_for_iosurface_frame(
                frame.ready_time,
                latch_time,
                *frame.latest_display_timing.lock(),
            ));
        } else {
            callback(PresentationFeedback {
                ready_time: frame.ready_time,
                latch_time,
                display_time: latch_time,
                interval: None,
                presented: false,
                vsync: false,
                hardware_completion: true,
            });
        }
    }
    if presented {
        frame
            .current_buffer_index
            .store(frame.buffer_index, Ordering::Release);
        *frame.committed_backpressure_fence.lock() = Some(frame.backpressure_fence.clone());
    }
    frame.buffer_pending.store(false, Ordering::Release);
    decrement_pending_swap_count(&frame.pending_swap_count);
    if frame.deferred_frame_requested.swap(false, Ordering::AcqRel)
        && let Some(callback) = &frame.deferred_frame_callback
    {
        callback();
    }
}

fn supports_ca_transaction_phase_handlers() -> bool {
    static SUPPORTS_CA_TRANSACTION_PHASE_HANDLERS: OnceLock<bool> = OnceLock::new();
    *SUPPORTS_CA_TRANSACTION_PHASE_HANDLERS.get_or_init(|| unsafe {
        msg_send![
            class!(CATransaction),
            respondsToSelector: sel!(addCommitHandler:forPhase:)
        ]
    })
}

fn fail_iosurface_frame(frame: Box<PresentedIosurfaceFrame>) {
    let latch_time = scheduler::Instant::now();
    restore_failed_buffer_damage(&frame.buffer_damage, frame.submitted_damage);
    if let Some(callback) = &frame.swap_completion_callback {
        callback(SwapCompletionFeedback {
            ready_time: frame.ready_time,
            latch_time,
            result: SwapCompletionResult::Failed,
            presented: false,
        });
    }
    if let Some(callback) = &frame.presentation_feedback_callback {
        callback(PresentationFeedback {
            ready_time: frame.ready_time,
            latch_time,
            display_time: latch_time,
            interval: None,
            presented: false,
            vsync: false,
            hardware_completion: true,
        });
    }
    frame.buffer_pending.store(false, Ordering::Release);
    decrement_pending_swap_count(&frame.pending_swap_count);
    frame
        .deferred_frame_requested
        .store(false, Ordering::Release);
    if let Some(callback) = &frame.deferred_frame_callback {
        callback();
    }
}

fn commit_iosurface_frame(frame: Box<PresentedIosurfaceFrame>) {
    if frame.generation != frame.current_generation.load(Ordering::Acquire) {
        complete_iosurface_frame(frame, false);
        return;
    }

    let frame = Rc::new(Cell::new(Some(frame)));
    let post_commit_handler = if supports_ca_transaction_phase_handlers() {
        let frame = frame.clone();
        let block = ConcreteBlock::new(move || {
            if let Some(frame) = frame.take() {
                complete_iosurface_frame(frame, true);
            }
        });
        Some(block.copy())
    } else {
        None
    };

    unsafe {
        let _: () = msg_send![class!(CATransaction), begin];
        let _: () = msg_send![class!(CATransaction), setDisableActions: YES];
        if let Some(post_commit_handler) = &post_commit_handler {
            let post_commit_handler = &**post_commit_handler;
            let _: () = msg_send![
                class!(CATransaction),
                addCommitHandler: post_commit_handler
                forPhase: CA_TRANSACTION_PHASE_POST_COMMIT
            ];
        }
        if let Some(frame_for_contents) = frame.take() {
            let contents = frame_for_contents.surface.as_concrete_TypeRef() as id;
            let previous_contents: id = msg_send![frame_for_contents.layer, contents];
            let supports_contents_changed: bool = msg_send![
                frame_for_contents.layer,
                respondsToSelector: sel!(setContentsChanged)
            ];
            if !contents.is_null() && contents == previous_contents && supports_contents_changed {
                let _: () = msg_send![frame_for_contents.layer, setContentsChanged];
            } else {
                let _: () = msg_send![frame_for_contents.layer, setContents: contents];
            }
            frame.set(Some(frame_for_contents));
        }
        let _: () = msg_send![class!(CATransaction), commit];
    }

    if let Some(frame) = frame.take() {
        complete_iosurface_frame(frame, true);
    }
}

fn should_commit_ready_iosurface_frame_immediately(
    queued_frame_count: usize,
    delay_until_next_vsync: bool,
) -> bool {
    queued_frame_count <= 1 && !delay_until_next_vsync
}

fn commit_ready_iosurface_frame(
    presented_frames: Arc<Mutex<IosurfaceSubmissionQueue<Box<PresentedIosurfaceFrame>>>>,
) -> bool {
    let frame = {
        let mut presented_frames = presented_frames.lock();
        presented_frames.pop_ready_front()
    };
    let Some(frame) = frame else {
        return false;
    };
    commit_iosurface_frame(frame);
    true
}

fn commit_all_ready_iosurface_frames(
    presented_frames: Arc<Mutex<IosurfaceSubmissionQueue<Box<PresentedIosurfaceFrame>>>>,
) {
    let ready_front_count = presented_frames.lock().ready_front_count();
    for _ in 0..ready_front_count {
        commit_ready_iosurface_frame(presented_frames.clone());
    }
}

fn commit_ready_iosurface_frame_after_completion(
    presented_frames: Arc<Mutex<IosurfaceSubmissionQueue<Box<PresentedIosurfaceFrame>>>>,
) {
    let should_commit = {
        let presented_frames = presented_frames.lock();
        let Some(frame) = presented_frames.ready_front() else {
            return;
        };
        should_commit_ready_iosurface_frame_immediately(
            presented_frames.len(),
            frame.delay_until_next_vsync,
        )
    };

    if should_commit {
        commit_ready_iosurface_frame(presented_frames);
    }
}

extern "C" fn complete_presented_iosurface_frame_async(context: *mut c_void) {
    let completion = unsafe { Box::from_raw(context as *mut IosurfaceFrameCompletion) };
    if completion.status == MTLCommandBufferStatus::Completed {
        let ready_time = scheduler::Instant::now();
        let marked_ready = {
            let mut presented_frames = completion.presented_frames.lock();
            presented_frames.mark_ready_with(completion.submission_order, |frame| {
                frame.ready_time = ready_time;
            })
        };
        if !marked_ready {
            log::error!(
                "completed IOSurface submission {} was not found in the presentation queue",
                completion.submission_order
            );
        }
        commit_ready_iosurface_frame_after_completion(completion.presented_frames.clone());
    } else {
        log::error!(
            "failed to render IOSurface frame: Metal command buffer finished with status {:?}",
            completion.status
        );
        let frame = {
            let mut presented_frames = completion.presented_frames.lock();
            presented_frames.remove(completion.submission_order)
        };
        if let Some(frame) = frame {
            fail_iosurface_frame(frame);
        } else {
            log::error!(
                "failed IOSurface submission {} was not found in the presentation queue",
                completion.submission_order
            );
        }
        commit_ready_iosurface_frame_after_completion(completion.presented_frames.clone());
    }
}

pub(crate) struct MetalRenderer {
    device: metal::Device,
    layer: Option<metal::MetalLayer>,
    core_animation_presenter: Option<CoreAnimationPresenter>,
    is_apple_gpu: bool,
    is_unified_memory: bool,
    presents_with_transaction: bool,
    swap_completion_callback: Option<Arc<dyn Fn(SwapCompletionFeedback) + Send + Sync>>,
    presentation_feedback_callback: Option<Arc<dyn Fn(PresentationFeedback) + Send + Sync>>,
    /// For headless rendering, tracks whether output should be opaque
    opaque: bool,
    command_queue: CommandQueue,
    paths_rasterization_pipeline_state: metal::RenderPipelineState,
    path_sprites_pipeline_state: metal::RenderPipelineState,
    shadows_pipeline_state: metal::RenderPipelineState,
    quads_pipeline_state: metal::RenderPipelineState,
    underlines_pipeline_state: metal::RenderPipelineState,
    monochrome_sprites_pipeline_state: metal::RenderPipelineState,
    polychrome_sprites_pipeline_state: metal::RenderPipelineState,
    surfaces_pipeline_state: metal::RenderPipelineState,
    unit_vertices: metal::Buffer,
    #[allow(clippy::arc_with_non_send_sync)]
    instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>,
    sprite_atlas: Arc<MetalAtlas>,
    core_video_texture_cache: core_video::metal_texture_cache::CVMetalTextureCache,
    path_intermediate_texture: Option<metal::Texture>,
    path_intermediate_msaa_texture: Option<metal::Texture>,
    path_sample_count: u32,
    /// Offscreen render target reused across `render_scene` calls when
    /// rendering headlessly without reading pixels back.
    #[cfg(any(test, feature = "test-support"))]
    headless_render_target: Option<metal::Texture>,
}

#[repr(C)]
pub struct PathRasterizationVertex {
    pub xy_position: Point<ScaledPixels>,
    pub st_position: Point<f32>,
    pub color: Background,
    pub bounds: Bounds<ScaledPixels>,
}

impl MetalRenderer {
    /// Creates a new MetalRenderer with a CAMetalLayer for window-based rendering.
    pub fn new(instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>, transparent: bool) -> Self {
        let device = Self::create_device();

        let layer = metal::MetalLayer::new();
        layer.set_device(&device);
        layer.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        // Support direct-to-display rendering if the window is not transparent
        // https://developer.apple.com/documentation/metal/managing-your-game-window-for-metal-in-macos
        layer.set_opaque(!transparent);
        layer.set_maximum_drawable_count(3);
        // Allow texture reading for visual tests (captures screenshots without ScreenCaptureKit)
        #[cfg(any(test, feature = "test-support"))]
        layer.set_framebuffer_only(false);
        unsafe {
            let _: () = msg_send![&*layer, setAllowsNextDrawableTimeout: NO];
            let _: () = msg_send![&*layer, setNeedsDisplayOnBoundsChange: YES];
            let _: () = msg_send![
                &*layer,
                setAutoresizingMask: AutoresizingMask::WIDTH_SIZABLE
                    | AutoresizingMask::HEIGHT_SIZABLE
            ];
        }

        Self::new_internal(device, Some(layer), !transparent, instance_buffer_pool)
    }

    pub fn new_chromium_pipeline(
        instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>,
        transparent: bool,
        initial_bounds: gpui::Size<f32>,
    ) -> Self {
        let device = Self::create_device();
        let initial_size = size(
            (initial_bounds.width.ceil() as i32).into(),
            (initial_bounds.height.ceil() as i32).into(),
        );
        let mut renderer = Self::new_internal(device, None, !transparent, instance_buffer_pool);
        match CoreAnimationPresenter::new(&renderer.device, transparent, initial_size) {
            Ok(presenter) => {
                renderer.core_animation_presenter = Some(presenter);
                log::info!("using Chromium-style CA/IOSurface renderer");
            }
            Err(error) => {
                log::error!(
                    "failed to initialize Chromium-style CA/IOSurface renderer: {error}; falling back to CAMetalLayer"
                );
                let fallback = Self::new(renderer.instance_buffer_pool.clone(), transparent);
                return fallback;
            }
        }
        renderer
    }

    /// Creates a new headless MetalRenderer for offscreen rendering without a window.
    ///
    /// This renderer can render scenes to images without requiring a CAMetalLayer,
    /// window, or AppKit. Use `render_scene_to_image()` to render scenes.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_headless(instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>) -> Self {
        let device = Self::create_device();
        Self::new_internal(device, None, true, instance_buffer_pool)
    }

    fn create_device() -> metal::Device {
        // Prefer low‐power integrated GPUs on Intel Mac. On Apple
        // Silicon, there is only ever one GPU, so this is equivalent to
        // `metal::Device::system_default()`.
        if let Some(d) = metal::Device::all()
            .into_iter()
            .min_by_key(|d| (d.is_removable(), !d.is_low_power()))
        {
            d
        } else {
            // For some reason `all()` can return an empty list, see https://github.com/zed-industries/zed/issues/37689
            // In that case, we fall back to the system default device.
            log::error!(
                "Unable to enumerate Metal devices; attempting to use system default device"
            );
            metal::Device::system_default().unwrap_or_else(|| {
                log::error!("unable to access a compatible graphics device");
                std::process::exit(1);
            })
        }
    }

    fn new_internal(
        device: metal::Device,
        layer: Option<metal::MetalLayer>,
        opaque: bool,
        instance_buffer_pool: Arc<Mutex<InstanceBufferPool>>,
    ) -> Self {
        #[cfg(feature = "runtime_shaders")]
        let library = device
            .new_library_with_source(&SHADERS_SOURCE_FILE, &metal::CompileOptions::new())
            .expect("error building metal library");
        #[cfg(not(feature = "runtime_shaders"))]
        let library = device
            .new_library_with_data(SHADERS_METALLIB)
            .expect("error building metal library");

        fn to_float2_bits(point: PointF) -> u64 {
            let mut output = point.y.to_bits() as u64;
            output <<= 32;
            output |= point.x.to_bits() as u64;
            output
        }

        // Shared memory can be used only if CPU and GPU share the same memory space.
        // https://developer.apple.com/documentation/metal/setting-resource-storage-modes
        let is_unified_memory = device.has_unified_memory();
        // Apple GPU families support memoryless textures, which can significantly reduce
        // memory usage by keeping render targets in on-chip tile memory instead of
        // allocating backing store in system memory.
        // https://developer.apple.com/documentation/metal/mtlgpufamily
        let is_apple_gpu = device.supports_family(MTLGPUFamily::Apple1);

        let unit_vertices = [
            to_float2_bits(point(0., 0.)),
            to_float2_bits(point(1., 0.)),
            to_float2_bits(point(0., 1.)),
            to_float2_bits(point(0., 1.)),
            to_float2_bits(point(1., 0.)),
            to_float2_bits(point(1., 1.)),
        ];
        let unit_vertices = device.new_buffer_with_data(
            unit_vertices.as_ptr() as *const c_void,
            mem::size_of_val(&unit_vertices) as u64,
            if is_unified_memory {
                MTLResourceOptions::StorageModeShared
                    | MTLResourceOptions::CPUCacheModeWriteCombined
            } else {
                MTLResourceOptions::StorageModeManaged
            },
        );

        let paths_rasterization_pipeline_state = build_path_rasterization_pipeline_state(
            &device,
            &library,
            "paths_rasterization",
            "path_rasterization_vertex",
            "path_rasterization_fragment",
            MTLPixelFormat::BGRA8Unorm,
            PATH_SAMPLE_COUNT,
        );
        let path_sprites_pipeline_state = build_path_sprite_pipeline_state(
            &device,
            &library,
            "path_sprites",
            "path_sprite_vertex",
            "path_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let shadows_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "shadows",
            "shadow_vertex",
            "shadow_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let quads_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "quads",
            "quad_vertex",
            "quad_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let underlines_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "underlines",
            "underline_vertex",
            "underline_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let monochrome_sprites_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "monochrome_sprites",
            "monochrome_sprite_vertex",
            "monochrome_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let polychrome_sprites_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "polychrome_sprites",
            "polychrome_sprite_vertex",
            "polychrome_sprite_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );
        let surfaces_pipeline_state = build_pipeline_state(
            &device,
            &library,
            "surfaces",
            "surface_vertex",
            "surface_fragment",
            MTLPixelFormat::BGRA8Unorm,
        );

        let command_queue = device.new_command_queue();
        let sprite_atlas = Arc::new(MetalAtlas::new(device.clone(), is_apple_gpu));
        let core_video_texture_cache =
            CVMetalTextureCache::new(None, device.clone(), None).unwrap();

        Self {
            device,
            layer,
            core_animation_presenter: None,
            presents_with_transaction: false,
            swap_completion_callback: None,
            presentation_feedback_callback: None,
            is_apple_gpu,
            is_unified_memory,
            opaque,
            command_queue,
            paths_rasterization_pipeline_state,
            path_sprites_pipeline_state,
            shadows_pipeline_state,
            quads_pipeline_state,
            underlines_pipeline_state,
            monochrome_sprites_pipeline_state,
            polychrome_sprites_pipeline_state,
            surfaces_pipeline_state,
            unit_vertices,
            instance_buffer_pool,
            sprite_atlas,
            core_video_texture_cache,
            path_intermediate_texture: None,
            path_intermediate_msaa_texture: None,
            path_sample_count: PATH_SAMPLE_COUNT,
            #[cfg(any(test, feature = "test-support"))]
            headless_render_target: None,
        }
    }

    pub fn backing_layer_ptr(&self) -> id {
        if let Some(presenter) = &self.core_animation_presenter {
            presenter.layer_ptr()
        } else {
            self.layer
                .as_ref()
                .map(|layer| layer.as_ptr() as id)
                .unwrap_or(ptr::null_mut())
        }
    }

    pub fn sprite_atlas(&self) -> &Arc<MetalAtlas> {
        &self.sprite_atlas
    }

    pub fn set_presents_with_transaction(&mut self, presents_with_transaction: bool) {
        self.presents_with_transaction = presents_with_transaction;
        if let Some(layer) = &self.layer {
            layer.set_presents_with_transaction(presents_with_transaction);
        }
    }

    pub fn set_contents_scale(&mut self, scale_factor: f64) {
        if let Some(layer) = &self.layer {
            unsafe {
                let _: () = msg_send![
                    layer.as_ref(),
                    setContentsScale: scale_factor
                ];
            }
        }
        if let Some(presenter) = &mut self.core_animation_presenter {
            presenter.set_contents_scale(scale_factor);
        }
    }

    pub fn set_deferred_frame_callback(&mut self, callback: Option<Arc<dyn Fn() + Send + Sync>>) {
        if let Some(presenter) = &mut self.core_animation_presenter {
            presenter.set_deferred_frame_callback(callback);
        }
    }

    pub fn supports_swap_completion_feedback(&self) -> bool {
        self.core_animation_presenter.is_some()
    }

    pub fn max_pending_swaps(&self) -> Option<u32> {
        self.core_animation_presenter
            .as_ref()
            .map(|_| IOSURFACE_MAX_PENDING_SWAPS as u32)
    }

    pub fn supports_delayed_begin_frame_scheduling(&self) -> bool {
        self.core_animation_presenter.is_none()
    }

    pub fn set_swap_completion_callback(
        &mut self,
        callback: Option<Arc<dyn Fn(SwapCompletionFeedback) + Send + Sync>>,
    ) {
        self.swap_completion_callback = callback.clone();
        if let Some(presenter) = &mut self.core_animation_presenter {
            presenter.set_swap_completion_callback(callback);
        }
    }

    pub fn set_presentation_feedback_callback(
        &mut self,
        callback: Option<Arc<dyn Fn(PresentationFeedback) + Send + Sync>>,
    ) {
        self.presentation_feedback_callback = callback.clone();
        if let Some(presenter) = &mut self.core_animation_presenter {
            presenter.set_presentation_feedback_callback(callback);
        }
    }

    pub fn set_display_timing(
        &mut self,
        next_display_time: scheduler::Instant,
        frame_interval: Option<Duration>,
    ) -> bool {
        if let Some(presenter) = &self.core_animation_presenter {
            presenter.set_display_timing(next_display_time, frame_interval)
        } else {
            false
        }
    }

    pub fn flush_ready_display_frames(&mut self) {
        if let Some(presenter) = &self.core_animation_presenter {
            commit_all_ready_iosurface_frames(presenter.presented_frames.clone());
        }
    }

    pub fn update_drawable_size(&mut self, size: Size<DevicePixels>) {
        if let Some(layer) = &self.layer {
            let ns_size = NSSize {
                width: size.width.0 as f64,
                height: size.height.0 as f64,
            };
            unsafe {
                let _: () = msg_send![
                    layer.as_ref(),
                    setDrawableSize: ns_size
                ];
            }
        }
        if let Some(presenter) = &mut self.core_animation_presenter
            && let Err(error) = presenter.update_drawable_size(&self.device, size)
        {
            log::error!("failed to resize CA/IOSurface buffers: {error}");
        }
        self.update_path_intermediate_textures(size);
    }

    fn update_path_intermediate_textures(&mut self, size: Size<DevicePixels>) {
        // We are uncertain when this happens, but sometimes size can be 0 here. Most likely before
        // the layout pass on window creation. Zero-sized texture creation causes SIGABRT.
        // https://github.com/zed-industries/zed/issues/36229
        if size.width.0 <= 0 || size.height.0 <= 0 {
            self.path_intermediate_texture = None;
            self.path_intermediate_msaa_texture = None;
            return;
        }

        let texture_descriptor = metal::TextureDescriptor::new();
        texture_descriptor.set_width(size.width.0 as u64);
        texture_descriptor.set_height(size.height.0 as u64);
        texture_descriptor.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
        texture_descriptor.set_storage_mode(metal::MTLStorageMode::Private);
        texture_descriptor
            .set_usage(metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead);
        self.path_intermediate_texture = Some(self.device.new_texture(&texture_descriptor));

        if self.path_sample_count > 1 {
            // https://developer.apple.com/documentation/metal/choosing-a-resource-storage-mode-for-apple-gpus
            // Rendering MSAA textures are done in a single pass, so we can use memory-less storage on Apple Silicon
            let storage_mode = if self.is_apple_gpu {
                metal::MTLStorageMode::Memoryless
            } else {
                metal::MTLStorageMode::Private
            };

            let msaa_descriptor = texture_descriptor;
            msaa_descriptor.set_texture_type(metal::MTLTextureType::D2Multisample);
            msaa_descriptor.set_storage_mode(storage_mode);
            msaa_descriptor.set_sample_count(self.path_sample_count as _);
            self.path_intermediate_msaa_texture = Some(self.device.new_texture(&msaa_descriptor));
        } else {
            self.path_intermediate_msaa_texture = None;
        }
    }

    pub fn update_transparency(&mut self, transparent: bool) {
        self.opaque = !transparent;
        if let Some(layer) = &self.layer {
            layer.set_opaque(!transparent);
        }
        if let Some(presenter) = &self.core_animation_presenter {
            presenter.set_opaque(!transparent);
        }
    }

    pub fn destroy(&self) {
        // nothing to do
    }

    pub fn draw(&mut self, scene: &Scene) -> PlatformDrawResult {
        if self.core_animation_presenter.is_some() {
            return self.draw_chromium_pipeline(scene);
        }

        let layer = match &self.layer {
            Some(l) => l.clone(),
            None => {
                log::error!(
                    "draw() called on headless renderer - use render_scene_to_image() instead"
                );
                return PlatformDrawResult::Skipped;
            }
        };
        let viewport_size = layer.drawable_size();
        let viewport_size: Size<DevicePixels> = size(
            (viewport_size.width.ceil() as i32).into(),
            (viewport_size.height.ceil() as i32).into(),
        );
        if viewport_size.width.0 <= 0 || viewport_size.height.0 <= 0 {
            return PlatformDrawResult::Skipped;
        }
        let drawable = if let Some(drawable) = layer.next_drawable() {
            drawable
        } else {
            log::error!(
                "failed to retrieve next drawable, drawable size: {:?}",
                viewport_size
            );
            return PlatformDrawResult::Deferred;
        };

        loop {
            let mut instance_buffer = self
                .instance_buffer_pool
                .lock()
                .acquire(&self.device, self.is_unified_memory);

            let command_buffer =
                self.draw_primitives(scene, &mut instance_buffer, drawable, viewport_size);

            match command_buffer {
                Ok(command_buffer) => {
                    let instance_buffer_pool = self.instance_buffer_pool.clone();
                    let instance_buffer = Cell::new(Some(instance_buffer));
                    let block = ConcreteBlock::new(move |_| {
                        if let Some(instance_buffer) = instance_buffer.take() {
                            instance_buffer_pool.lock().release(instance_buffer);
                        }
                    });
                    let block = block.copy();
                    command_buffer.add_completed_handler(&block);

                    if let Some(callback) = self.presentation_feedback_callback.clone() {
                        let block = ConcreteBlock::new(move |drawable: &metal::DrawableRef| {
                            callback(presentation_feedback_from_ca_time(
                                drawable.presented_time(),
                            ));
                        });
                        let block = block.copy();
                        drawable.add_presented_handler(&block);
                    }

                    if self.presents_with_transaction {
                        command_buffer.commit();
                        command_buffer.wait_until_scheduled();
                        drawable.present();
                    } else {
                        command_buffer.present_drawable(drawable);
                        command_buffer.commit();
                    }
                    return PlatformDrawResult::Submitted;
                }
                Err(err) => {
                    log::error!(
                        "failed to render: {}. retrying with larger instance buffer size",
                        err
                    );
                    let mut instance_buffer_pool = self.instance_buffer_pool.lock();
                    let buffer_size = instance_buffer_pool.buffer_size;
                    if buffer_size >= 256 * 1024 * 1024 {
                        log::error!("instance buffer size grew too large: {}", buffer_size);
                        break;
                    }
                    instance_buffer_pool.reset(buffer_size * 2);
                    log::info!(
                        "increased instance buffer size to {}",
                        instance_buffer_pool.buffer_size
                    );
                }
            }
        }
        PlatformDrawResult::Skipped
    }

    fn draw_chromium_pipeline(&mut self, scene: &Scene) -> PlatformDrawResult {
        let (viewport_size, buffer_index) = {
            let Some(presenter) = &self.core_animation_presenter else {
                return PlatformDrawResult::Skipped;
            };
            let viewport_size = presenter.drawable_size;
            if viewport_size.width.0 <= 0 || viewport_size.height.0 <= 0 {
                return PlatformDrawResult::Skipped;
            }

            presenter.apply_committed_backpressure();

            if presenter.pending_swap_count.load(Ordering::Acquire) >= IOSURFACE_MAX_PENDING_SWAPS {
                commit_ready_iosurface_frame(presenter.presented_frames.clone());
            }

            if scene_damage(scene, viewport_size).is_none() {
                return PlatformDrawResult::Skipped;
            }

            let Some(buffer_index) = presenter.next_buffer_index() else {
                log::debug!(
                    "skipping frame because the CA/IOSurface presenter has no available buffer"
                );
                return PlatformDrawResult::Deferred;
            };
            (viewport_size, buffer_index)
        };

        loop {
            let mut instance_buffer = self
                .instance_buffer_pool
                .lock()
                .acquire(&self.device, self.is_unified_memory);

            let command_buffer = {
                let (texture, damage) = {
                    let presenter = self
                        .core_animation_presenter
                        .as_mut()
                        .expect("checked above");
                    let frame_damage = scene_damage(scene, viewport_size).expect("checked above");
                    presenter.update_buffer_damage(frame_damage);
                    let damage = presenter.buffer_damage(buffer_index);
                    let texture = presenter.buffers[buffer_index].texture.clone();
                    (texture, damage)
                };
                self.draw_primitives_to_texture(
                    scene,
                    &mut instance_buffer,
                    &texture,
                    viewport_size,
                    Some(damage),
                )
                .map(|command_buffer| (command_buffer, damage))
            };

            match command_buffer {
                Ok((command_buffer, submitted_damage)) => {
                    let instance_buffer_pool = self.instance_buffer_pool.clone();
                    let presenter = self
                        .core_animation_presenter
                        .as_mut()
                        .expect("checked above");
                    presenter.clear_buffer_damage(buffer_index);
                    presenter.mark_buffer_pending(buffer_index);
                    presenter.prepare_to_present();
                    let submission_order = presenter.next_submission_order();
                    let backpressure_fence = presenter.next_backpressure_fence();
                    command_buffer
                        .encode_signal_event(&backpressure_fence.event, backpressure_fence.value);
                    let presented_frames = presenter.presented_frames.clone();

                    let buffer = &presenter.buffers[buffer_index];
                    let presented_frame = PresentedIosurfaceFrame {
                        layer: unsafe {
                            let retained_layer: id =
                                msg_send![presenter.content_layer_ptr(), retain];
                            retained_layer
                        },
                        surface: buffer.surface.clone(),
                        buffer_pending: buffer.pending.clone(),
                        pending_swap_count: presenter.pending_swap_count.clone(),
                        current_buffer_index: presenter.current_buffer_index.clone(),
                        current_generation: presenter.generation.clone(),
                        generation: presenter.generation.load(Ordering::Acquire),
                        deferred_frame_requested: presenter.deferred_frame_requested.clone(),
                        deferred_frame_callback: presenter.deferred_frame_callback.clone(),
                        swap_completion_callback: presenter.swap_completion_callback.clone(),
                        presentation_feedback_callback: presenter
                            .presentation_feedback_callback
                            .clone(),
                        latest_display_timing: presenter.latest_display_timing.clone(),
                        ready_time: scheduler::Instant::now(),
                        buffer_index,
                        buffer_damage: buffer.damage.clone(),
                        submitted_damage,
                        backpressure_fence,
                        committed_backpressure_fence: presenter
                            .committed_backpressure_fence
                            .clone(),
                        delay_until_next_vsync: scene.is_handling_interaction(),
                    };
                    presented_frames
                        .lock()
                        .push(submission_order, Box::new(presented_frame));
                    let instance_buffer = Cell::new(Some(instance_buffer));
                    let block =
                        ConcreteBlock::new(move |command_buffer: &metal::CommandBufferRef| {
                            if let Some(instance_buffer) = instance_buffer.take() {
                                instance_buffer_pool.lock().release(instance_buffer);
                            }

                            let completion = IosurfaceFrameCompletion {
                                presented_frames: presented_frames.clone(),
                                submission_order,
                                status: command_buffer.status(),
                            };
                            let context = Box::into_raw(Box::new(completion)) as *mut c_void;
                            unsafe {
                                dispatch2::DispatchQueue::main().exec_async_f(
                                    context,
                                    complete_presented_iosurface_frame_async,
                                );
                            }
                        });
                    let block = block.copy();
                    command_buffer.add_completed_handler(&block);
                    command_buffer.commit();
                    return PlatformDrawResult::Submitted;
                }
                Err(err) => {
                    log::error!(
                        "failed to render: {}. retrying with larger instance buffer size",
                        err
                    );
                    let mut instance_buffer_pool = self.instance_buffer_pool.lock();
                    let buffer_size = instance_buffer_pool.buffer_size;
                    if buffer_size >= 256 * 1024 * 1024 {
                        log::error!("instance buffer size grew too large: {}", buffer_size);
                        break;
                    }
                    instance_buffer_pool.reset(buffer_size * 2);
                    log::info!(
                        "increased instance buffer size to {}",
                        instance_buffer_pool.buffer_size
                    );
                }
            }
        }
        PlatformDrawResult::Skipped
    }

    /// Renders the scene to a texture and returns the pixel data as an RGBA image.
    /// This does not present the frame to screen - useful for visual testing
    /// where we want to capture what would be rendered without displaying it.
    ///
    /// Note: This requires a layer-backed renderer. For headless rendering,
    /// use `render_scene_to_image()` instead.
    #[cfg(any(test, feature = "test-support"))]
    pub fn render_to_image(&mut self, scene: &Scene) -> Result<RgbaImage> {
        let layer = self
            .layer
            .clone()
            .ok_or_else(|| anyhow::anyhow!("render_to_image requires a layer-backed renderer"))?;
        let viewport_size = layer.drawable_size();
        let viewport_size: Size<DevicePixels> = size(
            (viewport_size.width.ceil() as i32).into(),
            (viewport_size.height.ceil() as i32).into(),
        );
        let drawable = layer
            .next_drawable()
            .ok_or_else(|| anyhow::anyhow!("Failed to get drawable for render_to_image"))?;

        loop {
            let mut instance_buffer = self
                .instance_buffer_pool
                .lock()
                .acquire(&self.device, self.is_unified_memory);

            let command_buffer =
                self.draw_primitives(scene, &mut instance_buffer, drawable, viewport_size);

            match command_buffer {
                Ok(command_buffer) => {
                    let instance_buffer_pool = self.instance_buffer_pool.clone();
                    let instance_buffer = Cell::new(Some(instance_buffer));
                    let block = ConcreteBlock::new(move |_| {
                        if let Some(instance_buffer) = instance_buffer.take() {
                            instance_buffer_pool.lock().release(instance_buffer);
                        }
                    });
                    let block = block.copy();
                    command_buffer.add_completed_handler(&block);

                    // Commit and wait for completion without presenting
                    command_buffer.commit();
                    command_buffer.wait_until_completed();

                    // Read pixels from the texture
                    let texture = drawable.texture();
                    let width = texture.width() as u32;
                    let height = texture.height() as u32;
                    let bytes_per_row = width as usize * 4;
                    let buffer_size = height as usize * bytes_per_row;

                    let mut pixels = vec![0u8; buffer_size];

                    let region = metal::MTLRegion {
                        origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
                        size: metal::MTLSize {
                            width: width as u64,
                            height: height as u64,
                            depth: 1,
                        },
                    };

                    texture.get_bytes(
                        pixels.as_mut_ptr() as *mut std::ffi::c_void,
                        bytes_per_row as u64,
                        region,
                        0,
                    );

                    // Convert BGRA to RGBA (swap B and R channels)
                    for chunk in pixels.chunks_exact_mut(4) {
                        chunk.swap(0, 2);
                    }

                    return RgbaImage::from_raw(width, height, pixels).ok_or_else(|| {
                        anyhow::anyhow!("Failed to create RgbaImage from pixel data")
                    });
                }
                Err(err) => {
                    log::error!(
                        "failed to render: {}. retrying with larger instance buffer size",
                        err
                    );
                    let mut instance_buffer_pool = self.instance_buffer_pool.lock();
                    let buffer_size = instance_buffer_pool.buffer_size;
                    if buffer_size >= 256 * 1024 * 1024 {
                        anyhow::bail!("instance buffer size grew too large: {}", buffer_size);
                    }
                    instance_buffer_pool.reset(buffer_size * 2);
                    log::info!(
                        "increased instance buffer size to {}",
                        instance_buffer_pool.buffer_size
                    );
                }
            }
        }
    }

    /// Renders a scene to an image without requiring a window or CAMetalLayer.
    ///
    /// This is the primary method for headless rendering. It creates an offscreen
    /// texture, renders the scene to it, and returns the pixel data as an RGBA image.
    #[cfg(any(test, feature = "test-support"))]
    pub fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> Result<RgbaImage> {
        if size.width.0 <= 0 || size.height.0 <= 0 {
            anyhow::bail!("Invalid size for render_scene_to_image: {:?}", size);
        }

        // Update path intermediate textures for this size
        self.update_path_intermediate_textures(size);

        // Create an offscreen texture as render target
        let texture_descriptor = metal::TextureDescriptor::new();
        texture_descriptor.set_width(size.width.0 as u64);
        texture_descriptor.set_height(size.height.0 as u64);
        texture_descriptor.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
        texture_descriptor
            .set_usage(metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead);
        texture_descriptor.set_storage_mode(metal::MTLStorageMode::Managed);
        let target_texture = self.device.new_texture(&texture_descriptor);

        loop {
            let mut instance_buffer = self
                .instance_buffer_pool
                .lock()
                .acquire(&self.device, self.is_unified_memory);

            let command_buffer = self.draw_primitives_to_texture(
                scene,
                &mut instance_buffer,
                &target_texture,
                size,
                None,
            );

            match command_buffer {
                Ok(command_buffer) => {
                    let instance_buffer_pool = self.instance_buffer_pool.clone();
                    let instance_buffer = Cell::new(Some(instance_buffer));
                    let block = ConcreteBlock::new(move |_| {
                        if let Some(instance_buffer) = instance_buffer.take() {
                            instance_buffer_pool.lock().release(instance_buffer);
                        }
                    });
                    let block = block.copy();
                    command_buffer.add_completed_handler(&block);

                    // On discrete GPUs (non-unified memory), Managed textures
                    // require an explicit blit synchronize before the CPU can
                    // read back the rendered data. Without this, get_bytes
                    // returns stale zeros.
                    if !self.is_unified_memory {
                        let blit = command_buffer.new_blit_command_encoder();
                        blit.synchronize_resource(&target_texture);
                        blit.end_encoding();
                    }

                    // Commit and wait for completion
                    command_buffer.commit();
                    command_buffer.wait_until_completed();

                    // Read pixels from the texture
                    let width = size.width.0 as u32;
                    let height = size.height.0 as u32;
                    let bytes_per_row = width as usize * 4;
                    let buffer_size = height as usize * bytes_per_row;

                    let mut pixels = vec![0u8; buffer_size];

                    let region = metal::MTLRegion {
                        origin: metal::MTLOrigin { x: 0, y: 0, z: 0 },
                        size: metal::MTLSize {
                            width: width as u64,
                            height: height as u64,
                            depth: 1,
                        },
                    };

                    target_texture.get_bytes(
                        pixels.as_mut_ptr() as *mut std::ffi::c_void,
                        bytes_per_row as u64,
                        region,
                        0,
                    );

                    // Convert BGRA to RGBA (swap B and R channels)
                    for chunk in pixels.chunks_exact_mut(4) {
                        chunk.swap(0, 2);
                    }

                    return RgbaImage::from_raw(width, height, pixels).ok_or_else(|| {
                        anyhow::anyhow!("Failed to create RgbaImage from pixel data")
                    });
                }
                Err(err) => {
                    log::error!(
                        "failed to render: {}. retrying with larger instance buffer size",
                        err
                    );
                    let mut instance_buffer_pool = self.instance_buffer_pool.lock();
                    let buffer_size = instance_buffer_pool.buffer_size;
                    if buffer_size >= 256 * 1024 * 1024 {
                        anyhow::bail!("instance buffer size grew too large: {}", buffer_size);
                    }
                    instance_buffer_pool.reset(buffer_size * 2);
                    log::info!(
                        "increased instance buffer size to {}",
                        instance_buffer_pool.buffer_size
                    );
                }
            }
        }
    }

    /// Renders a scene to a reused offscreen texture without reading pixels
    /// back or blocking on GPU completion.
    ///
    /// This mirrors the CPU cost of presenting a frame to a window (scene
    /// encoding, instance buffer writes, command submission) and is used by
    /// headless benchmark rendering, where the produced pixels are never
    /// inspected.
    #[cfg(any(test, feature = "test-support"))]
    pub fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> Result<()> {
        if size.width.0 <= 0 || size.height.0 <= 0 {
            anyhow::bail!("Invalid size for render_scene: {:?}", size);
        }

        self.update_path_intermediate_textures(size);

        let needs_new_target = self.headless_render_target.as_ref().is_none_or(|texture| {
            texture.width() != size.width.0 as u64 || texture.height() != size.height.0 as u64
        });
        if needs_new_target {
            let texture_descriptor = metal::TextureDescriptor::new();
            texture_descriptor.set_width(size.width.0 as u64);
            texture_descriptor.set_height(size.height.0 as u64);
            texture_descriptor.set_pixel_format(MTLPixelFormat::BGRA8Unorm);
            texture_descriptor.set_usage(
                metal::MTLTextureUsage::RenderTarget | metal::MTLTextureUsage::ShaderRead,
            );
            texture_descriptor.set_storage_mode(metal::MTLStorageMode::Private);
            self.headless_render_target = Some(self.device.new_texture(&texture_descriptor));
        }
        let target_texture = self
            .headless_render_target
            .clone()
            .expect("just ensured the render target exists");

        loop {
            let mut instance_buffer = self
                .instance_buffer_pool
                .lock()
                .acquire(&self.device, self.is_unified_memory);

            let command_buffer = self.draw_primitives_to_texture(
                scene,
                &mut instance_buffer,
                &target_texture,
                size,
                None,
            );

            match command_buffer {
                Ok(command_buffer) => {
                    let instance_buffer_pool = self.instance_buffer_pool.clone();
                    let instance_buffer = Cell::new(Some(instance_buffer));
                    let block = ConcreteBlock::new(move |_| {
                        if let Some(instance_buffer) = instance_buffer.take() {
                            instance_buffer_pool.lock().release(instance_buffer);
                        }
                    });
                    let block = block.copy();
                    command_buffer.add_completed_handler(&block);

                    // Commit without waiting, mirroring presentation to a real
                    // window where the CPU doesn't block on the GPU.
                    command_buffer.commit();
                    return Ok(());
                }
                Err(err) => {
                    log::error!(
                        "failed to render: {}. retrying with larger instance buffer size",
                        err
                    );
                    let mut instance_buffer_pool = self.instance_buffer_pool.lock();
                    let buffer_size = instance_buffer_pool.buffer_size;
                    if buffer_size >= 256 * 1024 * 1024 {
                        anyhow::bail!("instance buffer size grew too large: {}", buffer_size);
                    }
                    instance_buffer_pool.reset(buffer_size * 2);
                    log::info!(
                        "increased instance buffer size to {}",
                        instance_buffer_pool.buffer_size
                    );
                }
            }
        }
    }

    fn draw_primitives(
        &mut self,
        scene: &Scene,
        instance_buffer: &mut InstanceBuffer,
        drawable: &metal::MetalDrawableRef,
        viewport_size: Size<DevicePixels>,
    ) -> Result<metal::CommandBuffer> {
        self.draw_primitives_to_texture(
            scene,
            instance_buffer,
            drawable.texture(),
            viewport_size,
            None,
        )
    }

    fn draw_primitives_to_texture(
        &mut self,
        scene: &Scene,
        instance_buffer: &mut InstanceBuffer,
        texture: &metal::TextureRef,
        viewport_size: Size<DevicePixels>,
        damage: Option<Bounds<DevicePixels>>,
    ) -> Result<metal::CommandBuffer> {
        let command_queue = self.command_queue.clone();
        let command_buffer = command_queue.new_command_buffer();
        let alpha = if self.opaque { 1. } else { 0. };
        let mut instance_offset = 0;
        let full_damage = full_texture_bounds(viewport_size);
        let damage = damage
            .map(|damage| clamp_damage(damage, viewport_size))
            .filter(|damage| !damage.is_empty())
            .unwrap_or(full_damage);
        let full_redraw = damage == full_damage;

        let mut command_encoder = new_command_encoder_for_texture(
            command_buffer,
            texture,
            viewport_size,
            |color_attachment| {
                if full_redraw {
                    color_attachment.set_load_action(metal::MTLLoadAction::Clear);
                    color_attachment.set_clear_color(metal::MTLClearColor::new(0., 0., 0., alpha));
                } else {
                    color_attachment.set_load_action(metal::MTLLoadAction::Load);
                }
            },
        );
        set_scissor_for_damage(command_encoder, damage);

        for batch in scene.batches() {
            let ok = match batch {
                PrimitiveBatch::Shadows(range) => self.draw_shadows(
                    &scene.shadows[range],
                    instance_buffer,
                    &mut instance_offset,
                    viewport_size,
                    command_encoder,
                ),
                PrimitiveBatch::Quads(range) => self.draw_quads(
                    &scene.quads[range],
                    instance_buffer,
                    &mut instance_offset,
                    viewport_size,
                    command_encoder,
                ),
                PrimitiveBatch::Paths(range) => {
                    let paths = &scene.paths[range];
                    command_encoder.end_encoding();

                    let did_draw = self.draw_paths_to_intermediate(
                        paths,
                        instance_buffer,
                        &mut instance_offset,
                        viewport_size,
                        command_buffer,
                    );

                    command_encoder = new_command_encoder_for_texture(
                        command_buffer,
                        texture,
                        viewport_size,
                        |color_attachment| {
                            color_attachment.set_load_action(metal::MTLLoadAction::Load);
                        },
                    );
                    set_scissor_for_damage(command_encoder, damage);

                    if did_draw {
                        self.draw_paths_from_intermediate(
                            paths,
                            instance_buffer,
                            &mut instance_offset,
                            viewport_size,
                            command_encoder,
                        )
                    } else {
                        false
                    }
                }
                PrimitiveBatch::Underlines(range) => self.draw_underlines(
                    &scene.underlines[range],
                    instance_buffer,
                    &mut instance_offset,
                    viewport_size,
                    command_encoder,
                ),
                PrimitiveBatch::MonochromeSprites { texture_id, range } => self
                    .draw_monochrome_sprites(
                        texture_id,
                        &scene.monochrome_sprites[range],
                        instance_buffer,
                        &mut instance_offset,
                        viewport_size,
                        command_encoder,
                    ),
                PrimitiveBatch::PolychromeSprites { texture_id, range } => self
                    .draw_polychrome_sprites(
                        texture_id,
                        &scene.polychrome_sprites[range],
                        instance_buffer,
                        &mut instance_offset,
                        viewport_size,
                        command_encoder,
                    ),
                PrimitiveBatch::Surfaces(range) => self.draw_surfaces(
                    &scene.surfaces[range],
                    instance_buffer,
                    &mut instance_offset,
                    viewport_size,
                    command_encoder,
                ),
                PrimitiveBatch::SubpixelSprites { .. } => unreachable!(),
            };
            if !ok {
                command_encoder.end_encoding();
                anyhow::bail!(
                    "scene too large: {} paths, {} shadows, {} quads, {} underlines, {} mono, {} poly, {} surfaces",
                    scene.paths.len(),
                    scene.shadows.len(),
                    scene.quads.len(),
                    scene.underlines.len(),
                    scene.monochrome_sprites.len(),
                    scene.polychrome_sprites.len(),
                    scene.surfaces.len(),
                );
            }
        }

        command_encoder.end_encoding();

        if !self.is_unified_memory {
            // Sync the instance buffer to the GPU
            instance_buffer.metal_buffer.did_modify_range(NSRange {
                location: 0,
                length: instance_offset as NSUInteger,
            });
        }

        Ok(command_buffer.to_owned())
    }

    fn draw_paths_to_intermediate(
        &self,
        paths: &[Path<ScaledPixels>],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_buffer: &metal::CommandBufferRef,
    ) -> bool {
        if paths.is_empty() {
            return true;
        }
        let Some(intermediate_texture) = &self.path_intermediate_texture else {
            return false;
        };

        let render_pass_descriptor = metal::RenderPassDescriptor::new();
        let color_attachment = render_pass_descriptor
            .color_attachments()
            .object_at(0)
            .unwrap();
        color_attachment.set_load_action(metal::MTLLoadAction::Clear);
        color_attachment.set_clear_color(metal::MTLClearColor::new(0., 0., 0., 0.));

        if let Some(msaa_texture) = &self.path_intermediate_msaa_texture {
            color_attachment.set_texture(Some(msaa_texture));
            color_attachment.set_resolve_texture(Some(intermediate_texture));
            color_attachment.set_store_action(metal::MTLStoreAction::MultisampleResolve);
        } else {
            color_attachment.set_texture(Some(intermediate_texture));
            color_attachment.set_store_action(metal::MTLStoreAction::Store);
        }

        let command_encoder = command_buffer.new_render_command_encoder(render_pass_descriptor);
        command_encoder.set_render_pipeline_state(&self.paths_rasterization_pipeline_state);

        align_offset(instance_offset);
        let mut vertices = Vec::new();
        for path in paths {
            vertices.extend(path.vertices.iter().map(|v| PathRasterizationVertex {
                xy_position: v.xy_position,
                st_position: v.st_position,
                color: path.color,
                bounds: path.bounds.intersect(&path.content_mask.bounds),
            }));
        }
        let vertices_bytes_len = mem::size_of_val(vertices.as_slice());
        let next_offset = *instance_offset + vertices_bytes_len;
        if next_offset > instance_buffer.size {
            command_encoder.end_encoding();
            return false;
        }
        command_encoder.set_vertex_buffer(
            PathRasterizationInputIndex::Vertices as u64,
            Some(&instance_buffer.metal_buffer),
            *instance_offset as u64,
        );
        command_encoder.set_vertex_bytes(
            PathRasterizationInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_buffer(
            PathRasterizationInputIndex::Vertices as u64,
            Some(&instance_buffer.metal_buffer),
            *instance_offset as u64,
        );
        let buffer_contents =
            unsafe { (instance_buffer.metal_buffer.contents() as *mut u8).add(*instance_offset) };
        unsafe {
            ptr::copy_nonoverlapping(
                vertices.as_ptr() as *const u8,
                buffer_contents,
                vertices_bytes_len,
            );
        }
        command_encoder.draw_primitives(
            metal::MTLPrimitiveType::Triangle,
            0,
            vertices.len() as u64,
        );
        *instance_offset = next_offset;

        command_encoder.end_encoding();
        true
    }

    fn draw_shadows(
        &self,
        shadows: &[Shadow],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) -> bool {
        if shadows.is_empty() {
            return true;
        }
        align_offset(instance_offset);

        command_encoder.set_render_pipeline_state(&self.shadows_pipeline_state);
        command_encoder.set_vertex_buffer(
            ShadowInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            ShadowInputIndex::Shadows as u64,
            Some(&instance_buffer.metal_buffer),
            *instance_offset as u64,
        );
        command_encoder.set_fragment_buffer(
            ShadowInputIndex::Shadows as u64,
            Some(&instance_buffer.metal_buffer),
            *instance_offset as u64,
        );

        command_encoder.set_vertex_bytes(
            ShadowInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        let shadow_bytes_len = mem::size_of_val(shadows);
        let buffer_contents =
            unsafe { (instance_buffer.metal_buffer.contents() as *mut u8).add(*instance_offset) };

        let next_offset = *instance_offset + shadow_bytes_len;
        if next_offset > instance_buffer.size {
            return false;
        }

        unsafe {
            ptr::copy_nonoverlapping(
                shadows.as_ptr() as *const u8,
                buffer_contents,
                shadow_bytes_len,
            );
        }

        command_encoder.draw_primitives_instanced(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            shadows.len() as u64,
        );
        *instance_offset = next_offset;
        true
    }

    fn draw_quads(
        &self,
        quads: &[Quad],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) -> bool {
        if quads.is_empty() {
            return true;
        }
        align_offset(instance_offset);

        command_encoder.set_render_pipeline_state(&self.quads_pipeline_state);
        command_encoder.set_vertex_buffer(
            QuadInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            QuadInputIndex::Quads as u64,
            Some(&instance_buffer.metal_buffer),
            *instance_offset as u64,
        );
        command_encoder.set_fragment_buffer(
            QuadInputIndex::Quads as u64,
            Some(&instance_buffer.metal_buffer),
            *instance_offset as u64,
        );

        command_encoder.set_vertex_bytes(
            QuadInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        let quad_bytes_len = mem::size_of_val(quads);
        let buffer_contents =
            unsafe { (instance_buffer.metal_buffer.contents() as *mut u8).add(*instance_offset) };

        let next_offset = *instance_offset + quad_bytes_len;
        if next_offset > instance_buffer.size {
            return false;
        }

        unsafe {
            ptr::copy_nonoverlapping(quads.as_ptr() as *const u8, buffer_contents, quad_bytes_len);
        }

        command_encoder.draw_primitives_instanced(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            quads.len() as u64,
        );
        *instance_offset = next_offset;
        true
    }

    fn draw_paths_from_intermediate(
        &self,
        paths: &[Path<ScaledPixels>],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) -> bool {
        let Some(first_path) = paths.first() else {
            return true;
        };

        let Some(ref intermediate_texture) = self.path_intermediate_texture else {
            return false;
        };

        command_encoder.set_render_pipeline_state(&self.path_sprites_pipeline_state);
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        command_encoder.set_fragment_texture(
            SpriteInputIndex::AtlasTexture as u64,
            Some(intermediate_texture),
        );

        // When copying paths from the intermediate texture to the drawable,
        // each pixel must only be copied once, in case of transparent paths.
        //
        // If all paths have the same draw order, then their bounds are all
        // disjoint, so we can copy each path's bounds individually. If this
        // batch combines different draw orders, we perform a single copy
        // for a minimal spanning rect.
        let sprites;
        if paths.last().unwrap().order == first_path.order {
            sprites = paths
                .iter()
                .map(|path| PathSprite {
                    bounds: path.clipped_bounds(),
                })
                .collect();
        } else {
            let mut bounds = first_path.clipped_bounds();
            for path in paths.iter().skip(1) {
                bounds = bounds.union(&path.clipped_bounds());
            }
            sprites = vec![PathSprite { bounds }];
        }

        align_offset(instance_offset);
        let sprite_bytes_len = mem::size_of_val(sprites.as_slice());
        let next_offset = *instance_offset + sprite_bytes_len;
        if next_offset > instance_buffer.size {
            return false;
        }

        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_buffer.metal_buffer),
            *instance_offset as u64,
        );

        let buffer_contents =
            unsafe { (instance_buffer.metal_buffer.contents() as *mut u8).add(*instance_offset) };
        unsafe {
            ptr::copy_nonoverlapping(
                sprites.as_ptr() as *const u8,
                buffer_contents,
                sprite_bytes_len,
            );
        }

        command_encoder.draw_primitives_instanced(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            sprites.len() as u64,
        );
        *instance_offset = next_offset;

        true
    }

    fn draw_underlines(
        &self,
        underlines: &[Underline],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) -> bool {
        if underlines.is_empty() {
            return true;
        }
        align_offset(instance_offset);

        command_encoder.set_render_pipeline_state(&self.underlines_pipeline_state);
        command_encoder.set_vertex_buffer(
            UnderlineInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            UnderlineInputIndex::Underlines as u64,
            Some(&instance_buffer.metal_buffer),
            *instance_offset as u64,
        );
        command_encoder.set_fragment_buffer(
            UnderlineInputIndex::Underlines as u64,
            Some(&instance_buffer.metal_buffer),
            *instance_offset as u64,
        );

        command_encoder.set_vertex_bytes(
            UnderlineInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        let underline_bytes_len = mem::size_of_val(underlines);
        let buffer_contents =
            unsafe { (instance_buffer.metal_buffer.contents() as *mut u8).add(*instance_offset) };

        let next_offset = *instance_offset + underline_bytes_len;
        if next_offset > instance_buffer.size {
            return false;
        }

        unsafe {
            ptr::copy_nonoverlapping(
                underlines.as_ptr() as *const u8,
                buffer_contents,
                underline_bytes_len,
            );
        }

        command_encoder.draw_primitives_instanced(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            underlines.len() as u64,
        );
        *instance_offset = next_offset;
        true
    }

    fn draw_monochrome_sprites(
        &self,
        texture_id: AtlasTextureId,
        sprites: &[MonochromeSprite],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) -> bool {
        if sprites.is_empty() {
            return true;
        }
        align_offset(instance_offset);

        let sprite_bytes_len = mem::size_of_val(sprites);
        let buffer_contents =
            unsafe { (instance_buffer.metal_buffer.contents() as *mut u8).add(*instance_offset) };

        let next_offset = *instance_offset + sprite_bytes_len;
        if next_offset > instance_buffer.size {
            return false;
        }

        let texture = self.sprite_atlas.metal_texture(texture_id);
        let texture_size = size(
            DevicePixels(texture.width() as i32),
            DevicePixels(texture.height() as i32),
        );
        command_encoder.set_render_pipeline_state(&self.monochrome_sprites_pipeline_state);
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_buffer.metal_buffer),
            *instance_offset as u64,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::AtlasTextureSize as u64,
            mem::size_of_val(&texture_size) as u64,
            &texture_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_buffer.metal_buffer),
            *instance_offset as u64,
        );
        command_encoder.set_fragment_texture(SpriteInputIndex::AtlasTexture as u64, Some(&texture));

        unsafe {
            ptr::copy_nonoverlapping(
                sprites.as_ptr() as *const u8,
                buffer_contents,
                sprite_bytes_len,
            );
        }

        command_encoder.draw_primitives_instanced(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            sprites.len() as u64,
        );
        *instance_offset = next_offset;
        true
    }

    fn draw_polychrome_sprites(
        &self,
        texture_id: AtlasTextureId,
        sprites: &[PolychromeSprite],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) -> bool {
        if sprites.is_empty() {
            return true;
        }
        align_offset(instance_offset);

        let texture = self.sprite_atlas.metal_texture(texture_id);
        let texture_size = size(
            DevicePixels(texture.width() as i32),
            DevicePixels(texture.height() as i32),
        );
        command_encoder.set_render_pipeline_state(&self.polychrome_sprites_pipeline_state);
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_buffer.metal_buffer),
            *instance_offset as u64,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_vertex_bytes(
            SpriteInputIndex::AtlasTextureSize as u64,
            mem::size_of_val(&texture_size) as u64,
            &texture_size as *const Size<DevicePixels> as *const _,
        );
        command_encoder.set_fragment_buffer(
            SpriteInputIndex::Sprites as u64,
            Some(&instance_buffer.metal_buffer),
            *instance_offset as u64,
        );
        command_encoder.set_fragment_texture(SpriteInputIndex::AtlasTexture as u64, Some(&texture));

        let sprite_bytes_len = mem::size_of_val(sprites);
        let buffer_contents =
            unsafe { (instance_buffer.metal_buffer.contents() as *mut u8).add(*instance_offset) };

        let next_offset = *instance_offset + sprite_bytes_len;
        if next_offset > instance_buffer.size {
            return false;
        }

        unsafe {
            ptr::copy_nonoverlapping(
                sprites.as_ptr() as *const u8,
                buffer_contents,
                sprite_bytes_len,
            );
        }

        command_encoder.draw_primitives_instanced(
            metal::MTLPrimitiveType::Triangle,
            0,
            6,
            sprites.len() as u64,
        );
        *instance_offset = next_offset;
        true
    }

    fn draw_surfaces(
        &mut self,
        surfaces: &[PaintSurface],
        instance_buffer: &mut InstanceBuffer,
        instance_offset: &mut usize,
        viewport_size: Size<DevicePixels>,
        command_encoder: &metal::RenderCommandEncoderRef,
    ) -> bool {
        command_encoder.set_render_pipeline_state(&self.surfaces_pipeline_state);
        command_encoder.set_vertex_buffer(
            SurfaceInputIndex::Vertices as u64,
            Some(&self.unit_vertices),
            0,
        );
        command_encoder.set_vertex_bytes(
            SurfaceInputIndex::ViewportSize as u64,
            mem::size_of_val(&viewport_size) as u64,
            &viewport_size as *const Size<DevicePixels> as *const _,
        );

        for surface in surfaces {
            let texture_size = size(
                DevicePixels::from(surface.image_buffer.get_width() as i32),
                DevicePixels::from(surface.image_buffer.get_height() as i32),
            );

            assert_eq!(
                surface.image_buffer.get_pixel_format(),
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
            );

            let y_texture = self
                .core_video_texture_cache
                .create_texture_from_image(
                    surface.image_buffer.as_concrete_TypeRef(),
                    None,
                    MTLPixelFormat::R8Unorm,
                    surface.image_buffer.get_width_of_plane(0),
                    surface.image_buffer.get_height_of_plane(0),
                    0,
                )
                .unwrap();
            let cb_cr_texture = self
                .core_video_texture_cache
                .create_texture_from_image(
                    surface.image_buffer.as_concrete_TypeRef(),
                    None,
                    MTLPixelFormat::RG8Unorm,
                    surface.image_buffer.get_width_of_plane(1),
                    surface.image_buffer.get_height_of_plane(1),
                    1,
                )
                .unwrap();

            align_offset(instance_offset);
            let next_offset = *instance_offset + mem::size_of::<Surface>();
            if next_offset > instance_buffer.size {
                return false;
            }

            command_encoder.set_vertex_buffer(
                SurfaceInputIndex::Surfaces as u64,
                Some(&instance_buffer.metal_buffer),
                *instance_offset as u64,
            );
            command_encoder.set_vertex_bytes(
                SurfaceInputIndex::TextureSize as u64,
                mem::size_of_val(&texture_size) as u64,
                &texture_size as *const Size<DevicePixels> as *const _,
            );
            // let y_texture = y_texture.get_texture().unwrap().
            command_encoder.set_fragment_texture(SurfaceInputIndex::YTexture as u64, unsafe {
                let texture = CVMetalTextureGetTexture(y_texture.as_concrete_TypeRef());
                Some(metal::TextureRef::from_ptr(texture as *mut _))
            });
            command_encoder.set_fragment_texture(SurfaceInputIndex::CbCrTexture as u64, unsafe {
                let texture = CVMetalTextureGetTexture(cb_cr_texture.as_concrete_TypeRef());
                Some(metal::TextureRef::from_ptr(texture as *mut _))
            });

            unsafe {
                let buffer_contents = (instance_buffer.metal_buffer.contents() as *mut u8)
                    .add(*instance_offset)
                    as *mut SurfaceBounds;
                ptr::write(
                    buffer_contents,
                    SurfaceBounds {
                        bounds: surface.bounds,
                        content_mask: surface.content_mask,
                    },
                );
            }

            command_encoder.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 6);
            *instance_offset = next_offset;
        }
        true
    }
}

fn new_command_encoder_for_texture<'a>(
    command_buffer: &'a metal::CommandBufferRef,
    texture: &'a metal::TextureRef,
    viewport_size: Size<DevicePixels>,
    configure_color_attachment: impl Fn(&RenderPassColorAttachmentDescriptorRef),
) -> &'a metal::RenderCommandEncoderRef {
    let render_pass_descriptor = metal::RenderPassDescriptor::new();
    let color_attachment = render_pass_descriptor
        .color_attachments()
        .object_at(0)
        .unwrap();
    color_attachment.set_texture(Some(texture));
    color_attachment.set_store_action(metal::MTLStoreAction::Store);
    configure_color_attachment(color_attachment);

    let command_encoder = command_buffer.new_render_command_encoder(render_pass_descriptor);
    command_encoder.set_viewport(metal::MTLViewport {
        originX: 0.0,
        originY: 0.0,
        width: i32::from(viewport_size.width) as f64,
        height: i32::from(viewport_size.height) as f64,
        znear: 0.0,
        zfar: 1.0,
    });
    command_encoder
}

fn full_texture_bounds(size: Size<DevicePixels>) -> Bounds<DevicePixels> {
    bounds(point(DevicePixels(0), DevicePixels(0)), size)
}

fn scene_damage(scene: &Scene, viewport_size: Size<DevicePixels>) -> Option<Bounds<DevicePixels>> {
    scene
        .damage()
        .map(scaled_damage_to_device_bounds)
        .map(|damage| clamp_damage(damage, viewport_size))
        .filter(|damage| !damage.is_empty())
}

fn scaled_damage_to_device_bounds(damage: Bounds<ScaledPixels>) -> Bounds<DevicePixels> {
    let min_x = damage.origin.x.0.floor() as i32;
    let min_y = damage.origin.y.0.floor() as i32;
    let max_x = (damage.origin.x.0 + damage.size.width.0).ceil() as i32;
    let max_y = (damage.origin.y.0 + damage.size.height.0).ceil() as i32;

    bounds(
        point(DevicePixels(min_x), DevicePixels(min_y)),
        size(
            DevicePixels((max_x - min_x).max(0)),
            DevicePixels((max_y - min_y).max(0)),
        ),
    )
}

fn clamp_damage(
    damage: Bounds<DevicePixels>,
    viewport_size: Size<DevicePixels>,
) -> Bounds<DevicePixels> {
    damage.intersect(&full_texture_bounds(viewport_size))
}

fn set_scissor_for_damage(
    command_encoder: &metal::RenderCommandEncoderRef,
    damage: Bounds<DevicePixels>,
) {
    command_encoder.set_scissor_rect(MTLScissorRect {
        x: damage.origin.x.0.max(0) as u64,
        y: damage.origin.y.0.max(0) as u64,
        width: damage.size.width.0.max(0) as u64,
        height: damage.size.height.0.max(0) as u64,
    });
}

fn build_pipeline_state(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(true);
    color_attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::SourceAlpha);
    color_attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::One);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

fn build_path_sprite_pipeline_state(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(true);
    color_attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::One);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

fn build_path_rasterization_pipeline_state(
    device: &metal::DeviceRef,
    library: &metal::LibraryRef,
    label: &str,
    vertex_fn_name: &str,
    fragment_fn_name: &str,
    pixel_format: metal::MTLPixelFormat,
    path_sample_count: u32,
) -> metal::RenderPipelineState {
    let vertex_fn = library
        .get_function(vertex_fn_name, None)
        .expect("error locating vertex function");
    let fragment_fn = library
        .get_function(fragment_fn_name, None)
        .expect("error locating fragment function");

    let descriptor = metal::RenderPipelineDescriptor::new();
    descriptor.set_label(label);
    descriptor.set_vertex_function(Some(vertex_fn.as_ref()));
    descriptor.set_fragment_function(Some(fragment_fn.as_ref()));
    if path_sample_count > 1 {
        descriptor.set_raster_sample_count(path_sample_count as _);
        descriptor.set_alpha_to_coverage_enabled(false);
    }
    let color_attachment = descriptor.color_attachments().object_at(0).unwrap();
    color_attachment.set_pixel_format(pixel_format);
    color_attachment.set_blending_enabled(true);
    color_attachment.set_rgb_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_alpha_blend_operation(metal::MTLBlendOperation::Add);
    color_attachment.set_source_rgb_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_source_alpha_blend_factor(metal::MTLBlendFactor::One);
    color_attachment.set_destination_rgb_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);
    color_attachment.set_destination_alpha_blend_factor(metal::MTLBlendFactor::OneMinusSourceAlpha);

    device
        .new_render_pipeline_state(&descriptor)
        .expect("could not create render pipeline state")
}

// Align to multiples of 256 make Metal happy.
fn align_offset(offset: &mut usize) {
    *offset = (*offset).div_ceil(256) * 256;
}

#[repr(C)]
enum ShadowInputIndex {
    Vertices = 0,
    Shadows = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum QuadInputIndex {
    Vertices = 0,
    Quads = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum UnderlineInputIndex {
    Vertices = 0,
    Underlines = 1,
    ViewportSize = 2,
}

#[repr(C)]
enum SpriteInputIndex {
    Vertices = 0,
    Sprites = 1,
    ViewportSize = 2,
    AtlasTextureSize = 3,
    AtlasTexture = 4,
}

#[repr(C)]
enum SurfaceInputIndex {
    Vertices = 0,
    Surfaces = 1,
    ViewportSize = 2,
    TextureSize = 3,
    YTexture = 4,
    CbCrTexture = 5,
}

#[repr(C)]
enum PathRasterizationInputIndex {
    Vertices = 0,
    ViewportSize = 1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct PathSprite {
    pub bounds: Bounds<ScaledPixels>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct SurfaceBounds {
    pub bounds: Bounds<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
}

#[cfg(any(test, feature = "test-support"))]
pub struct MetalHeadlessRenderer {
    renderer: MetalRenderer,
}

#[cfg(any(test, feature = "test-support"))]
impl MetalHeadlessRenderer {
    pub fn new() -> Self {
        let instance_buffer_pool = Arc::new(Mutex::new(InstanceBufferPool::default()));
        let renderer = MetalRenderer::new_headless(instance_buffer_pool);
        Self { renderer }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl gpui::PlatformHeadlessRenderer for MetalHeadlessRenderer {
    fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> anyhow::Result<image::RgbaImage> {
        self.renderer.render_scene_to_image(scene, size)
    }

    fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> anyhow::Result<()> {
        self.renderer.render_scene(scene, size)
    }

    fn sprite_atlas(&self) -> Arc<dyn gpui::PlatformAtlas> {
        self.renderer.sprite_atlas().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_damage_to_device_bounds_encloses_fractional_damage() {
        let damage = bounds(
            point(ScaledPixels(1.25), ScaledPixels(2.75)),
            size(ScaledPixels(3.5), ScaledPixels(4.125)),
        );

        assert_eq!(
            scaled_damage_to_device_bounds(damage),
            bounds(
                point(DevicePixels(1), DevicePixels(2)),
                size(DevicePixels(4), DevicePixels(5))
            )
        );
    }

    #[test]
    fn scene_damage_preserves_empty_damage() {
        let scene = Scene::default();

        assert_eq!(
            scene_damage(&scene, size(DevicePixels(100), DevicePixels(50))),
            None
        );
    }

    #[test]
    fn decrement_pending_swap_count_saturates_at_zero() {
        let pending_swap_count = AtomicUsize::new(0);
        decrement_pending_swap_count(&pending_swap_count);
        assert_eq!(pending_swap_count.load(Ordering::Acquire), 0);

        pending_swap_count.store(2, Ordering::Release);
        decrement_pending_swap_count(&pending_swap_count);
        assert_eq!(pending_swap_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn restore_failed_buffer_damage_unions_submitted_damage() {
        let buffer_damage = Mutex::new(bounds(
            point(DevicePixels(10), DevicePixels(10)),
            size(DevicePixels(5), DevicePixels(5)),
        ));
        let submitted_damage = bounds(
            point(DevicePixels(20), DevicePixels(12)),
            size(DevicePixels(4), DevicePixels(6)),
        );

        restore_failed_buffer_damage(&buffer_damage, submitted_damage);

        assert_eq!(
            *buffer_damage.lock(),
            bounds(
                point(DevicePixels(10), DevicePixels(10)),
                size(DevicePixels(14), DevicePixels(8))
            )
        );
    }

    #[test]
    fn restore_failed_buffer_damage_ignores_empty_submitted_damage() {
        let original_damage = bounds(
            point(DevicePixels(10), DevicePixels(10)),
            size(DevicePixels(5), DevicePixels(5)),
        );
        let buffer_damage = Mutex::new(original_damage);

        restore_failed_buffer_damage(&buffer_damage, Bounds::default());

        assert_eq!(*buffer_damage.lock(), original_damage);
    }

    #[test]
    fn swap_completion_result_tracks_iosurface_commit_result() {
        assert_eq!(
            swap_completion_result_for_iosurface_frame(true),
            SwapCompletionResult::Ack
        );
        assert_eq!(
            swap_completion_result_for_iosurface_frame(false),
            SwapCompletionResult::Skipped
        );
    }

    #[test]
    fn iosurface_commit_cadence_delays_when_queue_has_backlog() {
        assert!(should_commit_ready_iosurface_frame_immediately(0, false));
        assert!(should_commit_ready_iosurface_frame_immediately(1, false));
        assert!(!should_commit_ready_iosurface_frame_immediately(2, false));
    }

    #[test]
    fn iosurface_commit_cadence_delays_interaction_frames_until_vsync() {
        assert!(!should_commit_ready_iosurface_frame_immediately(0, true));
        assert!(!should_commit_ready_iosurface_frame_immediately(1, true));
        assert!(!should_commit_ready_iosurface_frame_immediately(2, true));
    }

    #[test]
    fn iosurface_backpressure_fence_uses_monotonic_signal_values() {
        assert!(!iosurface_backpressure_fence_is_signaled(1, 2));
        assert!(iosurface_backpressure_fence_is_signaled(2, 2));
        assert!(iosurface_backpressure_fence_is_signaled(3, 2));
    }

    #[test]
    fn iosurface_submission_queue_waits_for_ready_front() {
        let mut queue = IosurfaceSubmissionQueue::default();
        queue.push(1, "first");
        queue.push(2, "second");

        assert!(queue.mark_ready_with(2, |_| {}));
        assert_eq!(queue.pop_ready_front(), None);

        assert!(queue.mark_ready_with(1, |_| {}));
        assert_eq!(queue.pop_ready_front(), Some("first"));
        assert_eq!(queue.pop_ready_front(), Some("second"));
    }

    #[test]
    fn iosurface_submission_queue_removing_failed_front_unblocks_ready_frame() {
        let mut queue = IosurfaceSubmissionQueue::default();
        queue.push(1, "first");
        queue.push(2, "second");

        assert!(queue.mark_ready_with(2, |_| {}));
        assert_eq!(queue.remove(1), Some("first"));
        assert_eq!(queue.pop_ready_front(), Some("second"));
    }

    #[test]
    fn iosurface_submission_queue_counts_contiguous_ready_front() {
        let mut queue = IosurfaceSubmissionQueue::default();
        queue.push(1, "first");
        queue.push(2, "second");
        queue.push(3, "third");

        assert!(queue.mark_ready_with(1, |_| {}));
        assert!(queue.mark_ready_with(3, |_| {}));
        assert_eq!(queue.ready_front_count(), 1);

        assert!(queue.mark_ready_with(2, |_| {}));
        assert_eq!(queue.ready_front_count(), 3);
    }
}
