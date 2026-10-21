//! Types of the names a `range` clause declares.

use super::Ty;
use super::site::{Ctx, Eval, uni};

/// Predeclared integer types: `range n` yields the type of `n`.
fn is_integer(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "int8"
            | "int16"
            | "int32"
            | "int64"
            | "uint"
            | "uint8"
            | "uint16"
            | "uint32"
            | "uint64"
            | "uintptr"
            | "byte"
            | "rune"
    )
}

impl Eval<'_> {
    /// The type of the key (`key`) or the value of `for k, v := range text`.
    pub(super) fn range_ty(&mut self, site: &Ctx, text: &str, key: bool) -> Ty {
        let operand = self.eval_text_in(site, text);
        let (k, v) = self.range_parts(&operand);
        if key { k } else { v }
    }

    /// Key and value types of ranging over `operand`.
    fn range_parts(&mut self, operand: &Ty) -> (Ty, Ty) {
        match self.under(operand) {
            Ty::Slice(elem) | Ty::Array(elem) => (uni("int"), *elem),
            Ty::Pointer(inner) => match self.under(&inner) {
                Ty::Array(elem) => (uni("int"), *elem),
                _ => (Ty::Unknown, Ty::Unknown),
            },
            Ty::Map(k, v) => (*k, *v),
            Ty::Chan(elem) => (*elem, Ty::Unknown),
            Ty::Named { pkg, name, .. } if pkg.is_empty() && name == "string" => {
                (uni("int"), uni("rune"))
            }
            Ty::Named { pkg, name, .. } if pkg.is_empty() && is_integer(&name) => {
                (operand.clone(), Ty::Unknown)
            }
            Ty::Func { params, .. } => match params.first() {
                Some(Ty::Func {
                    params: yielded, ..
                }) => match yielded.as_slice() {
                    [] => (Ty::Unknown, Ty::Unknown),
                    [one] => (one.clone(), Ty::Unknown),
                    [k, v, ..] => (k.clone(), v.clone()),
                },
                _ => (Ty::Unknown, Ty::Unknown),
            },
            _ => (Ty::Unknown, Ty::Unknown),
        }
    }
}
