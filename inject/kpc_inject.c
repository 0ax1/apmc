// DYLD_INSERT_LIBRARIES helper for per-process PMC counting.
//
// Two complementary collection mechanisms cover all thread lifecycle patterns:
//
// 1. TLS destructor: When a thread terminates (e.g., spawn/join), the TLS
//    destructor fires while kpc state is still live, reads the thread's final
//    counters, and atomically accumulates the delta.
//
// 2. SIGUSR2 signal: At process exit, the library destructor enumerates all
//    live threads (e.g., thread pool workers that never terminate) via Mach
//    task_threads, sends each SIGUSR2. The handler runs in each thread's
//    context where kpc_get_thread_counters(0) reads that thread's counters.
//
// The parent process must have already configured and enabled kpc counting.

#include <dlfcn.h>
#include <mach/mach.h>
#include <pthread/introspection.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <stdatomic.h>

typedef int (*kpc_get_thread_counters_fn)(unsigned int tid, unsigned int count, unsigned long long *buf);
typedef int (*kpc_get_counter_count_fn)(unsigned int classes);

#define KPC_CLASS_FIXED  (1 << 0)
#define KPC_CLASS_CONFIG (1 << 1)
#define MAX_COUNTERS 16

static int g_n_total;
static int g_result_fd = -1;
static kpc_get_thread_counters_fn g_get_thread_counters;
static pthread_introspection_hook_t g_prev_hook;

// Accumulated counter values across all threads.
static _Atomic unsigned long long g_accum[MAX_COUNTERS];

// Number of threads still pending signal collection.
static _Atomic int g_remaining;

// Per-thread storage for the "start" snapshot.
static pthread_key_t g_key;

// Accumulate (current - start) counters for the calling thread.
static void accumulate_current_thread(unsigned long long *start) {
    unsigned long long end[MAX_COUNTERS] = {0};
    g_get_thread_counters(0, g_n_total, end);
    for (int i = 0; i < g_n_total; i++) {
        atomic_fetch_add(&g_accum[i], end[i] - start[i]);
    }
}

// TLS destructor: fires during thread teardown while kpc state is still live.
// Handles threads that terminate naturally (spawn/join pattern).
static void tls_destructor(void *arg) {
    unsigned long long *start = arg;
    if (!start || !g_get_thread_counters) return;
    accumulate_current_thread(start);
    free(start);
    // TLS value is automatically cleared after destructor returns.
}

// SIGUSR2 handler: runs on target thread's context where tid=0 reads
// that thread's own counters. Handles live threads at process exit
// (thread pool pattern).
static void collect_handler(int sig) {
    (void)sig;
    unsigned long long *start = pthread_getspecific(g_key);
    if (start && g_get_thread_counters) {
        accumulate_current_thread(start);
        free(start);
        pthread_setspecific(g_key, NULL);
    }
    atomic_fetch_sub(&g_remaining, 1);
}

static void thread_hook(unsigned int event, pthread_t thread,
                        void *addr, size_t size) {
    if (g_get_thread_counters == NULL) goto chain;

    if (event == PTHREAD_INTROSPECTION_THREAD_START) {
        // Record initial counter values in TLS. The TLS destructor or
        // SIGUSR2 handler will read final values and accumulate the delta.
        unsigned long long *start = malloc(sizeof(unsigned long long) * MAX_COUNTERS);
        if (start) {
            g_get_thread_counters(0, g_n_total, start);
            pthread_setspecific(g_key, start);
        }
    }
    // THREAD_TERMINATE: no action needed — the TLS destructor handles it.
    // (By this point, TLS may already be cleaned up anyway.)

chain:
    if (g_prev_hook) g_prev_hook(event, thread, addr, size);
}

__attribute__((constructor))
static void kpc_inject_init(void) {
    const char *fd_str = getenv("KPC_RESULT_FD");
    if (!fd_str) return;
    g_result_fd = atoi(fd_str);

    void *h = dlopen("/System/Library/PrivateFrameworks/kperf.framework/kperf", RTLD_LAZY);
    if (!h) return;

    g_get_thread_counters = dlsym(h, "kpc_get_thread_counters");
    kpc_get_counter_count_fn get_count = dlsym(h, "kpc_get_counter_count");
    if (!g_get_thread_counters || !get_count) return;

    int n_fixed = get_count(KPC_CLASS_FIXED);
    int n_config = get_count(KPC_CLASS_CONFIG);
    g_n_total = n_fixed + n_config;
    if (g_n_total > MAX_COUNTERS) g_n_total = MAX_COUNTERS;

    for (int i = 0; i < MAX_COUNTERS; i++)
        atomic_store(&g_accum[i], 0);

    // TLS key with destructor for natural thread termination.
    pthread_key_create(&g_key, tls_destructor);

    // SIGUSR2 handler for live thread collection at exit.
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = collect_handler;
    sa.sa_flags = 0;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR2, &sa, NULL);

    // Record main thread start.
    unsigned long long *start = malloc(sizeof(unsigned long long) * MAX_COUNTERS);
    if (start) {
        g_get_thread_counters(0, g_n_total, start);
        pthread_setspecific(g_key, start);
    }

    g_prev_hook = pthread_introspection_hook_install(thread_hook);
}

__attribute__((destructor))
static void kpc_inject_fini(void) {
    if (g_result_fd < 0 || !g_get_thread_counters) return;

    // Capture main thread's final delta and clear TLS to prevent the
    // TLS destructor from double-counting.
    unsigned long long *start = pthread_getspecific(g_key);
    if (start) {
        accumulate_current_thread(start);
        free(start);
        pthread_setspecific(g_key, NULL);
    }

    // Enumerate all live threads and signal them to collect their counters.
    // This captures thread-pool workers that never terminate naturally.
    thread_act_array_t threads = NULL;
    mach_msg_type_number_t thread_count = 0;
    kern_return_t kr = task_threads(mach_task_self(), &threads, &thread_count);

    if (kr == KERN_SUCCESS && thread_count > 0) {
        mach_port_t self_thread = mach_thread_self();
        int n_to_signal = 0;

        for (mach_msg_type_number_t i = 0; i < thread_count; i++) {
            if (threads[i] == self_thread) continue;
            pthread_t pt = pthread_from_mach_thread_np(threads[i]);
            if (pt != NULL) n_to_signal++;
        }

        if (n_to_signal > 0) {
            atomic_store(&g_remaining, n_to_signal);

            for (mach_msg_type_number_t i = 0; i < thread_count; i++) {
                if (threads[i] == self_thread) continue;
                pthread_t pt = pthread_from_mach_thread_np(threads[i]);
                if (pt != NULL) {
                    pthread_kill(pt, SIGUSR2);
                }
            }

            // Wait for all signal handlers to complete (100ms max).
            for (int w = 0; w < 1000 && atomic_load(&g_remaining) > 0; w++) {
                usleep(100);
            }
        }

        mach_port_deallocate(mach_task_self(), self_thread);
        vm_deallocate(mach_task_self(), (vm_address_t)threads,
                      sizeof(thread_act_t) * thread_count);
    }

    // Write accumulated totals to the pipe.
    unsigned long long totals[MAX_COUNTERS];
    for (int i = 0; i < g_n_total; i++)
        totals[i] = atomic_load(&g_accum[i]);

    unsigned int n = (unsigned int)g_n_total;
    write(g_result_fd, &n, sizeof(n));
    write(g_result_fd, totals, sizeof(unsigned long long) * g_n_total);
    close(g_result_fd);
}
