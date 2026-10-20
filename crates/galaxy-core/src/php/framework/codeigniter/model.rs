//! CodeIgniter's run-time call forwarding, which no declaration shows: a
//! model passes calls it does not declare to a query builder through
//! `__call`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::php::engine::PhpEngine;
use crate::php::parse::{ClassInfo, Member, Visibility};

const MODEL: &str = "CodeIgniter\\Model";
const BUILDER: &str = "CodeIgniter\\Database\\BaseBuilder";

/// `$model->where()` and friends: a model forwards calls it does not
/// declare to its builder. Only public builder methods can be reached.
pub(super) fn forwarded_method(
    engine: &mut PhpEngine,
    class: &Arc<ClassInfo>,
    name: &str,
) -> Option<(Arc<ClassInfo>, Member)> {
    if !engine.lineage(class).iter().any(|c| c.fqn == MODEL) {
        return None;
    }
    let builder = engine.class(BUILDER)?;
    engine
        .find_member(&builder, name, true)
        .filter(|(_, member)| member.visibility == Visibility::Public)
}

/// The builder that calls a model does not declare are forwarded to, when
/// `chain` is one of a model.
pub(super) fn forwarding_builder(
    engine: &mut PhpEngine,
    chain: &[(Arc<ClassInfo>, HashMap<String, String>)],
) -> Option<String> {
    if !chain.iter().any(|(c, _)| c.fqn == MODEL) {
        return None;
    }
    engine.class(BUILDER)?;
    Some(BUILDER.to_owned())
}
