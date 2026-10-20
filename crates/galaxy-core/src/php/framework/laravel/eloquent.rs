//! Eloquent's run-time call forwarding, which no declaration shows: a model
//! passes calls it does not declare to a query builder through `__call` and
//! `__callStatic`, and so do relations.

use std::collections::HashMap;
use std::sync::Arc;

use crate::php::engine::PhpEngine;
use crate::php::parse::{ClassInfo, Member};

const MODEL: &str = "Illuminate\\Database\\Eloquent\\Model";
const RELATION: &str = "Illuminate\\Database\\Eloquent\\Relations\\Relation";
const BUILDER: &str = "Illuminate\\Database\\Eloquent\\Builder";

/// `Model::create()` and friends: Eloquent models forward calls they do
/// not declare to a query builder through `__call` and `__callStatic`.
pub(super) fn forwarded_method(
    engine: &mut PhpEngine,
    class: &Arc<ClassInfo>,
    name: &str,
) -> Option<(Arc<ClassInfo>, Member)> {
    if !engine.lineage(class).iter().any(|c| c.fqn == MODEL) {
        return None;
    }
    let builder = engine.class(BUILDER)?;
    engine.find_member(&builder, name, true)
}

/// The `Builder<Model>` type that calls a model or relation does not
/// declare are forwarded to, when `chain` is one of those.
pub(super) fn forwarding_builder(
    engine: &mut PhpEngine,
    chain: &[(Arc<ClassInfo>, HashMap<String, String>)],
) -> Option<String> {
    engine.class(BUILDER)?;
    let model = if chain.iter().any(|(c, _)| c.fqn == MODEL) {
        chain[0].0.fqn.clone()
    } else {
        let (_, env) = chain.iter().find(|(c, _)| c.fqn == RELATION)?;
        env.get("TRelatedModel").filter(|m| !m.is_empty())?.clone()
    };
    Some(format!("{BUILDER}<{model}>"))
}
