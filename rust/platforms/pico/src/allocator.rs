use core::{
    alloc::{GlobalAlloc, Layout},
    cell::Cell,
};
use embassy_sync::blocking_mutex::{Mutex, raw::CriticalSectionRawMutex};
use talc::{TalcCell, source::Manual};

struct Heap {
    talc: TalcCell<Manual>,
    peak: Cell<usize>,
    failures: Cell<usize>,
}
impl Heap {
    fn allocated(&self, ptr: *mut u8) {
        self.peak
            .set(self.peak.get().max(self.talc.counters().allocated_bytes));
        if ptr.is_null() {
            self.failures.set(self.failures.get() + 1);
        }
    }
}
pub struct Allocator(Mutex<CriticalSectionRawMutex, Heap>);
#[global_allocator]
static ALLOCATOR: Allocator = Allocator(Mutex::new(Heap {
    talc: TalcCell::new(Manual),
    peak: Cell::new(0),
    failures: Cell::new(0),
}));

/// Claim the linker-reserved RAM after startup has initialized BSS.
///
/// # Safety
/// The caller owns this unaliased RAM region for the rest of the program.
pub unsafe fn init(base: *mut u8, bytes: usize) -> bool {
    ALLOCATOR
        .0
        .lock(|h| unsafe { h.talc.claim(base, bytes).is_some() })
}
pub struct Stats {
    pub allocated: usize,
    pub peak: usize,
    pub available: usize,
    pub overhead: usize,
    pub failures: usize,
}
pub fn stats() -> Stats {
    ALLOCATOR.0.lock(|h| {
        let c = h.talc.counters();
        Stats {
            allocated: c.allocated_bytes,
            peak: h.peak.get(),
            available: c.available_bytes,
            overhead: c.overhead_bytes(),
            failures: h.failures.get(),
        }
    })
}
// All Talc access is serialized by the platform's critical section. This also
// works on RP2040 without atomic compare-and-swap or a spinlock dependency.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.0.lock(|h| {
            let ptr = unsafe { h.talc.alloc(layout) };
            h.allocated(ptr);
            ptr
        })
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.0.lock(|h| unsafe { h.talc.dealloc(ptr, layout) });
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        self.0.lock(|h| {
            let ptr = unsafe { h.talc.realloc(ptr, layout, size) };
            h.allocated(ptr);
            ptr
        })
    }
}
