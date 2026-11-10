// The path check for `office.pkg`'s members. Verus checks this very file: `//@` lines hold the proof, see scripts/verify-shipped.sh.

//@ pub open spec fn starts_part(s: Seq<u8>, i: int) -> bool { i == 0 || s[i - 1] == 47u8 }
//@ pub open spec fn ends_part(s: Seq<u8>, i: int) -> bool { i == s.len() || s[i] == 47u8 }
//@ /// What a name that passes may not contain at byte k.
//@ pub open spec fn byte_ok(s: Seq<u8>, k: int) -> bool {
//@     &&& s[k] != 0u8 && s[k] != 92u8
//@     &&& (s[k] == 47u8 && k + 1 < s.len() ==> s[k + 1] != 47u8)
//@     &&& (starts_part(s, k) && s[k] == 46u8 ==> !ends_part(s, k + 1))
//@     &&& (starts_part(s, k) && s[k] == 46u8 && k + 1 < s.len() && s[k + 1] == 46u8 ==> !ends_part(s, k + 2))
//@ }

const MAX_MEMBER: usize = 512;

/// May `tar` write this member? Relative, down only, no surprises: no empty part, `.`, `..`, backslash or NUL. A folder may end in `/`.
pub fn member_path_ok(name: &[u8]) -> bool //@ret
//@ ensures ret ==> 0 < name@.len() <= 512 && name@[0] != 47u8 && forall|k: int| 0 <= k < name@.len() ==> byte_ok(name@, k),
{
    let n = name.len();
    if n == 0 || n > MAX_MEMBER || name[0] == b'/' {
        return false;
    }
    let mut i = 0;
    while i < n
    //@ invariant 0 <= i <= n, n == name@.len(), 0 < n <= 512, name@[0] != 47u8, forall|k: int| 0 <= k < i ==> byte_ok(name@, k),
    //@ decreases n - i,
    {
        let c = name[i];
        if c == 0 || c == b'\\' {
            return false;
        }
        if c == b'/' && i + 1 < n && name[i + 1] == b'/' {
            return false;
        }
        if c == b'.' && (i == 0 || name[i - 1] == b'/') {
            let one = i + 1 == n || name[i + 1] == b'/';
            let two = i + 1 < n && name[i + 1] == b'.' && (i + 2 == n || name[i + 2] == b'/');
            if one || two {
                return false;
            }
        }
        i += 1;
    }
    true
}
