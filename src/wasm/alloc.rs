//! Routes tree-sitter's C allocations through Rust's global allocator.
//!
//! On `wasm32-unknown-unknown` the `tree-sitter` crate links the C runtime
//! against `tree-sitter-language`'s `wasm/src/stdlib.c`, a bump allocator
//! written for external scanners inside a wasmtime store. Its heap starts
//! wherever `reset_heap` puts it, and nothing outside such a store calls
//! `reset_heap`: here it starts at address 0, which is the bottom of the
//! Rust shadow stack. Parse trees then grow up through the stack and into
//! Rust's own heap, and past 4 MiB `malloc` returns `NULL` and tree-sitter
//! aborts. The host sees `memory access out of bounds` or `unreachable`
//! once enough source has been parsed — about 20K characters of schema.
//!
//! [`install`] hands tree-sitter a `malloc` family backed by
//! [`std::alloc`], so both languages share one heap.

use std::alloc::{Layout, alloc, alloc_zeroed, dealloc, handle_alloc_error, realloc};
use std::ffi::c_void;

/// `alignof(max_align_t)` for clang's wasm32 target. The header is the same
/// size so the pointer handed to C keeps that alignment.
const ALIGN: usize = 16;
const HEADER: usize = 16;

/// Install the allocator. Must run before tree-sitter allocates anything,
/// since memory from one allocator cannot be freed by the other.
pub fn install() {
    // SAFETY: called once from the module's start function, before any
    // parser, tree or query exists.
    unsafe {
        tree_sitter::set_allocator(
            Some(ts_malloc),
            Some(ts_calloc),
            Some(ts_realloc),
            Some(ts_free),
        );
    }
}

fn layout_for(size: usize) -> Layout {
    size.checked_add(HEADER)
        .and_then(|total| Layout::from_size_align(total, ALIGN).ok())
        .unwrap_or_else(|| handle_alloc_error(Layout::new::<u8>()))
}

/// Writes the allocation's size into its header and returns the C pointer.
/// tree-sitter never checks for `NULL` from a custom allocator, so failure
/// aborts here rather than later at a wild address.
unsafe fn finish(base: *mut u8, layout: Layout) -> *mut c_void {
    if base.is_null() {
        handle_alloc_error(layout);
    }
    unsafe {
        base.cast::<usize>().write(layout.size());
        base.add(HEADER).cast()
    }
}

unsafe fn header(ptr: *mut c_void) -> (*mut u8, Layout) {
    unsafe {
        let base = ptr.cast::<u8>().sub(HEADER);
        let size = base.cast::<usize>().read();
        (base, Layout::from_size_align_unchecked(size, ALIGN))
    }
}

unsafe extern "C" fn ts_malloc(size: usize) -> *mut c_void {
    let layout = layout_for(size);
    unsafe { finish(alloc(layout), layout) }
}

unsafe extern "C" fn ts_calloc(count: usize, size: usize) -> *mut c_void {
    let size = count
        .checked_mul(size)
        .unwrap_or_else(|| handle_alloc_error(Layout::new::<u8>()));
    let layout = layout_for(size);
    unsafe { finish(alloc_zeroed(layout), layout) }
}

unsafe extern "C" fn ts_realloc(ptr: *mut c_void, size: usize) -> *mut c_void {
    if ptr.is_null() {
        return unsafe { ts_malloc(size) };
    }
    let new_layout = layout_for(size);
    unsafe {
        let (base, layout) = header(ptr);
        finish(realloc(base, layout, new_layout.size()), new_layout)
    }
}

unsafe extern "C" fn ts_free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let (base, layout) = header(ptr);
        dealloc(base, layout);
    }
}
