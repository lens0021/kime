use gdk_sys::{GdkEventKey, GdkWindow, GDK_KEY_RELEASE};
use glib_sys::{g_malloc0, g_strcmp0, g_strdup, gboolean, gpointer, GType, GFALSE, GTRUE};
use gobject_sys::{
    g_object_new, g_object_ref, g_object_unref, g_signal_emit, g_signal_lookup,
    g_type_check_class_cast, g_type_check_instance_cast, g_type_module_register_type,
    g_type_module_use, g_type_register_static, GObject, GObjectClass, GTypeClass, GTypeInfo,
    GTypeInstance, GTypeModule, G_TYPE_OBJECT,
};
use gtk_sys::{gtk_im_context_get_type, GtkIMContext, GtkIMContextClass, GtkIMContextInfo};
use once_cell::sync::OnceCell;
use pango_sys::PangoAttrList;
use std::mem::size_of;
use std::os::raw::{c_char, c_int};
use std::ptr::{self, NonNull};
use std::{convert::TryFrom, os::raw::c_uint};

use kime_engine::{InputEngine, InputResult, Layout};

#[repr(transparent)]
struct TypeInfoWrapper(GTypeInfo);

unsafe impl Send for TypeInfoWrapper {}
unsafe impl Sync for TypeInfoWrapper {}

#[repr(transparent)]
struct ContextInfoWrapper(GtkIMContextInfo);

unsafe impl Send for ContextInfoWrapper {}
unsafe impl Sync for ContextInfoWrapper {}

macro_rules! cs {
    ($text:expr) => {
        concat!($text, "\0").as_ptr().cast::<c_char>()
    };
}

struct KimeIMSignals {
    commit: c_uint,
    preedit_changed: c_uint,
}

impl KimeIMSignals {
    pub unsafe fn new(class: *mut KimeIMContextClass) -> Self {
        let ty = type_of_class(class.cast());

        macro_rules! sig {
            ($($name:ident),+) => {
                Self { $($name: g_signal_lookup(cs!(stringify!($name)), ty),)+ }
            }
        }

        sig!(commit, preedit_changed)
    }
}

static SIGNALS: OnceCell<KimeIMSignals> = OnceCell::new();

#[repr(C)]
struct KimeIMContextClass {
    _parent: GtkIMContextClass,
}

#[repr(C)]
struct KimeIMContext {
    parent: GtkIMContext,
    client_window: Option<NonNull<GdkWindow>>,
    engine: InputEngine,
    preedit_str: String,
}

impl KimeIMContext {
    pub fn as_obj(&mut self) -> *mut GObject {
        &mut self.parent.parent_instance
    }

    pub fn filter_keypress(&mut self, key: &GdkEventKey) -> bool {
        // Release key
        if key.type_ == GDK_KEY_RELEASE {
            return false;
        }

        if let Ok(code) = u8::try_from(key.hardware_keycode) {
            let ret = self
                .engine
                .key_press(code, key.state & 0x1 != 0, key.state & 0x4 != 0);

            log(&format!("{:?}", ret));

            match ret {
                InputResult::CommitBypass(c) => {
                    self.commit(c);
                    true
                }
                InputResult::CommitPreedit(c, p) => {
                    self.commit(c);
                    self.preedit(p);
                    true
                }
                InputResult::Preedit(p) => {
                    self.preedit(p);
                    true
                }
                // TODO: send end preedit
                InputResult::ClearPreedit => {
                    self.clear_preedit();
                    true
                }
                InputResult::Bypass => false,
                InputResult::Consume => true,
            }
        } else {
            false
        }
    }

    pub fn reset(&mut self) {
        match self.engine.reset() {
            Some(c) => {
                self.clear_preedit();
                self.commit(c);
            }
            _ => {}
        }
    }

    pub fn preedit(&mut self, c: char) {
        self.preedit_str.clear();
        self.preedit_str.push(c);
        unsafe {
            g_signal_emit(
                self.as_obj(),
                SIGNALS.get().unwrap().preedit_changed,
                0,
                self.preedit_str.as_ptr(),
            );
        }
    }

    pub fn clear_preedit(&mut self) {
        log("clear preedit");
        if !self.preedit_str.is_empty() {
            self.preedit_str.clear();
            unsafe {
                g_signal_emit(self.as_obj(), SIGNALS.get().unwrap().preedit_changed, 0);
            }
        }
    }

    pub fn commit(&mut self, c: char) {
        self.clear_preedit();

        let mut buf = [0; 8];
        c.encode_utf8(&mut buf);
        unsafe {
            g_signal_emit(
                self.as_obj(),
                SIGNALS.get().unwrap().commit,
                0,
                buf.as_ptr(),
            );
        }
    }
}

static KIME_TYPE_IM_CONTEXT: OnceCell<GType> = OnceCell::new();

unsafe fn type_of_class(class: *mut GTypeClass) -> GType {
    (*class).g_type
}

unsafe fn register_module(module: *mut GTypeModule) {
    unsafe extern "C" fn im_context_class_init(class: gpointer, _data: gpointer) {
        log("class init");

        let class = class.cast::<KimeIMContextClass>();

        let im_context_class = g_type_check_class_cast(class.cast(), gtk_im_context_get_type())
            .cast::<GtkIMContextClass>()
            .as_mut()
            .unwrap();
        let gobject_class =
            g_type_check_class_cast(class.cast(), G_TYPE_OBJECT).cast::<GObjectClass>();

        im_context_class.set_client_window = Some(set_client_window);
        im_context_class.filter_keypress = Some(filter_keypress);
        im_context_class.reset = Some(reset_im);
        im_context_class.get_preedit_string = Some(get_preedit_string);
        im_context_class.focus_in = None;
        im_context_class.focus_out = None;
        im_context_class.set_cursor_location = None;
        im_context_class.set_use_preedit = None;
        im_context_class.set_surrounding = None;

        SIGNALS.get_or_init(|| KimeIMSignals::new(class));

        (*gobject_class).finalize = Some(im_context_instance_finalize);
    }

    unsafe extern "C" fn reset_im(ctx: *mut GtkIMContext) {
        log("reset IME");
        let ctx = ctx.cast::<KimeIMContext>().as_mut().unwrap();
        ctx.reset();
    }

    unsafe extern "C" fn filter_keypress(
        ctx: *mut GtkIMContext,
        key: *mut GdkEventKey,
    ) -> gboolean {
        let ctx = ctx.cast::<KimeIMContext>().as_mut().unwrap();
        let key = key.as_mut().unwrap();

        if ctx.filter_keypress(key) {
            GTRUE
        } else {
            GFALSE
        }
    }

    unsafe extern "C" fn get_preedit_string(
        ctx: *mut GtkIMContext,
        out: *mut *mut c_char,
        attrs: *mut *mut PangoAttrList,
        cursor_pos: *mut c_int,
    ) {
        log("get preedit string");

        let ctx = ctx.cast::<KimeIMContext>().as_mut().unwrap();
        let mut str_len = 0;

        if !out.is_null() {
            match ctx.engine.preedit_char() {
                Some(ch) => {
                    log(&format!("preedit {}", ch));
                    str_len = ch.len_utf8();
                    let s = g_malloc0(str_len + 1).cast::<c_char>();
                    ch.encode_utf8(std::slice::from_raw_parts_mut(s.cast(), str_len));
                    s.add(str_len).write(0);
                    out.write(s);
                }
                None => {
                    out.write(g_strdup(cs!("")));
                }
            }
        }

        if !attrs.is_null() {
            attrs.write(pango_sys::pango_attr_list_new());

            if !out.is_null() {
                let attr = pango_sys::pango_attr_underline_new(pango_sys::PANGO_UNDERLINE_SINGLE);
                (*attr).start_index = 0;
                (*attr).end_index = str_len as _;
                pango_sys::pango_attr_list_insert(attrs.read(), attr);
            }
        }

        if !cursor_pos.is_null() {
            cursor_pos.write(1);
        }
    }

    unsafe extern "C" fn set_client_window(ctx: *mut GtkIMContext, window: *mut GdkWindow) {
        log(&format!("Set client window: {:p}", window));

        let ctx = ctx.cast::<KimeIMContext>().as_mut().unwrap();
        let window = NonNull::new(window);

        if let Some(prev_win) = ctx.client_window {
            g_object_unref(prev_win.as_ptr().cast());
        }

        if let Some(win) = window {
            g_object_ref(win.as_ptr().cast());
        }

        ctx.client_window = window;
    }

    unsafe extern "C" fn im_context_class_finalize(class: gpointer, _data: gpointer) {
        let _class = class.cast::<KimeIMContextClass>();
    }

    unsafe extern "C" fn im_context_instance_init(ctx: *mut GTypeInstance, _class: gpointer) {
        log("instance init");

        // Context is uninitalized so all fields must be initialize in this code except parent
        let ctx = ctx.cast::<KimeIMContext>();

        (*ctx).client_window = None;
        (*ctx).engine = InputEngine::new(Layout::dubeolsik());
        (*ctx).preedit_str = String::with_capacity(12);
    }

    unsafe extern "C" fn im_context_instance_finalize(ctx: *mut GObject) {
        let ctx = ctx.cast::<KimeIMContext>();
        ctx.drop_in_place();
    }

    static INFO: TypeInfoWrapper = TypeInfoWrapper(GTypeInfo {
        class_size: size_of::<KimeIMContextClass>() as _,
        base_init: None,
        base_finalize: None,
        class_init: Some(im_context_class_init),
        class_finalize: Some(im_context_class_finalize),
        class_data: ptr::null(),
        instance_size: size_of::<KimeIMContext>() as _,
        n_preallocs: 0,
        instance_init: Some(im_context_instance_init),
        value_table: ptr::null(),
    });

    KIME_TYPE_IM_CONTEXT.get_or_init(|| {
        if module.is_null() {
            g_type_register_static(gtk_im_context_get_type(), cs!("KimeImContext"), &INFO.0, 0)
        } else {
            g_type_module_register_type(
                module,
                gtk_im_context_get_type(),
                cs!("KimeIMContext"),
                &INFO.0,
                0,
            )
        }
    });
}

fn log(t: &str) {
    let s = std::ffi::CString::new(t).unwrap();
    unsafe {
        glib_sys::g_log(cs!("kime"), glib_sys::G_LOG_LEVEL_WARNING, s.as_ptr());
    }
}

#[no_mangle]
pub unsafe extern "C" fn im_module_init(module: *mut GTypeModule) {
    log("module init");
    g_type_module_use(module);
    register_module(module);
}

#[no_mangle]
pub unsafe extern "C" fn im_module_exit() {
    log("module exit");
}

#[no_mangle]
pub unsafe extern "C" fn im_module_list(
    contexts: *mut *const *const GtkIMContextInfo,
    n_contexts: *mut c_int,
) {
    log("module list");
    static INFO: ContextInfoWrapper = ContextInfoWrapper(GtkIMContextInfo {
        context_id: cs!("kime"),
        context_name: cs!("Kime (Korean IME)"),
        domain: cs!("kime"),
        domain_dirname: cs!("/usr/share/locale"),
        default_locales: cs!("ko:*"),
    });

    static INFOS: &[&ContextInfoWrapper] = &[&INFO];

    // SAFETY: *const &ContextInfoWrapper -> *const *const GtkIMContextInfo
    // & == *const, ContextInfoWrapper == GtkIMContextInfo(transparent)
    contexts.write(INFOS.as_ptr().cast());
    n_contexts.write(INFOS.len() as _);
}

#[no_mangle]
pub unsafe extern "C" fn im_module_create(
    context_id: *const c_char,
) -> Option<ptr::NonNull<GtkIMContext>> {
    log("module create");
    if !context_id.is_null() && g_strcmp0(context_id, cs!("kime")) == 0 {
        let ty = *KIME_TYPE_IM_CONTEXT.get()?;
        let obj = g_object_new(ty, ptr::null());
        ptr::NonNull::new(g_type_check_instance_cast(obj.cast(), ty).cast())
    } else {
        None
    }
}
