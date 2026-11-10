//! The device tree walker for [`Consumer`](crate::share::Consumer): every enabled node that holds a power domain of the
//! SC7180 RPMh power domain provider, with the highest performance state it can ask for. This is what Linux's genpd sees
//! on the same tree.
//!
//! The rules follow Linux 7.1. `drivers/pmdomain/core.c` attaches a device to a power domain by reading its
//! `power-domains` property as (phandle, argument) entries, the argument count being the provider's `#power-domain-cells`
//! (`genpd_dev_pm_attach_by_id`, `of_genpd_add_provider_onecell`); a node whose `status` is not `okay` is not attached, and
//! neither are its descendants. A clock controller's GDSCs are subdomains of the controller's own domain:
//! `drivers/clk/qcom/gdsc.c` registers each GDSC with `pm_genpd_add_subdomain` under the domain the controller node itself
//! lists in `power-domains`, so `<&gcc USB30_PRIM_GDSC>` lands in the domain gcc holds, one level of indirection. The
//! remote processors and the video codec vote for themselves through their own drivers, so they are left out here. The
//! performance state is the `opp-level` of the nodes a `required-opps` list points at, or of the children of the
//! `operating-points-v2` table (`drivers/opp/of.c`); the highest of the table is used because no driver here follows the
//! clock, so the highest is never below what Linux would vote at any moment.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use fdt::node::FdtNode;

use crate::share::Consumer;

/// The RPMh power domain provider on SC7180.
const RPMHPD: &str = "qcom,sc7180-rpmhpd";

/// Nodes whose drivers vote for their own power domains, so the tree walk does not count them.
pub const SELF_VOTING: [&str; 3] = ["qcom,sc7180-mpss-pas", "qcom,sc7180-adsp-pas", "qcom,sc7180-venus"];

/// Every enabled node holding a domain of the RPMh power domain provider, in walk order (depth first, children in tree
/// order), one entry per (node, distinct domain), the domains in the order found.
pub fn consumers(dt: &fdt::Fdt) -> Vec<Consumer> {
    let mut out = Vec::new();
    let Some(provider) = dt.find_compatible(&[RPMHPD]) else {
        return out;
    };
    let Some(phandle) = provider.property("phandle").and_then(|p| cell(p.value)) else {
        return out;
    };
    if let Some(root) = dt.find_node("/") {
        walk(dt, root, "/", phandle, &mut out);
    }
    out
}

/// One big-endian u32 cell, `None` when the property is shorter than a cell.
fn cell(value: &[u8]) -> Option<u32> {
    (value.len() >= 4).then(|| u32::from_be_bytes([value[0], value[1], value[2], value[3]]))
}

/// The property value as big-endian u32 cells; a trailing odd byte is dropped.
fn cells(value: &[u8]) -> Vec<u32> {
    let mut out = Vec::with_capacity(value.len() / 4);
    let mut rest = value;
    while rest.len() >= 4 {
        out.push(u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]));
        rest = &rest[4..];
    }
    out
}

/// A node is enabled when `status` is absent, `okay` or `ok`.
fn enabled(node: FdtNode) -> bool {
    match node.property("status").and_then(|p| p.as_str()) {
        None => true,
        Some(s) => s == "okay" || s == "ok",
    }
}

fn self_voting(node: FdtNode) -> bool {
    node.compatible().is_some_and(|c| c.all().any(|s| SELF_VOTING.contains(&s)))
}

/// The distinct domains of the node's `power-domains` entries, in the order found.
fn domains(dt: &fdt::Fdt, node: FdtNode, rpmhpd: u32) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    let Some(pd) = node.property("power-domains") else {
        return out;
    };
    let cells = cells(pd.value);
    let mut i = 0;
    while i < cells.len() {
        let Some(provider) = dt.find_phandle(cells[i]) else {
            break;
        };
        let n = provider.property("#power-domain-cells").and_then(|p| cell(p.value)).unwrap_or(0) as usize;
        if i + 1 + n > cells.len() {
            break;
        }
        let domain = if cells[i] == rpmhpd {
            (n >= 1).then(|| cells[i + 1] as usize)
        } else {
            subdomain(dt, provider, rpmhpd)
        };
        if let Some(d) = domain
            && !out.contains(&d)
        {
            out.push(d);
        }
        i += 1 + n;
    }
    out
}

/// The domain a provider node itself holds from the RPMh provider, when its own `power-domains` starts with an entry for
/// it: a GDSC of a clock controller is a subdomain of the controller's domain.
fn subdomain(dt: &fdt::Fdt, provider: FdtNode, rpmhpd: u32) -> Option<usize> {
    let pd = provider.property("power-domains")?;
    let cells = cells(pd.value);
    if cells.first() != Some(&rpmhpd) || cells.len() < 2 {
        return None;
    }
    let n = dt
        .find_phandle(rpmhpd)?
        .property("#power-domain-cells")
        .and_then(|p| cell(p.value))
        .unwrap_or(0) as usize;
    (n >= 1).then(|| cells[1] as usize)
}

/// The highest `opp-level` the node can ask for: the maximum over its `required-opps` nodes and, when it has an
/// `operating-points-v2` table, over the `required-opps` of every child of that table.
fn state(dt: &fdt::Fdt, node: FdtNode) -> u32 {
    let mut max = 0;
    if let Some(ro) = node.property("required-opps") {
        max = max.max(required_opps(dt, ro.value));
    }
    if let Some(table) = node.property("operating-points-v2").and_then(|p| cell(p.value)).and_then(|ph| dt.find_phandle(ph)) {
        for child in table.children() {
            if let Some(ro) = child.property("required-opps") {
                max = max.max(required_opps(dt, ro.value));
            }
        }
    }
    max
}

/// The maximum `opp-level` of the nodes a `required-opps` list points at, 0 for a node without one.
fn required_opps(dt: &fdt::Fdt, value: &[u8]) -> u32 {
    cells(value)
        .into_iter()
        .filter_map(|ph| dt.find_phandle(ph))
        .filter_map(|opp| opp.property("opp-level").and_then(|p| cell(p.value)))
        .max()
        .unwrap_or(0)
}

fn walk(dt: &fdt::Fdt, node: FdtNode, path: &str, rpmhpd: u32, out: &mut Vec<Consumer>) {
    if !enabled(node) {
        return;
    }
    if !self_voting(node) {
        let domains = domains(dt, node, rpmhpd);
        if !domains.is_empty() {
            let state = state(dt, node);
            for d in domains {
                out.push(Consumer { node: String::from(path), domain: d, state });
            }
        }
    }
    for child in node.children() {
        let child_path = if path == "/" { format!("/{}", child.name) } else { format!("{path}/{}", child.name) };
        walk(dt, child, &child_path, rpmhpd, out);
    }
}
