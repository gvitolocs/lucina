/* What Linux's teo idle-governor decision functions need of the kernel, for the oracle. The per-CPU macros collapse to one shared
 * struct (main drives a single CPU), and the tick_nohz and cpuidle lookups read globals that main sets before every select and
 * reflect; the rest are the cpuidle types the governor walks. */
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef int32_t s32;
typedef uint32_t u32;
typedef int64_t s64;
typedef uint64_t u64;
typedef s64 ktime_t;
#define S64_MAX INT64_MAX
#define KTIME_MAX S64_MAX
#define NSEC_PER_USEC 1000L
#define TICK_NSEC 1000000L
#define CPUIDLE_STATE_MAX 10
#define CPUIDLE_FLAG_POLLING 1
#define __init
#define postcore_initcall(x)
#define DEFINE_PER_CPU(type, name) static type name
#define this_cpu_ptr(p) (p)
#define per_cpu_ptr(p, cpu) (p)

struct cpuidle_state {
	s64 target_residency_ns;
	s64 exit_latency_ns;
	unsigned int flags;
};
struct cpuidle_driver {
	struct cpuidle_state states[CPUIDLE_STATE_MAX];
	int state_count;
};
struct cpuidle_state_usage {
	unsigned long long disable;
};
struct cpuidle_device {
	int cpu;
	int last_state_idx;
	s64 last_residency_ns;
	bool poll_time_limit;
	struct cpuidle_state_usage states_usage[CPUIDLE_STATE_MAX];
};
struct cpuidle_governor {
	const char *name;
	int rating;
	int (*enable)(struct cpuidle_driver *, struct cpuidle_device *);
	int (*select)(struct cpuidle_driver *, struct cpuidle_device *, bool *);
	void (*reflect)(struct cpuidle_device *, int);
};
static inline int cpuidle_register_governor(struct cpuidle_governor *g) { (void)g; return 0; }

/* What main sets before each select and reflect. */
extern s64 g_latency_req;
extern s64 g_sleep;
extern s64 g_delta;
extern int g_got_tick;
static inline s64 cpuidle_governor_latency_req(int cpu) { (void)cpu; return g_latency_req; }
static inline bool tick_nohz_tick_stopped(void) { return 0; }
static inline s64 tick_nohz_get_sleep_length(ktime_t *d) { *d = g_delta; return g_sleep; }
static inline bool tick_nohz_idle_got_tick(void) { return g_got_tick; }
