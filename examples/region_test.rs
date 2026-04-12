// Test binary for apmc region mode.
//
//   cargo build --release --example region_test
//   sudo apmc stat --region -- ./target/release/examples/region_test
//
// Expected: counters reflect only the measured region, not the
// surrounding uncounted work.

fn main() {
    // Work outside region — should NOT be counted.
    for i in 0..10_000_000u64 {
        std::hint::black_box(i);
    }

    // Measured region.
    apmc::region::start();
    for i in 0..10_000_000u64 {
        std::hint::black_box(i * i);
    }
    apmc::region::stop();

    // More uncounted work.
    for i in 0..10_000_000u64 {
        std::hint::black_box(i);
    }
}
