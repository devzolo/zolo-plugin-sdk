//! Build-script helper for binaries that host zolo plugins.
//!
//! A plugin-hosting executable owns the single process-wide allocator (see
//! `zolo-alloc` with the `host` feature) and must EXPORT the four `zolo_rt_*`
//! symbols from its own dynamic symbol table — plugin DLLs resolve them at
//! runtime via `GetProcAddress(GetModuleHandleW(NULL))` / `dlsym(RTLD_DEFAULT)`.
//! Call [`emit`] from the binary crate's build.rs:
//!
//! ```ignore
//! fn main() { zolo_alloc_exports::emit("rustc-link-arg-bins"); }
//! ```
//!
//! Per-toolchain behavior:
//! - **MSVC**: `/EXPORT:sym` — creates the export AND forces the symbol to be
//!   resolved from the archives (an exe can carry an export table like a DLL).
//! - **windows-gnu**: a generated `.def` file passed as a linker input, plus
//!   `-Wl,-u,sym` to pull the defining objects from the rlib archives.
//! - **Linux/ELF**: `-Wl,-u,sym` (archive pull) + `-Wl,--export-dynamic-symbol=sym`
//!   (adds to .dynsym; binutils ≥ 2.35 and lld both support it).
//! - **macOS**: `-Wl,-u,_sym` (Mach-O underscore prefix). Executables put their
//!   global symbols in the export trie by default, so no explicit export flag —
//!   and we deliberately avoid `-exported_symbol`, which would *restrict* the
//!   export list.

pub const ZOLO_RT_SYMBOLS: [&str; 4] = [
    "zolo_rt_alloc",
    "zolo_rt_alloc_zeroed",
    "zolo_rt_realloc",
    "zolo_rt_dealloc",
];

/// Emit `cargo:{directive}=...` lines exporting the allocator symbols.
/// `directive` is typically `"rustc-link-arg-bins"`; use
/// `"rustc-link-arg-tests"` for packages whose test binaries load plugins
/// in-process.
pub fn emit(directive: &str) {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    match (os.as_str(), env.as_str()) {
        ("windows", "msvc") => {
            for s in ZOLO_RT_SYMBOLS {
                println!("cargo:{directive}=/EXPORT:{s}");
            }
        }
        ("windows", _) => {
            // GNU ld for PE accepts a .def file as a linker input.
            let out = std::env::var("OUT_DIR").expect("OUT_DIR set in build scripts");
            let def = std::path::Path::new(&out).join("zolo_rt_exports.def");
            let mut body = String::from("EXPORTS\n");
            for s in ZOLO_RT_SYMBOLS {
                body.push_str("    ");
                body.push_str(s);
                body.push('\n');
            }
            std::fs::write(&def, body).expect("write zolo_rt_exports.def");
            for s in ZOLO_RT_SYMBOLS {
                println!("cargo:{directive}=-Wl,-u,{s}");
            }
            println!("cargo:{directive}={}", def.display());
        }
        ("macos" | "ios", _) => {
            for s in ZOLO_RT_SYMBOLS {
                println!("cargo:{directive}=-Wl,-u,_{s}");
            }
        }
        _ => {
            for s in ZOLO_RT_SYMBOLS {
                println!("cargo:{directive}=-Wl,-u,{s}");
                println!("cargo:{directive}=-Wl,--export-dynamic-symbol={s}");
            }
        }
    }
}
