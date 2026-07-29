use serde::Serialize;
use syn::Visibility;

use crate::scan::syntax_fingerprint;

use super::super::{ProductionCfgContext, normalized_ident};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::structure) enum TypeDeclarationKind {
    Type,
    Trait,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TypeDeclarationEvidence {
    pub item_path: Vec<String>,
    pub fingerprint: String,
    #[serde(skip)]
    pub(in crate::structure) cfg: ProductionCfgContext,
    #[serde(skip)]
    pub(in crate::structure) kind: TypeDeclarationKind,
    #[serde(skip)]
    pub(in crate::structure) direct_exposure_cfg: Option<ProductionCfgContext>,
}

pub(super) struct TypeDeclarationContext<'a> {
    pub(super) module: &'a [String],
    pub(super) cfg: &'a ProductionCfgContext,
    pub(super) ancestors: &'a str,
    pub(super) direct_exposure_cfg: Option<ProductionCfgContext>,
}

pub(super) fn type_declaration_evidence(
    kind: TypeDeclarationKind,
    syntax_kind: &str,
    ident: &proc_macro2::Ident,
    visibility: &Visibility,
    context: &TypeDeclarationContext<'_>,
) -> TypeDeclarationEvidence {
    let mut item_path = context.module.to_vec();
    item_path.push(normalized_ident(ident));
    let identity = format!(
        "type-declaration:{syntax_kind}:{}\0visibility:{}\0cfg:{}\0ancestors:{}",
        item_path.join("::"),
        syntax_fingerprint(visibility),
        context.cfg.identity(),
        context.ancestors,
    );
    TypeDeclarationEvidence {
        item_path,
        fingerprint: syntax_fingerprint(&identity),
        cfg: context.cfg.clone(),
        kind,
        direct_exposure_cfg: context.direct_exposure_cfg.clone(),
    }
}
