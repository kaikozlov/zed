use cocoa::{
    base::{NO, YES, id, nil},
    foundation::{NSPoint, NSRect, NSSize},
    quartzcore::AutoresizingMask,
};
use gpui::{DevicePixels, Size};
use mach2::{
    kern_return::KERN_SUCCESS,
    mach_port::mach_port_deallocate,
    port::{MACH_PORT_NULL, mach_port_t},
    traps::mach_task_self,
};
use objc::{class, msg_send, runtime::Class, sel, sel_impl};
use std::collections::VecDeque;

const MAX_CA_CONTEXT_FENCE_PORTS: usize = 4;

pub(crate) struct CoreAnimationLayerTree {
    backing_layer: id,
    content_layer: id,
    ca_context: id,
    ca_context_fence_ports: VecDeque<mach_port_t>,
    uses_ca_context: bool,
    contents_scale: f64,
}

impl CoreAnimationLayerTree {
    pub(crate) fn new(transparent: bool, initial_size: Size<DevicePixels>) -> Self {
        let tree = Self::new_remote(transparent).unwrap_or_else(|| Self::new_direct(transparent));
        tree.set_drawable_size(initial_size);
        tree
    }

    pub(crate) fn backing_layer(&self) -> id {
        self.backing_layer
    }

    pub(crate) fn content_layer(&self) -> id {
        self.content_layer
    }

    pub(crate) fn uses_ca_context(&self) -> bool {
        self.uses_ca_context
    }

    pub(crate) fn recreate_ca_context(&mut self) -> bool {
        if !self.uses_ca_context {
            return false;
        }

        let Some(ca_context) = Self::create_ca_context(self.content_layer) else {
            return false;
        };

        unsafe {
            self.create_and_set_fence_port();
            let _: () = msg_send![self.ca_context, setLayer: nil];
            let _: () = msg_send![self.ca_context, release];
            let context_id: u32 = msg_send![ca_context, contextId];
            let _: () = msg_send![self.backing_layer, setContextId: context_id];
        }
        self.ca_context = ca_context;
        true
    }

    pub(crate) fn set_contents_scale(&mut self, scale_factor: f64) {
        self.contents_scale = scale_factor.max(1.0);
        unsafe {
            let _: () = msg_send![self.backing_layer, setContentsScale: self.contents_scale];
            let _: () = msg_send![self.content_layer, setContentsScale: self.contents_scale];
        }
    }

    pub(crate) fn set_opaque(&self, opaque: bool) {
        unsafe {
            let _: () = msg_send![self.backing_layer, setOpaque: if opaque { YES } else { NO }];
            let _: () = msg_send![self.content_layer, setOpaque: if opaque { YES } else { NO }];
        }
    }

    pub(crate) fn set_drawable_size(&self, drawable_size: Size<DevicePixels>) {
        let width = f64::from(drawable_size.width.0.max(0)) / self.contents_scale;
        let height = f64::from(drawable_size.height.0.max(0)) / self.contents_scale;
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height));

        unsafe {
            let _: () = msg_send![self.content_layer, setBounds: bounds];
            if self.uses_ca_context {
                let _: () = msg_send![self.backing_layer, setBounds: bounds];
            }
        }
    }

    fn create_and_set_fence_port(&mut self) -> bool {
        if self.ca_context == nil {
            return false;
        }

        let supports_create_fence_port: bool =
            unsafe { msg_send![self.ca_context, respondsToSelector: sel!(createFencePort)] };
        let supports_set_fence_port: bool =
            unsafe { msg_send![self.ca_context, respondsToSelector: sel!(setFencePort:)] };
        if !supports_create_fence_port || !supports_set_fence_port {
            return false;
        }

        let fence_port: mach_port_t = unsafe { msg_send![self.ca_context, createFencePort] };
        if fence_port == MACH_PORT_NULL {
            return false;
        }

        unsafe {
            let _: () = msg_send![self.ca_context, setFencePort: fence_port];
        }
        self.ca_context_fence_ports.push_back(fence_port);
        while self.ca_context_fence_ports.len() > MAX_CA_CONTEXT_FENCE_PORTS {
            if let Some(fence_port) = self.ca_context_fence_ports.pop_front() {
                deallocate_mach_port(fence_port);
            }
        }
        true
    }

    fn new_remote(transparent: bool) -> Option<Self> {
        let ca_context_class = Class::get("CAContext")?;
        let ca_layer_host_class = Class::get("CALayerHost")?;
        let supports_ca_context: bool = unsafe {
            msg_send![
                ca_context_class,
                respondsToSelector: sel!(contextWithCGSConnection:options:)
            ]
        };
        let supports_layer_host: bool = unsafe {
            msg_send![
                ca_layer_host_class,
                instancesRespondToSelector: sel!(setContextId:)
            ]
        };
        if !supports_ca_context || !supports_layer_host {
            return None;
        }

        let backing_layer: id = unsafe { msg_send![ca_layer_host_class, new] };
        let content_layer: id = unsafe { msg_send![class!(CALayer), new] };
        let Some(ca_context) = Self::create_ca_context(content_layer) else {
            unsafe {
                let _: () = msg_send![backing_layer, release];
                let _: () = msg_send![content_layer, release];
            }
            return None;
        };

        unsafe {
            configure_layer(backing_layer, transparent);
            configure_layer(content_layer, transparent);
            configure_hosted_layer_geometry(backing_layer);
            configure_hosted_layer_geometry(content_layer);
            let _: () = msg_send![content_layer, setGeometryFlipped: YES];
            let context_id: u32 = msg_send![ca_context, contextId];
            let _: () = msg_send![backing_layer, setContextId: context_id];
        }

        Some(Self {
            backing_layer,
            content_layer,
            ca_context,
            ca_context_fence_ports: VecDeque::new(),
            uses_ca_context: true,
            contents_scale: 1.0,
        })
    }

    fn create_ca_context(content_layer: id) -> Option<id> {
        let ca_context_class = Class::get("CAContext")?;
        unsafe {
            let options: id = msg_send![class!(NSDictionary), dictionary];
            let context: id = msg_send![
                ca_context_class,
                contextWithCGSConnection: CGSMainConnectionID()
                options: options
            ];
            if context == nil {
                return None;
            }
            let context: id = msg_send![context, retain];
            let _: () = msg_send![context, setLayer: content_layer];
            Some(context)
        }
    }

    fn new_direct(transparent: bool) -> Self {
        let layer: id = unsafe { msg_send![class!(CALayer), new] };
        unsafe {
            configure_layer(layer, transparent);
        }
        Self {
            backing_layer: layer,
            content_layer: layer,
            ca_context: nil,
            ca_context_fence_ports: VecDeque::new(),
            uses_ca_context: false,
            contents_scale: 1.0,
        }
    }
}

impl Drop for CoreAnimationLayerTree {
    fn drop(&mut self) {
        unsafe {
            if self.ca_context != nil {
                let _: () = msg_send![self.ca_context, setLayer: nil];
                let _: () = msg_send![self.ca_context, release];
            }
            if self.content_layer != self.backing_layer {
                let _: () = msg_send![self.content_layer, release];
            }
            let _: () = msg_send![self.backing_layer, release];
        }
        for fence_port in self.ca_context_fence_ports.drain(..) {
            deallocate_mach_port(fence_port);
        }
    }
}

fn deallocate_mach_port(port: mach_port_t) {
    if port == MACH_PORT_NULL {
        return;
    }

    let result = unsafe { mach_port_deallocate(mach_task_self(), port) };
    if result != KERN_SUCCESS {
        log::warn!("failed to deallocate CAContext fence mach port: {result}");
    }
}

unsafe fn configure_layer(layer: id, transparent: bool) {
    unsafe {
        let _: () = msg_send![layer, setOpaque: if transparent { NO } else { YES }];
        let _: () = msg_send![layer, setNeedsDisplayOnBoundsChange: YES];
        let _: () = msg_send![
            layer,
            setAutoresizingMask: AutoresizingMask::WIDTH_SIZABLE
                | AutoresizingMask::HEIGHT_SIZABLE
        ];
    }
}

unsafe fn configure_hosted_layer_geometry(layer: id) {
    unsafe {
        let _: () = msg_send![layer, setAnchorPoint: NSPoint::new(0.0, 0.0)];
        let _: () = msg_send![layer, setPosition: NSPoint::new(0.0, 0.0)];
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSMainConnectionID() -> u32;
}
