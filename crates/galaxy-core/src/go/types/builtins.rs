//! Calls of the predeclared functions: `make`, `new`, `append`, `len`, ...

use tree_sitter::Node;

use super::Ty;
use super::eval::is_literal;
use super::site::{Ctx, Eval, kids, text, uni};
use crate::go::engine::FileSymbol;

/// The arguments of a call, without comments.
pub(super) fn call_args<'t>(call: Node<'t>) -> Vec<Node<'t>> {
    match call.child_by_field_name("arguments") {
        Some(list) => kids(list),
        None => Vec::new(),
    }
}

impl Eval<'_> {
    /// The result of `function(args)` when `function` is a predeclared
    /// function that no local or package-level name hides.
    pub(super) fn builtin_call(
        &mut self,
        ctx: &Ctx,
        src: &str,
        call: Node<'_>,
        function: Node<'_>,
    ) -> Option<Ty> {
        if function.kind() != "identifier" {
            return None;
        }
        let name = text(src, function);
        if ctx.scope.get(name).is_some() {
            return None;
        }
        if !matches!(
            self.engine.lookup_in_file_scope(&ctx.file, &ctx.pkg, name),
            Some(FileSymbol::Builtin)
        ) {
            return None;
        }
        let args = call_args(call);
        let out = match name {
            "make" => self.type_arg(ctx, src, args.first()?),
            "new" => match self.type_arg(ctx, src, args.first()?) {
                Ty::Unknown => Ty::Unknown,
                inner => Ty::Pointer(Box::new(inner)),
            },
            "append" => self.eval_node(ctx, src, *args.first()?),
            "len" | "cap" | "copy" => uni("int"),
            "delete" | "panic" | "print" | "println" | "close" | "clear" => Ty::Tuple(Vec::new()),
            "min" | "max" => self.first_typed(ctx, src, &args),
            "recover" => uni("any"),
            "complex" => uni("complex128"),
            "real" | "imag" => uni("float64"),
            _ => return None,
        };
        Some(out)
    }

    /// The type an argument of `make` or `new` names.
    fn type_arg(&mut self, ctx: &Ctx, src: &str, node: &Node<'_>) -> Ty {
        self.as_type(ctx, src, *node).unwrap_or(Ty::Unknown)
    }

    /// The type of the first argument that is not an untyped literal, or of
    /// the first one when all are.
    fn first_typed(&mut self, ctx: &Ctx, src: &str, args: &[Node<'_>]) -> Ty {
        let mut first = None;
        for arg in args {
            let ty = self.eval_node(ctx, src, *arg);
            if !is_literal(arg.kind()) && !ty.is_unknown() {
                return ty;
            }
            first.get_or_insert(ty);
        }
        first.unwrap_or(Ty::Unknown)
    }
}
