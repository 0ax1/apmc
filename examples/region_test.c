// Test program for apmc region mode (C).
//
//   cargo build --release
//   sudo apmc stat --region -- target/release/build/apmc-*/out/region_test_c

#include <apmc.h>

// Prevent the compiler from optimizing away the loop.
static volatile unsigned long long sink;

int main(void) {
    // Work outside region — should NOT be counted.
    for (unsigned long long i = 0; i < 10000000; i++)
        sink = i;

    // Measured region.
    apmc_start();
    for (unsigned long long i = 0; i < 10000000; i++)
        sink = i * i;
    apmc_stop();

    // More uncounted work.
    for (unsigned long long i = 0; i < 10000000; i++)
        sink = i;

    return 0;
}
