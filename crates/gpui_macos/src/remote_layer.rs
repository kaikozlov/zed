use crate::ns_string;
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
use std::{collections::VecDeque, ffi::c_char, sync::OnceLock};

const MAX_CA_CONTEXT_FENCE_PORTS: usize = 4;

pub(crate) struct CoreAnimationLayerTree {
    backing_layer: id,
    host_layer: id,
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

        let Some(ca_context) = Self::create_ca_context() else {
            return false;
        };

        unsafe {
            with_disabled_ca_actions(|| {
                self.create_and_set_fence_port();
                let _: () = msg_send![self.ca_context, setLayer: nil];
                let _: () = msg_send![self.ca_context, release];
                let _: () = msg_send![ca_context, setLayer: self.content_layer];
                let context_id: u32 = msg_send![ca_context, contextId];
                self.replace_host_layer(context_id);
            });
        }
        self.ca_context = ca_context;
        true
    }

    pub(crate) fn set_contents_scale(&mut self, scale_factor: f64) {
        self.contents_scale = scale_factor.max(1.0);
        unsafe {
            with_disabled_ca_actions(|| {
                let _: () = msg_send![self.backing_layer, setContentsScale: self.contents_scale];
                if self.host_layer != nil {
                    let _: () = msg_send![self.host_layer, setContentsScale: self.contents_scale];
                }
                let _: () = msg_send![self.content_layer, setContentsScale: self.contents_scale];
            });
        }
    }

    pub(crate) fn set_opaque(&self, opaque: bool) {
        unsafe {
            with_disabled_ca_actions(|| {
                let _: () = msg_send![self.backing_layer, setOpaque: if opaque { YES } else { NO }];
                if self.host_layer != nil {
                    let _: () =
                        msg_send![self.host_layer, setOpaque: if opaque { YES } else { NO }];
                }
                let _: () = msg_send![self.content_layer, setOpaque: if opaque { YES } else { NO }];
            });
        }
    }

    pub(crate) fn set_drawable_size(&self, drawable_size: Size<DevicePixels>) {
        let width = f64::from(drawable_size.width.0.max(0)) / self.contents_scale;
        let height = f64::from(drawable_size.height.0.max(0)) / self.contents_scale;
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height));

        unsafe {
            with_disabled_ca_actions(|| {
                let _: () = msg_send![self.backing_layer, setBounds: bounds];
                let _: () = msg_send![self.content_layer, setBounds: bounds];
                if self.host_layer != nil {
                    let _: () = msg_send![self.host_layer, setBounds: bounds];
                }
            });
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
        if !remote_layer_api_supported() {
            return None;
        }

        let backing_layer: id = unsafe { msg_send![class!(CALayer), new] };
        let content_layer: id = unsafe { msg_send![class!(CALayer), new] };
        let Some(ca_context) = Self::create_ca_context() else {
            unsafe {
                let _: () = msg_send![backing_layer, release];
                let _: () = msg_send![content_layer, release];
            }
            return None;
        };

        unsafe {
            with_disabled_ca_actions(|| {
                configure_layer(backing_layer, transparent);
                configure_layer(content_layer, transparent);
                configure_iosurface_content_layer(content_layer);
                configure_hosted_layer_geometry(backing_layer);
                configure_hosted_layer_geometry(content_layer);
                let _: () = msg_send![backing_layer, setGeometryFlipped: YES];
                let _: () = msg_send![content_layer, setGeometryFlipped: YES];
                let _: () = msg_send![ca_context, setLayer: content_layer];
            });
        }

        let mut tree = Self {
            backing_layer,
            host_layer: nil,
            content_layer,
            ca_context,
            ca_context_fence_ports: VecDeque::new(),
            uses_ca_context: true,
            contents_scale: 1.0,
        };
        let context_id: u32 = unsafe { msg_send![tree.ca_context, contextId] };
        unsafe {
            with_disabled_ca_actions(|| {
                tree.replace_host_layer(context_id);
            });
        }
        Some(tree)
    }

    fn create_ca_context() -> Option<id> {
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
            Some(context)
        }
    }

    fn new_direct(transparent: bool) -> Self {
        let backing_layer: id = unsafe { msg_send![class!(CALayer), new] };
        let content_layer: id = unsafe { msg_send![class!(CALayer), new] };
        unsafe {
            with_disabled_ca_actions(|| {
                configure_layer(backing_layer, transparent);
                configure_layer(content_layer, transparent);
                configure_iosurface_content_layer(content_layer);
                configure_hosted_layer_geometry(backing_layer);
                configure_hosted_layer_geometry(content_layer);
                let _: () = msg_send![backing_layer, setGeometryFlipped: YES];
                let _: () = msg_send![backing_layer, addSublayer: content_layer];
            });
        }
        Self {
            backing_layer,
            host_layer: nil,
            content_layer,
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
            with_disabled_ca_actions(|| {
                if self.ca_context != nil {
                    let _: () = msg_send![self.ca_context, setLayer: nil];
                    let _: () = msg_send![self.ca_context, release];
                }
                if self.host_layer != nil {
                    let _: () = msg_send![self.host_layer, removeFromSuperlayer];
                    let _: () = msg_send![self.host_layer, release];
                }
                if self.content_layer != self.backing_layer {
                    let _: () = msg_send![self.content_layer, release];
                }
                let _: () = msg_send![self.backing_layer, release];
            });
        }
        for fence_port in self.ca_context_fence_ports.drain(..) {
            deallocate_mach_port(fence_port);
        }
    }
}

impl CoreAnimationLayerTree {
    unsafe fn replace_host_layer(&mut self, context_id: u32) {
        unsafe {
            let ca_layer_host_class = Class::get("CALayerHost").expect("checked by support gate");
            let host_layer: id = msg_send![ca_layer_host_class, new];
            configure_layer(host_layer, !self.is_opaque());
            configure_host_layer(host_layer);
            let bounds: NSRect = msg_send![self.backing_layer, bounds];
            let _: () = msg_send![host_layer, setBounds: bounds];
            let _: () = msg_send![host_layer, setContentsScale: self.contents_scale];
            let _: () = msg_send![host_layer, setContextId: context_id];
            let _: () = msg_send![self.backing_layer, addSublayer: host_layer];
            if self.host_layer != nil {
                let _: () = msg_send![self.host_layer, removeFromSuperlayer];
                let _: () = msg_send![self.host_layer, release];
            }
            self.host_layer = host_layer;
        }
    }

    unsafe fn is_opaque(&self) -> bool {
        unsafe { msg_send![self.backing_layer, isOpaque] }
    }
}

fn remote_layer_api_supported() -> bool {
    static REMOTE_LAYER_API_SUPPORTED: OnceLock<bool> = OnceLock::new();
    *REMOTE_LAYER_API_SUPPORTED.get_or_init(|| {
        let Some(ca_context_class) = Class::get("CAContext") else {
            return false;
        };
        let supports_ca_context: bool = unsafe {
            msg_send![
                ca_context_class,
                respondsToSelector: sel!(contextWithCGSConnection:options:)
            ]
        };
        if !supports_ca_context
            || !class_has_property(ca_context_class, b"contextId\0")
            || !class_has_property(ca_context_class, b"layer\0")
        {
            return false;
        }

        let Some(ca_layer_host_class) = Class::get("CALayerHost") else {
            return false;
        };
        let supports_context_id: bool = unsafe {
            msg_send![
                ca_layer_host_class,
                instancesRespondToSelector: sel!(contextId)
            ]
        };
        let supports_set_context_id: bool = unsafe {
            msg_send![
                ca_layer_host_class,
                instancesRespondToSelector: sel!(setContextId:)
            ]
        };
        supports_context_id && supports_set_context_id
    })
}

fn class_has_property(class: &Class, name: &'static [u8]) -> bool {
    let name = name.as_ptr().cast::<c_char>();
    unsafe { !class_getProperty(class, name).is_null() }
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

unsafe fn with_disabled_ca_actions<R>(operation: impl FnOnce() -> R) -> R {
    unsafe {
        let _: () = msg_send![class!(CATransaction), begin];
        let _: () = msg_send![class!(CATransaction), setDisableActions: YES];
        let result = operation();
        let _: () = msg_send![class!(CATransaction), commit];
        result
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

unsafe fn configure_iosurface_content_layer(layer: id) {
    unsafe {
        let _: () = msg_send![layer, setContentsGravity: ns_string("topLeft")];
        let nearest_filter = ns_string("nearest");
        let _: () = msg_send![layer, setMinificationFilter: nearest_filter];
        let _: () = msg_send![layer, setMagnificationFilter: nearest_filter];
    }
}

unsafe fn configure_host_layer(layer: id) {
    unsafe {
        configure_hosted_layer_geometry(layer);
        let _: () = msg_send![
            layer,
            setAutoresizingMask: AutoresizingMask::MAX_X_MARGIN | AutoresizingMask::MAX_Y_MARGIN
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

#[link(name = "objc", kind = "dylib")]
unsafe extern "C" {
    fn class_getProperty(class: *const Class, name: *const c_char) -> *const libc::c_void;
}
