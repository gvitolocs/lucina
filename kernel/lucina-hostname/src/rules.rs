// Which names the kernel takes from `lucina.hostname=`: 1 to 64 bytes (the UTS nodename field holds 65 with its NUL), only
// letters, digits and '-', and no '-' at either end (RFC 1123 host name labels, which DHCP option 12 and the FRITZ!Box's
// DNS expect). Verus checks this very file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011);
// verification/juno-hostname/mutants.sh plants bugs that Verus must refuse. lib.rs calls these functions.

//@ pub open spec fn allowed_spec(b: u8) -> bool { (0x30u8 <= b && b <= 0x39u8) || (0x41u8 <= b && b <= 0x5au8) || (0x61u8 <= b && b <= 0x7au8) || b == 0x2du8 }

/// The longest name: the UTS nodename field is 65 bytes with its NUL.
pub const MAX: usize = 64;
/// '-', allowed inside a name, not at either end.
pub const HYPHEN: u8 = 0x2d;

/// A byte a host name may hold: '0'-'9', 'A'-'Z', 'a'-'z' or '-'.
pub fn allowed(b: u8) -> bool //@ret
//@ ensures ret == allowed_spec(b),
{
    (0x30 <= b && b <= 0x39) || (0x41 <= b && b <= 0x5a) || (0x61 <= b && b <= 0x7a) || b == HYPHEN
}

/// Whether `name` is a host name the kernel takes: 1 to [`MAX`] allowed bytes, no '-' first or last.
pub fn valid(name: &[u8]) -> bool //@ret
//@ ensures
//@     // 64 written out, not MAX: the bound is the UTS field's, and MAX must not drift from it.
//@     ret == (1 <= name@.len() && name@.len() <= 64 && name@[0] != HYPHEN && name@[name@.len() - 1] != HYPHEN
//@         && forall|i: int| 0 <= i < name@.len() ==> allowed_spec(#[trigger] name@[i])),
{
    let n = name.len();
    if n == 0 || n > MAX {
        return false;
    }
    if name[0] == HYPHEN || name[n - 1] == HYPHEN {
        return false;
    }
    let mut i = 0;
    while i < n
    //@ invariant
    //@     n == name@.len(), 1 <= n <= MAX, 0 <= i <= n, name@[0] != HYPHEN, name@[n - 1] != HYPHEN,
    //@     forall|j: int| 0 <= j < i ==> allowed_spec(#[trigger] name@[j]),
    //@ decreases n - i,
    {
        if !allowed(name[i]) {
            return false;
        }
        i += 1;
    }
    true
}
