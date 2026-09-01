//! The zolo shared allocator: ONE process-wide heap for the host binary and
//! every dynamically-loaded plugin.
//!
//! ## Why
//!
//! The plugin ABI transfers ownership of heap allocations across the DLL
//! boundary in both directions: a plugin's `register()` hands the host owned
//! `String`s/`Vec`s, and every plugin call returns owned `PluginValue`s the
//! host later drops. That is only sound if the memory is freed by the *same*
//! allocator that allocated it. Two separately statically-linked allocators do
//! NOT share a heap — freeing a block on the wrong heap corrupts it (on
//! Windows: `0xC0000005` the moment the host drops a plugin-allocated value).
//!
//! ## How
//!
//! Exactly one allocator instance lives **inside the host executable** (the
//! `host` feature half of this crate), which exports the four `zolo_rt_*`
//! symbols from its dynamic symbol table (link args emitted by the
//! `zolo-alloc-exports` build helper). Plugin DLLs install [`ZoloAlloc`], a
//! shim that resolves those symbols from the executable on first allocation —
//! `GetProcAddress(GetModuleHandleW(NULL))` on Windows, `dlsym(RTLD_DEFAULT)`
//! elsewhere. This is safe even for allocations in the plugin's static
//! initializers: by the time `LoadLibrary`/`dlopen` runs plugin code, the
//! executable is fully loaded and its exports are resolvable.
//!
//! If the process does NOT export the symbols the shim **aborts** with a raw
//! stderr message. It must not fall back to another allocator: a fallback heap
//! would be a *different* heap and reintroduce the exact corruption this crate
//! exists to prevent. Hosts get a friendlier failure earlier: the plugin
//! loader pre-flights [`host_allocator_exported`] before `dlopen`.
//!
//! ## Backend selection (build time, `host` feature)
//!
//! mimalloc is the implicit default on every OS; `backend-jemalloc`,
//! `backend-snmalloc`, `backend-system` override it (enable at most one). The
//! exported ABI carries `size`/`align` on free and realloc precisely so any
//! `GlobalAlloc` backend works — mimalloc ignores them, System/snmalloc need
//! them. A/B benchmarking rebuilds the host with a feature flag (see
//! `benchmark/allocators/`).

use core::alloc::{GlobalAlloc, Layout};

// ─── Host half: the single backend instance + exported C ABI ────────────────

#[cfg(feature = "host")]
mod host {
    use super::*;

    #[cfg(not(any(
        feature = "backend-jemalloc",
        feature = "backend-snmalloc",
        feature = "backend-system"
    )))]
    const BACKEND: mimalloc::MiMalloc = mimalloc::MiMalloc;

    #[cfg(feature = "backend-jemalloc")]
    const BACKEND: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

    #[cfg(feature = "backend-snmalloc")]
    const BACKEND: snmalloc_rs::SnMalloc = snmalloc_rs::SnMalloc;

    #[cfg(feature = "backend-system")]
    const BACKEND: std::alloc::System = std::alloc::System;

    // At most one explicit backend. (Two explicit backends also fail to
    // compile via the duplicate `BACKEND` definition; this just gives a
    // friendlier message.)
    #[cfg(any(
        all(feature = "backend-jemalloc", feature = "backend-snmalloc"),
        all(feature = "backend-jemalloc", feature = "backend-system"),
        all(feature = "backend-snmalloc", feature = "backend-system"),
    ))]
    compile_error!("zolo-alloc: enable at most one backend-* feature");

    #[inline]
    fn layout(size: usize, align: usize) -> Layout {
        // SAFETY: callers only pass a (size, align) taken from a valid Layout.
        unsafe { Layout::from_size_align_unchecked(size, align) }
    }

    /// Allocate `size` bytes aligned to `align`. Mirrors `GlobalAlloc::alloc`.
    #[unsafe(no_mangle)]
    pub extern "C" fn zolo_rt_alloc(size: usize, align: usize) -> *mut u8 {
        unsafe { BACKEND.alloc(layout(size, align)) }
    }

    /// Allocate `size` zeroed bytes aligned to `align`.
    #[unsafe(no_mangle)]
    pub extern "C" fn zolo_rt_alloc_zeroed(size: usize, align: usize) -> *mut u8 {
        unsafe { BACKEND.alloc_zeroed(layout(size, align)) }
    }

    /// Resize the `old_size`-byte, `align`-aligned allocation at `ptr` to
    /// `new_size`. Mirrors `GlobalAlloc::realloc` (whose `Layout` is the
    /// *old* one).
    ///
    /// # Safety
    ///
    /// `ptr` must currently be allocated via this same ABI with a layout of
    /// `(old_size, align)`, per `GlobalAlloc::realloc`'s contract.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn zolo_rt_realloc(
        ptr: *mut u8,
        old_size: usize,
        align: usize,
        new_size: usize,
    ) -> *mut u8 {
        unsafe { BACKEND.realloc(ptr, layout(old_size, align), new_size) }
    }

    /// Free the `size`-byte, `align`-aligned block at `ptr`. The size/align
    /// are part of the ABI so backends that need them (System, snmalloc)
    /// work; mimalloc ignores them and uses its own segment metadata.
    ///
    /// # Safety
    ///
    /// `ptr` must currently be allocated via this same ABI with a layout of
    /// `(size, align)` and must not be used again after this call.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn zolo_rt_dealloc(ptr: *mut u8, size: usize, align: usize) {
        unsafe { BACKEND.dealloc(ptr, layout(size, align)) }
    }

    /// Global allocator for HOST binaries (`zolo`, `zolo-lsp`, `zolo-dap`).
    /// Calls the exported `zolo_rt_*` functions directly — zero indirection,
    /// and the calls anchor those symbols so the linker keeps them for the
    /// exe's export table.
    pub struct ZoloAllocHost;

    unsafe impl GlobalAlloc for ZoloAllocHost {
        #[inline]
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            zolo_rt_alloc(l.size(), l.align())
        }
        #[inline]
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            zolo_rt_alloc_zeroed(l.size(), l.align())
        }
        #[inline]
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            unsafe { zolo_rt_dealloc(p, l.size(), l.align()) }
        }
        #[inline]
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            unsafe { zolo_rt_realloc(p, l.size(), l.align(), n) }
        }
    }
}

#[cfg(feature = "host")]
pub use host::{
    ZoloAllocHost, zolo_rt_alloc, zolo_rt_alloc_zeroed, zolo_rt_dealloc, zolo_rt_realloc,
};

// ─── Plugin half: lazy-resolving shim ────────────────────────────────────────

#[cfg(any(unix, windows))]
mod shim {
    use super::*;
    use core::sync::atomic::{AtomicUsize, Ordering};

    type RtAlloc = unsafe extern "C" fn(usize, usize) -> *mut u8;
    type RtRealloc = unsafe extern "C" fn(*mut u8, usize, usize, usize) -> *mut u8;
    type RtDealloc = unsafe extern "C" fn(*mut u8, usize, usize);

    // Resolved function addresses; 0 = not yet resolved. Resolution is
    // idempotent (same addresses every time), so a benign race between threads
    // just stores identical values.
    static ALLOC: AtomicUsize = AtomicUsize::new(0);
    static ALLOC_ZEROED: AtomicUsize = AtomicUsize::new(0);
    static REALLOC: AtomicUsize = AtomicUsize::new(0);
    static DEALLOC: AtomicUsize = AtomicUsize::new(0);

    /// Resolve all four symbols from the host executable. Returns false if any
    /// is missing. Never allocates (we may BE the allocator mid-call).
    #[cold]
    fn resolve() -> bool {
        let a = sys::lookup(c"zolo_rt_alloc");
        let z = sys::lookup(c"zolo_rt_alloc_zeroed");
        let r = sys::lookup(c"zolo_rt_realloc");
        let d = sys::lookup(c"zolo_rt_dealloc");
        if a == 0 || z == 0 || r == 0 || d == 0 {
            return false;
        }
        ALLOC_ZEROED.store(z, Ordering::Release);
        REALLOC.store(r, Ordering::Release);
        DEALLOC.store(d, Ordering::Release);
        ALLOC.store(a, Ordering::Release);
        true
    }

    #[inline]
    fn fetch(slot: &AtomicUsize) -> usize {
        let p = slot.load(Ordering::Acquire);
        if p != 0 {
            return p;
        }
        if !resolve() {
            die();
        }
        slot.load(Ordering::Acquire)
    }

    /// Non-aborting probe: does this process export the shared-allocator ABI?
    /// The plugin loader calls this BEFORE dlopen'ing a plugin, turning what
    /// would be a plugin-side abort into a friendly load error.
    pub fn host_allocator_exported() -> bool {
        ALLOC.load(Ordering::Acquire) != 0 || resolve()
    }

    #[cold]
    fn die() -> ! {
        // Raw write: we are inside the global allocator, so anything that
        // might allocate (eprintln!, panic formatting) could recurse.
        sys::raw_stderr(
            b"fatal: this process does not export the zolo shared-allocator \
symbols (zolo_rt_*).\nzolo plugins must be loaded by a zolo host (zolo, \
zolo-lsp, zolo-dap, or a zolo-built executable). If you are embedding zolo \
plugins in your own binary, enable zolo-alloc's `host` feature and emit the \
zolo-alloc-exports link args from your build.rs.\n",
        );
        std::process::abort()
    }

    /// Global allocator installed by `zolo_plugin::export_plugin!` in every
    /// plugin cdylib: forwards to the host executable's single heap.
    pub struct ZoloAlloc;

    unsafe impl GlobalAlloc for ZoloAlloc {
        #[inline]
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let f: RtAlloc = unsafe { core::mem::transmute(fetch(&ALLOC)) };
            unsafe { f(l.size(), l.align()) }
        }
        #[inline]
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            let f: RtAlloc = unsafe { core::mem::transmute(fetch(&ALLOC_ZEROED)) };
            unsafe { f(l.size(), l.align()) }
        }
        #[inline]
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            let f: RtDealloc = unsafe { core::mem::transmute(fetch(&DEALLOC)) };
            unsafe { f(p, l.size(), l.align()) }
        }
        #[inline]
        unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
            let f: RtRealloc = unsafe { core::mem::transmute(fetch(&REALLOC)) };
            unsafe { f(p, l.size(), l.align(), n) }
        }
    }

    #[cfg(windows)]
    mod sys {
        use core::ffi::{CStr, c_void};

        #[link(name = "kernel32", kind = "raw-dylib")]
        unsafe extern "system" {
            fn GetModuleHandleW(name: *const u16) -> *mut c_void;
            fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
            fn GetStdHandle(which: u32) -> *mut c_void;
            fn WriteFile(
                h: *mut c_void,
                buf: *const u8,
                len: u32,
                written: *mut u32,
                overlapped: *mut c_void,
            ) -> i32;
        }

        pub fn lookup(name: &CStr) -> usize {
            unsafe {
                // NULL = the process's executable module, whatever its name.
                let exe = GetModuleHandleW(core::ptr::null());
                if exe.is_null() {
                    return 0;
                }
                GetProcAddress(exe, name.as_ptr().cast()) as usize
            }
        }

        pub fn raw_stderr(bytes: &[u8]) {
            const STD_ERROR_HANDLE: u32 = -12i32 as u32;
            unsafe {
                let h = GetStdHandle(STD_ERROR_HANDLE);
                let mut written = 0u32;
                let _ = WriteFile(
                    h,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    &mut written,
                    core::ptr::null_mut(),
                );
            }
        }
    }

    #[cfg(unix)]
    mod sys {
        use core::ffi::{CStr, c_char, c_int, c_void};

        // dlsym lives in libc proper on glibc ≥ 2.34 / musl / macOS's
        // libSystem; the explicit -ldl keeps older glibc working (libdl.so.2
        // remains as a stub on new systems).
        #[cfg_attr(all(target_os = "linux", target_env = "gnu"), link(name = "dl"))]
        unsafe extern "C" {
            fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
            fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
        }

        // RTLD_DEFAULT: 0 on glibc/musl, -2 on macOS.
        #[cfg(target_os = "macos")]
        const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
        #[cfg(not(target_os = "macos"))]
        const RTLD_DEFAULT: *mut c_void = core::ptr::null_mut();

        pub fn lookup(name: &CStr) -> usize {
            unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) as usize }
        }

        pub fn raw_stderr(bytes: &[u8]) {
            unsafe {
                let _ = write(2, bytes.as_ptr().cast(), bytes.len());
            }
        }
    }
}

#[cfg(any(unix, windows))]
pub use shim::{ZoloAlloc, host_allocator_exported};
