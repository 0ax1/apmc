// DYLD_INSERT_LIBRARIES helper for per-process PMC counting.
//
// Architecture:
//   - Global slot array stores per-thread start counters (no heap allocation)
//   - __thread variable stores each thread's slot index (async-signal-safe)
//   - pthread_key destructor fires on natural thread termination (spawn/join)
//   - SIGUSR2 handler collects counters from live threads at exit (thread pools)
//   - CAS on slot state ensures exactly-once collection regardless of mechanism
//   - pthread_atfork child handler resets state after fork-without-exec, so each
//     descendant process accumulates and reports its own counters independently
//
// All operations in the signal handler are async-signal-safe:
//   __thread read, atomic CAS, kpc_get_thread_counters (Mach trap), atomic add.
//
// The parent process must have already configured and enabled kpc counting.
//
// Note: This dylib installs a SIGUSR2 handler. If the target program uses
// SIGUSR2, it will be overridden. macOS does not support POSIX real-time
// signals, so there is no less-intrusive alternative.

#include <dlfcn.h>
#include <mach/mach.h>
#include <pthread/introspection.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

typedef int (*kpc_get_thread_counters_fn)(unsigned int tid, unsigned int count, unsigned long long *buf);
typedef int (*kpc_get_counter_count_fn)(unsigned int classes);

#define KPC_CLASS_FIXED (1 << 0)
#define KPC_CLASS_CONFIG (1 << 1)
#define MAX_COUNTERS 16
#define MAX_THREAD_SLOTS 1024

enum { SLOT_FREE = 0, SLOT_ACTIVE = 1, SLOT_COLLECTED = 2 };

struct thread_slot {
    _Atomic int state;
    unsigned long long start[MAX_COUNTERS];
};

static int g_total_counters;
static int g_result_fd = -1;
static kpc_get_thread_counters_fn g_get_thread_counters;
static pthread_introspection_hook_t g_previous_hook;

static struct thread_slot g_slots[MAX_THREAD_SLOTS];
static _Atomic int g_next_slot;

static _Atomic unsigned long long g_accumulated[MAX_COUNTERS];
static _Atomic int g_pending_signals;

// Each thread's index into g_slots, or -1 if not tracked.
// Declared __thread (TLS) so the SIGUSR2 signal handler can read it
// without locks or atomic ops — accessing a __thread variable compiles
// to a segment-relative memory load, which is async-signal-safe.
static __thread int t_my_slot = -1;

// Used only to trigger destructor on natural thread termination.
static pthread_key_t g_tls_key;

// Collect counters for a slot. CAS ensures exactly-once semantics —
// safe to call from both signal handler and TLS destructor.
static void collect_slot(int slot) {
    if (slot < 0 || slot >= MAX_THREAD_SLOTS) {
        return;
    }
    int expected = SLOT_ACTIVE;
    if (atomic_compare_exchange_strong(&g_slots[slot].state, &expected, SLOT_COLLECTED)) {
        unsigned long long current[MAX_COUNTERS] = {0};
        g_get_thread_counters(0, g_total_counters, current);
        for (int idx = 0; idx < g_total_counters; ++idx) {
            atomic_fetch_add(&g_accumulated[idx], current[idx] - g_slots[slot].start[idx]);
        }
    }
}

// SIGUSR2 signal handler invoked on each live thread during process teardown.
// Collects the calling thread's counter delta via collect_slot (read current
// counters, subtract start snapshot, accumulate into g_accumulated), then
// decrements g_pending_signals so the finalizer knows when all threads have
// reported. Fully async-signal-safe — uses only __thread reads, atomic CAS,
// a Mach trap (kpc_get_thread_counters), and atomic add/sub.
static void collect_handler(int signal_number) {
    (void)signal_number;
    if (g_get_thread_counters) {
        collect_slot(t_my_slot);
    }
    atomic_fetch_sub(&g_pending_signals, 1);
}

// TLS destructor — fires during thread teardown for naturally terminating
// threads. Uses the slot index from the pthread_key value (not __thread,
// which may already be torn down). CAS prevents double-counting.
static void tls_destructor(void *arg) {
    int slot = (int)(intptr_t)arg - 1;
    if (g_get_thread_counters) {
        collect_slot(slot);
    }
}

static void thread_hook(unsigned int event, pthread_t thread, void *addr, size_t size) {
    if (g_get_thread_counters == NULL) {
        goto chain;
    }

    if (event == PTHREAD_INTROSPECTION_THREAD_START) {
        int slot = atomic_fetch_add(&g_next_slot, 1);
        if (slot < MAX_THREAD_SLOTS) {
            t_my_slot = slot;
            g_get_thread_counters(0, g_total_counters, g_slots[slot].start);
            atomic_store(&g_slots[slot].state, SLOT_ACTIVE);
            // Non-NULL value triggers destructor on thread exit.
            pthread_setspecific(g_tls_key, (void *)(intptr_t)(slot + 1));
        }
    }

chain:
    // Forward to any previously installed introspection hook so we don't
    // break other DYLD_INSERT_LIBRARIES dylibs or runtime tooling that
    // also monitors thread lifecycle events.
    if (g_previous_hook) {
        g_previous_hook(event, thread, addr, size);
    }
}

// pthread_atfork child handler — runs in the child process immediately after
// fork(). Resets all dylib state so the child accumulates only its own work
// and writes an independent result message to the inherited pipe fd.
// This handles programs like `stress` that fork workers without exec().
static void atfork_child(void) {
    if (!g_get_thread_counters || g_result_fd < 0) {
        return;
    }

    // Zero accumulated counters — parent's totals belong to the parent.
    for (int idx = 0; idx < MAX_COUNTERS; ++idx) {
        atomic_store(&g_accumulated[idx], 0);
    }

    // Mark all inherited slots as collected so the destructor won't try
    // to collect stale parent snapshots.
    int n_slots = atomic_load(&g_next_slot);
    for (int idx = 0; idx < n_slots && idx < MAX_THREAD_SLOTS; ++idx) {
        atomic_store(&g_slots[idx].state, SLOT_COLLECTED);
    }

    // Reset slot allocator and pending signal count.
    atomic_store(&g_next_slot, 0);
    atomic_store(&g_pending_signals, 0);

    // Re-create pthread key for this process (only one thread exists
    // after fork, so the old key's destructor state is stale).
    pthread_key_delete(g_tls_key);
    pthread_key_create(&g_tls_key, tls_destructor);

    // Snapshot the (only) thread's counters fresh.
    int slot = atomic_fetch_add(&g_next_slot, 1);
    if (slot < MAX_THREAD_SLOTS) {
        t_my_slot = slot;
        g_get_thread_counters(0, g_total_counters, g_slots[slot].start);
        atomic_store(&g_slots[slot].state, SLOT_ACTIVE);
        pthread_setspecific(g_tls_key, (void *)(intptr_t)(slot + 1));
    }

    // Note: the pthread introspection hook and SIGUSR2 handler are inherited
    // across fork and remain valid — do NOT re-install them, as that would
    // set g_previous_hook to thread_hook itself, creating infinite recursion.
}

// Dylib constructor — runs when DYLD_INSERT_LIBRARIES loads this dylib into
// the target process. Resolves kperf symbols, installs the pthread introspection
// hook to capture thread starts, registers a SIGUSR2 handler for collecting
// counters from live threads at exit, and snapshots the main thread's counters.
__attribute__((constructor)) static void kpc_inject_initialize(void) {
    const char *result_fd_env = getenv("KPC_RESULT_FD");
    if (!result_fd_env) {
        return;
    }
    g_result_fd = atoi(result_fd_env);

    void *kperf_handle = dlopen("/System/Library/PrivateFrameworks/kperf.framework/kperf", RTLD_LAZY);
    if (!kperf_handle) {
        return;
    }

    // POSIX dlsym returns void*, but we need function pointers. Cast through
    // void** to avoid -Wpedantic warnings about void*-to-function-pointer.
    *(void **)&g_get_thread_counters = dlsym(kperf_handle, "kpc_get_thread_counters");
    kpc_get_counter_count_fn get_counter_count;
    *(void **)&get_counter_count = dlsym(kperf_handle, "kpc_get_counter_count");
    if (!g_get_thread_counters || !get_counter_count) {
        return;
    }

    int fixed_counter_count = get_counter_count(KPC_CLASS_FIXED);
    int config_counter_count = get_counter_count(KPC_CLASS_CONFIG);
    g_total_counters = fixed_counter_count + config_counter_count;
    if (g_total_counters > MAX_COUNTERS) {
        g_total_counters = MAX_COUNTERS;
    }

    for (int idx = 0; idx < MAX_COUNTERS; ++idx) {
        atomic_store(&g_accumulated[idx], 0);
    }
    atomic_store(&g_next_slot, 0);

    pthread_key_create(&g_tls_key, tls_destructor);

    struct sigaction sig_action;
    memset(&sig_action, 0, sizeof(sig_action));
    sig_action.sa_handler = collect_handler;
    sig_action.sa_flags = 0;
    sigemptyset(&sig_action.sa_mask);
    sigaction(SIGUSR2, &sig_action, NULL);

    // Record main thread start.
    int slot = atomic_fetch_add(&g_next_slot, 1);
    if (slot < MAX_THREAD_SLOTS) {
        t_my_slot = slot;
        g_get_thread_counters(0, g_total_counters, g_slots[slot].start);
        atomic_store(&g_slots[slot].state, SLOT_ACTIVE);
        pthread_setspecific(g_tls_key, (void *)(intptr_t)(slot + 1));
    }

    g_previous_hook = pthread_introspection_hook_install(thread_hook);

    // Register fork handler so child processes reset state and report
    // their own counters independently.
    pthread_atfork(NULL, NULL, atfork_child);
}

// Dylib destructor — runs during process teardown. Collects the main thread's
// counters, signals all remaining live threads via SIGUSR2 to collect theirs,
// waits briefly for responses, then writes the accumulated totals to the pipe
// fd for the parent process to read.
__attribute__((destructor)) static void kpc_inject_finalize(void) {
    if (g_result_fd < 0 || !g_get_thread_counters) {
        return;
    }

    // Collect main thread counters (destructor runs on main thread).
    collect_slot(t_my_slot);

    // Signal all live threads to collect their counters.
    thread_act_array_t threads = NULL;
    mach_msg_type_number_t thread_count = 0;
    kern_return_t ret = task_threads(mach_task_self(), &threads, &thread_count);

    if (ret == KERN_SUCCESS && thread_count > 0) {
        mach_port_t current_thread = mach_thread_self();
        int threads_to_signal = 0;

        for (mach_msg_type_number_t tidx = 0; tidx < thread_count; ++tidx) {
            if (threads[tidx] == current_thread) {
                continue;
            }
            pthread_t thread = pthread_from_mach_thread_np(threads[tidx]);
            if (thread != NULL) {
                threads_to_signal++;
            }
        }

        if (threads_to_signal > 0) {
            atomic_store(&g_pending_signals, threads_to_signal);

            for (mach_msg_type_number_t tidx = 0; tidx < thread_count; ++tidx) {
                if (threads[tidx] == current_thread) {
                    continue;
                }
                pthread_t thread = pthread_from_mach_thread_np(threads[tidx]);
                if (thread != NULL) {
                    pthread_kill(thread, SIGUSR2);
                }
            }

            for (int attempt = 0; attempt < 1000 && atomic_load(&g_pending_signals) > 0; ++attempt) {
                usleep(100);
            }
        }

        mach_port_deallocate(mach_task_self(), current_thread);
        // task_threads() allocates via Mach VM, not malloc — must use
        // vm_deallocate to free the out-of-line thread array.
        vm_deallocate(mach_task_self(), (vm_address_t)threads, sizeof(thread_act_t) * thread_count);
    }

    // Write accumulated totals as a single atomic write. Multiple processes
    // (fork children) may write to the same pipe concurrently — a single
    // write() up to PIPE_BUF (>=512 on macOS) is guaranteed atomic.
    // Max payload: 4 + 16*8 = 132 bytes, well under PIPE_BUF.
    unsigned long long totals[MAX_COUNTERS];
    for (int idx = 0; idx < g_total_counters; ++idx) {
        totals[idx] = atomic_load(&g_accumulated[idx]);
    }

    unsigned char msg[sizeof(unsigned int) + MAX_COUNTERS * sizeof(unsigned long long)];
    unsigned int counter_count = (unsigned int)g_total_counters;
    memcpy(msg, &counter_count, sizeof(counter_count));
    memcpy(msg + sizeof(counter_count), totals,
           sizeof(unsigned long long) * g_total_counters);
    size_t msg_len = sizeof(counter_count) + sizeof(unsigned long long) * g_total_counters;
    write(g_result_fd, msg, msg_len);
    close(g_result_fd);
}
