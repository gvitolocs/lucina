//! A field's value, and the one way to log something secret.

use std::fmt;

/// What a log line prints in place of a secret.
pub const REDACTED_TEXT: &str = "<redacted>";

/// A secret's stand-in. It holds nothing: [`redacted`] looks at the secret's address and forgets it, so there is no way to get the
/// content back out of a `Redacted`, to print it, to format it or to send it. Only [`redacted`] makes one.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Redacted {
    _private: (),
}

/// The name used in field lists: `key = secret(&x)` reads the same as `redacted(&x)`.
pub type Secret = Redacted;

/// Marks a value as secret. The result prints as `"<redacted>"` everywhere; the value itself is not kept.
pub fn redacted<T: ?Sized>(_secret: &T) -> Redacted {
    Redacted { _private: () }
}

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED_TEXT)
    }
}

impl fmt::Display for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED_TEXT)
    }
}

/// A field's value: a string, an integer, a bool, or a secret that prints as `"<redacted>"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Int(i64),
    UInt(u64),
    Bool(bool),
    Redacted,
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Str(v.to_owned())
    }
}
impl From<&String> for Value {
    fn from(v: &String) -> Self {
        Value::Str(v.clone())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Str(v)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<Redacted> for Value {
    fn from(_: Redacted) -> Self {
        Value::Redacted
    }
}

macro_rules! from_int {
    ($variant:ident, $wide:ty: $($t:ty),*) => {$(
        impl From<$t> for Value {
            fn from(v: $t) -> Self {
                Value::$variant(<$wide>::from(v))
            }
        }
    )*};
}
from_int!(Int, i64: i8, i16, i32, i64);
from_int!(UInt, u64: u8, u16, u32, u64);

impl From<isize> for Value {
    fn from(v: isize) -> Self {
        Value::Int(v as i64)
    }
}
impl From<usize> for Value {
    fn from(v: usize) -> Self {
        Value::UInt(v as u64)
    }
}
