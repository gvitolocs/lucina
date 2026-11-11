//! `lucina-devd`: Lucina's own tools on Juno (design: `docs/research/juno-lucina-tools.md`).
//!
//! The service dials out to her server with the device token that `lucina-accountd` holds, long-polls for her calls and
//! runs them here. No port listens.
//!
//! * [`poll`]: the loop, the wire protocol and the deadlines; [`audit`]: one line per call.
//! * [`tools`]: the driver tools, their schemas and the checks on their arguments.
//! * [`screen`], [`status`], [`files`] (with [`places`], what they may reach), [`bash`] (as the signed-in user, root on Juno),
//!   [`shellctl`] (apps and notices, through the shell's socket); input is `lucina-inject`.
//! * [`offlimits`]: what no tool touches (UFS LUNs 1-5, the Qualcomm firmware, the charge limits, the sign-in's folders);
//!   [`redact`]: credential values out of `read_file` and `bash` output; [`account`]: the sign-in, followed.
//! * [`system`]: `generation_install`, `reload`, `reboot`, `poweroff`; [`events`]: `lucina-eventd` (the `events` tool,
//!   `wait` on its events, and the forwarder to the server).
//!
//! The tool registry is one list, `tools::TOOLS` (`ToolSpec {name, description, input_schema, available}`), with one dispatch
//! (`tools::parse` then `tools::run`); a later stage adds its entries there and its code in a module of its own.
// The tools test builds one json! literal with every tool in it.
#![recursion_limit = "256"]

pub mod account;
pub mod audit;
pub mod b64;
pub mod bash;
pub mod events;
pub mod files;
pub mod offlimits;
pub mod places;
pub mod poll;
pub mod redact;
pub mod reliability;
pub mod screen;
pub mod shellctl;
pub mod status;
pub mod system;
pub mod tools;
