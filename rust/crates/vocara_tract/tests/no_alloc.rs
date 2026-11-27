//! Once warm, a frame allocates nothing: the inversion (fits, restarts from the codebook,
//! holds through unvoiced frames), the forward model's formants, the 2D cut-away and the 3D
//! head of the estimate. Counted with a global allocator of this test binary's own, so this
//! file holds this one test.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use vocara_tract::anatomy::{BodyMix, BodyName};
use vocara_tract::components::{TractComponents, VocalFolds};
use vocara_tract::forward::TractModel;
use vocara_tract::inversion::{FormantFrame, Inverter};
use vocara_tract::lines::{Lines3d, Outlines2d};
use vocara_tract::posture::{sung_vowels, Posture};

struct Counting;

static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call is passed on to the system allocator unchanged; the counter is a side
// effect only.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `alloc` above with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: as `GlobalAlloc::realloc` requires, passed on.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn a_warm_frame_allocates_nothing() {
    let b = BodyName::Child;
    let body: BodyMix = b.into();
    let mut model = TractModel::new(body);
    let targets: Vec<[f64; 3]> = sung_vowels(b)
        .map(|v| {
            let f = model.lowest_formants(&Posture::of(&v.artic).articulation(), 3);
            [f.freq[0], f.freq[1], f.freq[2]]
        })
        .collect();
    let frames: Vec<FormantFrame> = (0..1200)
        .map(|i| {
            let vowel = i / 30;
            let breath = vowel % 4 == 3 && i % 30 < 8;
            let wobble = 1.0 + 0.02 * ((i as f64) * 0.7).sin();
            FormantFrame {
                time: i as f64 * 0.0107,
                f0: 280.0,
                formants: if breath {
                    [f64::NAN; 3]
                } else {
                    targets[(vowel * 5) % targets.len()].map(|x| x * wobble)
                },
                confidence: if breath { 0.0 } else { 1.0 },
            }
        })
        .collect();
    let mut inv = Inverter::new(body);
    let (mut flat, mut head) = (Outlines2d::default(), Lines3d::default());
    let folds = |f: &FormantFrame| VocalFolds {
        f0_hz: f.f0 * f.confidence,
        breathiness: 0.2,
        register: 0.5,
    };
    let mut frame = |inv: &mut Inverter, f: &FormantFrame, model: &mut TractModel| {
        inv.update(f);
        let a = inv.estimate().articulation;
        std::hint::black_box(model.formants(&a));
        std::hint::black_box(TractComponents::of(&a, body));
        model.outlines(&a, &mut flat);
        model.head3d(&a, &folds(f), &mut head);
    };
    // warm: the codebook, the priors, the head's rest shapes, every buffer at its working size
    for f in &frames[..400] {
        frame(&mut inv, f, &mut model);
    }
    COUNTING.store(true, Ordering::Relaxed);
    let mut restarts = 0;
    for f in &frames[400..] {
        frame(&mut inv, f, &mut model);
        restarts += usize::from(inv.last_evaluations() >= 8);
    }
    COUNTING.store(false, Ordering::Relaxed);
    assert!(
        restarts > 5,
        "the frames restarted from the codebook {restarts} times"
    );
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
