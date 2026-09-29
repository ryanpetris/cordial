use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
struct Measured;
static MEASURE: AtomicBool = AtomicBool::new(false);
static ALLOCATED: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Measured {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURE.load(Ordering::Relaxed) {
            ALLOCATED.fetch_add(layout.size(), Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if MEASURE.load(Ordering::Relaxed) {
            ALLOCATED.fetch_add(size, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Measured = Measured;

#[test]
fn invalid_array_value_is_not_buffered_into_an_allocated_tree() {
    let line = format!(
        r#"{{"v":1,"id":7,"cmd":"hidpp.setting.set","args":{{"device_id":"d_1","key":"backlight.enabled","value":[{}0]}}}}"#,
        "0,".repeat(1900)
    );
    assert!(line.len() < 4096);
    MEASURE.store(true, Ordering::SeqCst);
    let result = cordial_protocol::codec::decode_request(line.as_bytes());
    MEASURE.store(false, Ordering::SeqCst);
    assert!(result.is_err());
    assert!(ALLOCATED.load(Ordering::Relaxed) < 2048);
}
