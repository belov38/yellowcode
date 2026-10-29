#![allow(unsafe_code)]
use memory_budget::sealed_heap::{RESOURCE_EXIT_CODE, SealError, SealedHeap};
#[global_allocator]
static HEAP: SealedHeap = SealedHeap::new();
fn main() {
    let mode = std::env::args().nth(1).expect("probe mode");
    let mut bootstrap = Vec::with_capacity(64);
    bootstrap.extend_from_slice(b"bootstrap data");
    let invalid = unsafe { HEAP.seal(0) };
    assert_eq!(invalid, Err(SealError::InvalidCapacity));
    unsafe { HEAP.seal(1 << 20) }.unwrap();
    assert_eq!(HEAP.capacity(), 1 << 20);
    match mode.as_str() {
        "aligned" => {
            for align in [1, 2, 8, 16, 128, 4096, 65536, 262144] {
                let layout = std::alloc::Layout::from_size_align(113, align).unwrap();
                let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
                assert!(!ptr.is_null());
                assert_eq!(ptr as usize % align, 0);
                let initialized = unsafe { std::slice::from_raw_parts(ptr, 113) };
                assert!(initialized.iter().all(|&b| b == 0));
                assert!(HEAP.contains(ptr));
                unsafe { std::alloc::dealloc(ptr, layout) };
            }
        }
        "realloc" => {
            assert!(!HEAP.contains(bootstrap.as_ptr()));
            bootstrap.reserve_exact(32 << 10);
            assert!(HEAP.contains(bootstrap.as_ptr()));
            assert_eq!(bootstrap.as_slice(), b"bootstrap data");
            let first = HEAP.used();
            let capacity = bootstrap.capacity();
            bootstrap.reserve_exact(capacity * 2);
            assert_eq!(bootstrap.as_slice(), b"bootstrap data");
            assert!(HEAP.used() > first);
            let used = HEAP.used();
            drop(bootstrap);
            assert_eq!(
                HEAP.used(),
                used,
                "arena deallocation must not recycle untracked capacity"
            );
            std::process::exit(0);
        }
        "concurrent" => {
            let handles: Vec<_> = (0..3)
                .map(|i| {
                    std::thread::spawn(move || {
                        (0..64)
                            .map(|j| Box::new([i * 64 + j; 32]))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            let rows: Vec<_> = handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect();
            let mut pointers: Vec<_> = rows.iter().map(|r| r.as_ptr() as usize).collect();
            assert!(rows.iter().all(|r| HEAP.contains(r.as_ptr().cast::<u8>())));
            pointers.sort_unstable();
            pointers.dedup();
            assert_eq!(pointers.len(), 192);
            for (i, row) in rows.iter().enumerate() {
                assert!(row.iter().all(|&n| n == i));
            }
        }
        "exhaust" => {
            assert_eq!(RESOURCE_EXIT_CODE, 72);
            let layout = std::alloc::Layout::from_size_align(2 << 20, 16).unwrap();
            let _pointer = unsafe { std::alloc::GlobalAlloc::alloc(&HEAP, layout) };
            std::process::exit(99);
        }
        "second" => {
            let used = HEAP.used();
            assert_eq!(unsafe { HEAP.seal(4 << 20) }, Err(SealError::AlreadyActive));
            assert_eq!(HEAP.capacity(), 1 << 20);
            assert_eq!(HEAP.used(), used);
        }
        _ => panic!("unknown test mode"),
    }
    drop(bootstrap);
    std::process::exit(0);
}
