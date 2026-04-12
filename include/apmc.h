// apmc region mode API.
//
// Measure hardware performance counters for specific code sections:
//
//   apmc_start();
//   // ... hot code ...
//   apmc_stop();
//
// When the apmc inject dylib is loaded (`apmc stat --region`), these functions
// start/stop counter measurement for the calling thread. Multiple start/stop
// pairs accumulate. Results are summed across all threads and regions.
//
// When the dylib is NOT loaded, these are zero-cost no-ops.
//
// Compile with: -I /path/to/apmc/include

#ifndef APMC_H
#define APMC_H

#include <dlfcn.h>
#include <stdatomic.h>

#ifdef __cplusplus
extern "C" {
#endif

static inline void apmc_start(void) {
    // Atomic acquire/release ensures the function pointer is visible to any
    // thread that observes resolved == 1, even on weakly-ordered ARM.
    static void (*fn)(void);
    static _Atomic int resolved;
    if (!atomic_load_explicit(&resolved, memory_order_acquire)) {
        *(void **)&fn = dlsym(RTLD_DEFAULT, "apmc_start_impl");
        atomic_store_explicit(&resolved, 1, memory_order_release);
    }
    if (fn) {
        fn();
    }
}

static inline void apmc_stop(void) {
    static void (*fn)(void);
    static _Atomic int resolved;
    if (!atomic_load_explicit(&resolved, memory_order_acquire)) {
        *(void **)&fn = dlsym(RTLD_DEFAULT, "apmc_stop_impl");
        atomic_store_explicit(&resolved, 1, memory_order_release);
    }
    if (fn) {
        fn();
    }
}

#ifdef __cplusplus
}
#endif

#endif /* APMC_H */
