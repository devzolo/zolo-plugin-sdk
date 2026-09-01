//! End-to-end test of the plugin-side lazy shim: this test binary plays the
//! host role (zolo-alloc's build.rs emits the export link args for tests, and
//! the `host` feature defines the symbols), and the shim must resolve them
//! from the executable and round-trip allocations through the shared heap.

use core::alloc::{GlobalAlloc, Layout};

#[test]
fn probe_reports_exported() {
    assert!(
        zolo_alloc::host_allocator_exported(),
        "test binary must export zolo_rt_* (build.rs rustc-link-arg-tests)"
    );
}

#[test]
fn shim_resolves_and_roundtrips() {
    let shim = zolo_alloc::ZoloAlloc;
    unsafe {
        let l = Layout::from_size_align(64, 16).unwrap();
        let p = shim.alloc(l);
        assert!(!p.is_null());
        p.write_bytes(0xAB, 64);
        let p2 = shim.realloc(p, l, 128);
        assert!(!p2.is_null());
        assert_eq!(*p2, 0xAB);
        shim.dealloc(p2, Layout::from_size_align(128, 16).unwrap());

        let z = shim.alloc_zeroed(l);
        assert!(!z.is_null());
        assert_eq!(core::slice::from_raw_parts(z, 64), &[0u8; 64]);
        shim.dealloc(z, l);
    }
}

#[test]
fn shim_and_host_share_one_heap() {
    // Allocate via the shim (resolved pointers), free via the host-direct
    // path — the exact cross-module pattern the plugin ABI performs.
    let shim = zolo_alloc::ZoloAlloc;
    let host = zolo_alloc::ZoloAllocHost;
    unsafe {
        let l = Layout::from_size_align(256, 8).unwrap();
        let p = shim.alloc(l);
        assert!(!p.is_null());
        host.dealloc(p, l);
    }
}
