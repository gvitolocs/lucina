/* Drives the cut teo governor through 18000 steps of two idle tables in three wake-up patterns and prints, for each step, the
 * inputs and the governor's answer. The inputs are a deterministic xorshift64 sequence (seed 1); the residency and got_tick of each
 * step are the wake-up outcome the Rust reimplementation replays against. teo-cut.c is Linux's code, cut out of teo.c by build.sh,
 * unchanged. */
#include "teo-cut.c"

s64 g_latency_req, g_sleep, g_delta;
int g_got_tick;

static uint64_t rng = 1;
static uint64_t rnd(void)
{
	uint64_t x = rng;
	x ^= x << 13;
	x ^= x >> 7;
	x ^= x << 17;
	rng = x;
	return x;
}
static s64 rnd_range(s64 lo, s64 hi)
{
	return lo + (s64)(rnd() % (uint64_t)(hi - lo + 1));
}
static int rnd_double_below(double p)
{
	return (double)rnd() / 18446744073709551615.0 < p;
}

struct table {
	const char *name;
	struct cpuidle_state states[3];
};
/* Juno's CPUs, from the laptop's device tree as cpuidle reads it (residency = min-residency-us, latency = entry + exit): WFI
 * (cpuidle-psci's 1 us / 1 us) and two power-down states, the silver cores' then the gold cores' (from cpu-sleep-0-x and
 * cpu-sleep-1-x). */
static struct table tables[] = {
	{ "silver", { { 1000, 1000, 0 }, { 1774000, 1450000, 0 }, { 4001000, 1617000, 0 } } },
	{ "gold", { { 1000, 1000, 0 }, { 2207000, 1767000, 0 }, { 5555000, 2380000, 0 } } },
};

static void run(const char *tname, struct cpuidle_state *states, const char *scenario)
{
	static const int STEPS = 3000;
	struct cpuidle_driver drv;
	struct cpuidle_device dev;
	int t, s, step;
	int timers_only = scenario[0] == 't';
	int irqs_only = scenario[0] == 'i';
	int mixed = scenario[0] == 'm';

	drv.state_count = 3;
	for (t = 0; t < 3; t++)
		drv.states[t] = states[t];
	dev.cpu = 0;
	dev.last_state_idx = -1;
	dev.last_residency_ns = 0;
	dev.poll_time_limit = false;
	for (s = 0; s < CPUIDLE_STATE_MAX; s++)
		dev.states_usage[s].disable = 0;
	teo_enable_device(&drv, &dev);

	for (step = 0; step < STEPS; step++) {
		int mask = 0;
		s64 sleep, residency;
		int got_tick;
		bool stop_tick = true;
		int idx;

		for (s = 0; s < 3; s++)
			dev.states_usage[s].disable = 0;

		g_delta = rnd_range(0, TICK_NSEC);
		/* A tight latency limit sometimes, in the mixed pattern. */
		g_latency_req = (mixed && rnd_double_below(0.2))
			? (rnd_double_below(0.5) ? 1500000 : 2000000)
			: S64_MAX;

		/* Half the mixed steps wake on a timer, half on an interrupt. */
		int timers_like;
		if (timers_only)
			timers_like = 1;
		else if (irqs_only)
			timers_like = 0;
		else
			timers_like = !rnd_double_below(0.5);
		if (timers_like) {
			/* A timer 2 - 50 ms out, and the CPU wakes at its due time, within 3% most of the time. */
			s64 pct = rnd_double_below(0.9) ? rnd_range(-3, 3) : rnd_range(-50, 50);
			sleep = rnd_range(2000000, 50000000);
			residency = sleep + (sleep * pct) / 100;
			got_tick = rnd_double_below(0.8);
		} else {
			/* A long timer 1 - 1000 ms out, but an interrupt wakes the CPU after 20 us - 3 ms. */
			sleep = rnd_range(1000000, 1000000000);
			residency = rnd_range(20000, 3000000);
			got_tick = rnd_double_below(0.2);
		}
		if (residency < 0)
			residency = 0;

		/* In the mixed pattern, a random state other than 0 is disabled 5% of the time. */
		if (mixed && rnd_double_below(0.05)) {
			int dis = 1 + (int)(rnd() % 2);
			dev.states_usage[dis].disable = 1;
			mask |= 1 << dis;
		}

		g_sleep = sleep;
		g_got_tick = 0;
		stop_tick = true;
		idx = teo_select(&drv, &dev, &stop_tick);

		/* The wake-up that follows entering idx. */
		dev.last_residency_ns = residency;
		g_got_tick = got_tick;
		teo_reflect(&dev, idx);

		printf("%s %s %d %lld %lld %lld %d %lld %d %d %d\n",
		       tname, scenario, step,
		       (long long)g_sleep, (long long)g_delta, (long long)g_latency_req,
		       mask, (long long)residency, got_tick, idx, (int)stop_tick);
	}
}

int main(void)
{
	int t;
	for (t = 0; t < 2; t++) {
		run(tables[t].name, tables[t].states, "timers");
		run(tables[t].name, tables[t].states, "irqs");
		run(tables[t].name, tables[t].states, "mixed");
	}
	return 0;
}
