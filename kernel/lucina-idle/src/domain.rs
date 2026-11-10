// SPDX-License-Identifier: MPL-2.0

//! The cluster: when the last of its CPUs goes idle, which of the cluster's own idle states it takes.
//!
//! Behaviour of Linux's CPU PM domains (`drivers/cpuidle/cpuidle-psci-domain.c`, genpd's `cpu_power_down_ok` in
//! `drivers/pmdomain/governor.c`, 7.1), reimplemented. In OS-initiated mode each CPU that enters its deepest idle state puts
//! its own domain off; the CPU that puts the last one off is the one that may take the cluster down, and it picks the
//! deepest cluster state whose residency plus entry latency fits before the earliest wake-up of any CPU in the cluster, and
//! whose entry plus exit latency fits the latency limit. A CPU that is not idle has no wake-up time (0), which keeps the
//! cluster up, as does an inter-processor interrupt on its way to any of them. The first CPU to wake puts its domain on
//! again. The firmware checks the same claim and refuses a wrong one (`DENIED`), which the kernel counts as a rejected entry.
//!
//! The kernel holds one lock around these calls, as genpd does for an IRQ-safe CPU domain.

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The most CPUs a cluster can have here.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_CPUS: usize = 64;

/// One of the cluster's idle states, as genpd keeps it: from `min-residency-us`, `entry-latency-us`, `exit-latency-us`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub struct DomainState {
    pub residency_ns: i64,
    /// Entry latency.
    pub power_off_latency_ns: i64,
    /// Exit latency.
    pub power_on_latency_ns: i64,
}

/// Which CPUs belong to the cluster and which of them have put their domain off.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub struct Cluster {
    pub member: [bool; MAX_CPUS],
    pub down: [bool; MAX_CPUS],
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl Cluster {
    /// A cluster with no CPUs yet, all up.
    pub const fn new() -> Self {
        Self { member: [false; MAX_CPUS], down: [false; MAX_CPUS] }
    }

    /// Adds `cpu` as a running member (a CPU that came online).
    #[cfg_attr(verus_keep_ghost, verus_spec(
        requires cpu < MAX_CPUS,
        ensures
            final(self).member@ == old(self).member@.update(cpu as int, true),
            final(self).down@ == old(self).down@.update(cpu as int, false),
    ))]
    pub fn add(&mut self, cpu: usize) {
        self.member[cpu] = true;
        self.down[cpu] = false;
    }

    /// `cpu` puts its own domain off. Returns whether every member is now off: then this CPU is the last, and the cluster
    /// may go down.
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        requires cpu < MAX_CPUS,
        ensures
            final(self).member@ == old(self).member@,
            final(self).down@ == old(self).down@.update(cpu as int, true),
            r == forall|i: int| 0 <= i < MAX_CPUS as int && final(self).member@[i] ==> final(self).down@[i],
    ))]
    pub fn cpu_down(&mut self, cpu: usize) -> bool {
        self.down[cpu] = true;
        self.all_down()
    }

    /// `cpu` woke and puts its domain on again, and with it the cluster.
    #[cfg_attr(verus_keep_ghost, verus_spec(
        requires cpu < MAX_CPUS,
        ensures
            final(self).member@ == old(self).member@,
            final(self).down@ == old(self).down@.update(cpu as int, false),
    ))]
    pub fn cpu_up(&mut self, cpu: usize) {
        self.down[cpu] = false;
    }

    /// Whether every member has its domain off.
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        ensures r == forall|i: int| 0 <= i < MAX_CPUS as int && self.member@[i] ==> self.down@[i],
    ))]
    pub fn all_down(&self) -> bool {
        let mut i = 0;
        #[cfg_attr(verus_keep_ghost, verus_spec(
            invariant
                0 <= i <= MAX_CPUS,
                forall|j: int| 0 <= j < i ==> (self.member@[j] ==> self.down@[j]),
            decreases MAX_CPUS - i
        ))]
        while i < MAX_CPUS {
            if self.member[i] && !self.down[i] {
                return false;
            }
            i += 1;
        }
        true
    }

    /// The last CPU's question (cpuidle-psci-domain's power-off through genpd): the cluster state to take now, or `None`.
    /// Never a state while any member is running, nor one that does not fit, nor with an IPI pending. The kernel calls
    /// this under the cluster's lock, after [`Cluster::cpu_down`] for its own CPU, so no member can come up between the
    /// check and the decision.
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        requires states.len() > 0,
        ensures
            r.is_some() ==> forall|i: int| 0 <= i < MAX_CPUS as int && self.member@[i] ==> self.down@[i],
            r.is_some() ==> !ipi_pending && r.unwrap() < states.len()
                && state_fits_spec(states[r.unwrap() as int], idle_ns, limit_ns),
    ))]
    pub fn cluster_off(&self, states: &[DomainState], idle_ns: i64, limit_ns: i64, ipi_pending: bool) -> Option<usize> {
        if !self.all_down() {
            return None;
        }
        choose(states, idle_ns, limit_ns, ipi_pending)
    }
}

/// The earliest wake-up among the cluster's members, `i64::MAX` if none: a CPU's entry is the counter time its timer fires,
/// in nanoseconds, or 0 while it is not idle (cpuidle's `next_hrtimer`).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires member.len() == next_wakeup_ns.len(),
    ensures
        forall|i: int| 0 <= i < member.len() && member[i] ==> r <= next_wakeup_ns[i],
        r == i64::MAX || exists|i: int| 0 <= i < member.len() && member[i] && r == next_wakeup_ns[i],
))]
pub fn domain_wakeup(member: &[bool], next_wakeup_ns: &[i64]) -> i64 {
    let mut earliest = i64::MAX;
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            0 <= i <= member.len(),
            member.len() == next_wakeup_ns.len(),
            forall|j: int| 0 <= j < i && member[j] ==> earliest <= next_wakeup_ns[j],
            earliest == i64::MAX || exists|j: int| 0 <= j < i && member[j] && earliest == next_wakeup_ns[j],
        decreases member.len() - i
    ))]
    while i < member.len() {
        if member[i] && next_wakeup_ns[i] < earliest {
            earliest = next_wakeup_ns[i];
        }
        i += 1;
    }
    earliest
}

/// Whether `state` fits an idle period of `idle_ns` and the latency limit `limit_ns`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r == (idle_ns as int >= state.residency_ns as int + state.power_off_latency_ns as int
        && limit_ns as int >= state.power_on_latency_ns as int + state.power_off_latency_ns as int),
))]
pub fn state_fits(state: DomainState, idle_ns: i64, limit_ns: i64) -> bool {
    (idle_ns as i128) >= (state.residency_ns as i128) + (state.power_off_latency_ns as i128)
        && (limit_ns as i128) >= (state.power_on_latency_ns as i128) + (state.power_off_latency_ns as i128)
}

/// The cluster state to take (genpd's `cpu_power_down_ok`), or `None` to keep the cluster up: the deepest state that fits
/// the time from `now_ns` to the members' earliest wake-up and the latency limit. No state when that time is not positive,
/// or when an inter-processor interrupt is pending for a member.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires states.len() > 0,
    ensures
        (ipi_pending || idle_ns <= 0) ==> r.is_none(),
        r.is_some() ==> r.unwrap() < states.len()
            && state_fits_spec(states[r.unwrap() as int], idle_ns, limit_ns)
            && forall|j: int| r.unwrap() < j < states.len() ==> !state_fits_spec(states[j], idle_ns, limit_ns),
        (r.is_none() && !ipi_pending && idle_ns > 0) ==>
            forall|j: int| 0 <= j < states.len() ==> !state_fits_spec(states[j], idle_ns, limit_ns),
))]
pub fn choose(states: &[DomainState], idle_ns: i64, limit_ns: i64, ipi_pending: bool) -> Option<usize> {
    // genpd looks for a state first and for IPIs after; the answer is the same.
    if idle_ns <= 0 || ipi_pending {
        return None;
    }
    let mut i = states.len();
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            0 <= i <= states.len(),
            !ipi_pending,
            idle_ns > 0,
            forall|j: int| i <= j < states.len() ==> !state_fits_spec(states[j], idle_ns, limit_ns),
        decreases i
    ))]
    while i > 0 {
        i -= 1;
        if state_fits(states[i], idle_ns, limit_ns) {
            return Some(i);
        }
    }
    None
}

#[cfg(verus_keep_ghost)]
verus! {
    /// [`state_fits`] as a specification.
    pub open spec fn state_fits_spec(state: DomainState, idle_ns: i64, limit_ns: i64) -> bool {
        idle_ns as int >= state.residency_ns as int + state.power_off_latency_ns as int
            && limit_ns as int >= state.power_on_latency_ns as int + state.power_off_latency_ns as int
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The laptop's `domain-idle-states` for `power-domain-cluster`: `cluster-l3-power-collapse` (6118 / 2752 / 3048 µs),
    /// `cluster-cx-retention` (8467 / 3638 / 4562 µs), `cluster-power-down` (9826 / 3263 / 6562 µs).
    const CLUSTER: [DomainState; 3] = [
        DomainState { residency_ns: 6_118_000, power_off_latency_ns: 2_752_000, power_on_latency_ns: 3_048_000 },
        DomainState { residency_ns: 8_467_000, power_off_latency_ns: 3_638_000, power_on_latency_ns: 4_562_000 },
        DomainState { residency_ns: 9_826_000, power_off_latency_ns: 3_263_000, power_on_latency_ns: 6_562_000 },
    ];
    const FREE: i64 = i64::MAX;

    #[test]
    fn only_the_last_cpu_down_may_take_the_cluster() {
        let mut c = Cluster::new();
        for cpu in 0..8 {
            c.add(cpu);
        }
        for cpu in 0..7 {
            assert!(!c.cpu_down(cpu));
        }
        assert!(c.cpu_down(7));
        c.cpu_up(3);
        assert!(!c.all_down());
        assert!(c.cpu_down(3));
    }

    #[test]
    fn a_cpu_that_never_came_online_does_not_hold_the_cluster_up() {
        let mut c = Cluster::new();
        c.add(0);
        c.add(1);
        assert!(!c.cpu_down(0));
        assert!(c.cpu_down(1));
    }

    #[test]
    fn the_deepest_state_that_fits_wins() {
        // 9826 + 3263 = 13.089 ms is the deepest's need.
        assert_eq!(choose(&CLUSTER, 13_089_000, FREE, false), Some(2));
        assert_eq!(choose(&CLUSTER, 13_088_999, FREE, false), Some(1));
        // 8467 + 3638 = 12.105 ms.
        assert_eq!(choose(&CLUSTER, 12_104_999, FREE, false), Some(0));
        // 6118 + 2752 = 8.870 ms.
        assert_eq!(choose(&CLUSTER, 8_869_999, FREE, false), None);
    }

    #[test]
    fn latency_and_ipis_and_running_cpus_keep_it_up() {
        // Entry + exit: 5.8 ms, 8.2 ms, 9.825 ms.
        assert_eq!(choose(&CLUSTER, 100_000_000, 9_000_000, false), Some(1));
        assert_eq!(choose(&CLUSTER, 100_000_000, 5_799_999, false), None);
        assert_eq!(choose(&CLUSTER, 100_000_000, FREE, true), None);
        assert_eq!(choose(&CLUSTER, 0, FREE, false), None);
    }

    #[test]
    fn the_earliest_member_wakeup_counts() {
        let member = [true, true, false, true];
        assert_eq!(domain_wakeup(&member, &[50, 20, 1, 30]), 20);
        // A running member (0) keeps the cluster up: its wake-up is now.
        assert_eq!(domain_wakeup(&member, &[50, 0, 1, 30]), 0);
        assert_eq!(domain_wakeup(&[false; 4], &[1, 2, 3, 4]), i64::MAX);
    }

    fn two_members() -> Cluster {
        let mut c = Cluster::new();
        c.add(0);
        c.add(1);
        c
    }

    #[test]
    fn cluster_off_refuses_while_a_member_runs() {
        let mut c = two_members();
        c.cpu_down(0);
        assert_eq!(c.cluster_off(&CLUSTER, FREE, FREE, false), None);
    }

    #[test]
    fn cluster_off_takes_the_deepest_state_once_all_are_down() {
        let mut c = two_members();
        c.cpu_down(0);
        c.cpu_down(1);
        assert_eq!(c.cluster_off(&CLUSTER, FREE, FREE, false), Some(2));
    }

    #[test]
    fn cluster_off_refuses_again_after_a_member_wakes() {
        let mut c = two_members();
        c.cpu_down(0);
        c.cpu_down(1);
        c.cpu_up(1);
        assert_eq!(c.cluster_off(&CLUSTER, FREE, FREE, false), None);
    }

    #[test]
    fn cluster_off_ignores_cpus_that_are_not_members() {
        let mut c = two_members();
        c.cpu_down(0);
        c.cpu_down(1);
        // CPU 5 was never added: it does not keep this cluster up.
        assert_eq!(c.cluster_off(&CLUSTER, FREE, FREE, false), Some(2));
    }

    #[test]
    fn cluster_off_refuses_with_an_ipi_pending() {
        let mut c = two_members();
        c.cpu_down(0);
        c.cpu_down(1);
        assert_eq!(c.cluster_off(&CLUSTER, FREE, FREE, true), None);
    }

    #[test]
    fn cluster_off_takes_only_what_fits() {
        let mut c = two_members();
        c.cpu_down(0);
        c.cpu_down(1);
        // 9 ms covers L3 collapse (6118 + 2752 µs) but not CX retention (8467 + 3638 µs).
        assert_eq!(c.cluster_off(&CLUSTER, 9_000_000, FREE, false), Some(0));
        assert_eq!(c.cluster_off(&CLUSTER, 8_000_000, FREE, false), None);
        // A 6 ms latency limit allows L3 collapse (3048 + 2752 µs) only.
        assert_eq!(c.cluster_off(&CLUSTER, FREE, 6_000_000, false), Some(0));
    }
}
