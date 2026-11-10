//! Host tests. `support` has the RAM both sides share; `model` has the modem-side model and the scenarios;
//! `golden` pins the byte layouts to the C; `fifo` tests the ring; `fuzz` feeds garbage.

mod fifo;
mod fuzz;
mod golden;
mod model;
mod support;
