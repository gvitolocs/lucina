//! The PM6150's power-on (PON) registers: the decoder is shared with the kernel's crash record, in lucina-pon
//! (kernel/lucina-pon), so that both read them alike.

pub use lucina_pon::*;
