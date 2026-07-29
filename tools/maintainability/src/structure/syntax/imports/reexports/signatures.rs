use std::collections::BTreeSet;

use anyhow::Result;
use quote::ToTokens as _;
use syn::visit::{self, Visit};
use syn::{Generics, Path, Signature, TraitBound, Type, TypePath, Visibility};

use crate::scan::syntax_fingerprint;

use super::super::super::{ProductionCfgContext, ProductionSourceRevision, normalized_ident};
use super::super::concrete::without_documentation;
use super::super::resolution::resolve_path;
use super::{PendingPublicReexport, PublicReexportEvidence};

pub(in crate::structure::syntax::imports) struct PublicTypeExposureContext<'a> {
    pub(in crate::structure::syntax::imports) boundary_kind: &'a str,
    pub(in crate::structure::syntax::imports) exported_path: &'a [String],
    pub(in crate::structure::syntax::imports) source_path: Option<&'a [String]>,
    pub(in crate::structure::syntax::imports) required_trait_path: Option<&'a [String]>,
    pub(in crate::structure::syntax::imports) module: &'a [String],
    pub(in crate::structure::syntax::imports) visibility: &'a Visibility,
    pub(in crate::structure::syntax::imports) cfg: &'a ProductionCfgContext,
    pub(in crate::structure::syntax::imports) direct_exposure_cfg: Option<ProductionCfgContext>,
    pub(in crate::structure::syntax::imports) ancestors: &'a str,
    pub(in crate::structure::syntax::imports) rust_2015_absolute_paths: bool,
    pub(in crate::structure::syntax::imports) source_revision: ProductionSourceRevision,
}

pub(in crate::structure::syntax::imports) fn public_signature_type_exposures(
    signature: &Signature,
    inherited_generic_types: &BTreeSet<String>,
    context: &PublicTypeExposureContext<'_>,
) -> Result<Vec<PendingPublicReexport>> {
    let mut generic_types = inherited_generic_types.clone();
    generic_types.extend(signature.generics.type_params().map(|parameter| normalized_ident(&parameter.ident)));
    let fingerprint = syntax_fingerprint(&without_documentation(&signature.to_token_stream()));
    collect_type_exposures(generic_types, &fingerprint, "signature", context, |collector| {
        collector.visit_signature(signature);
    })
}

pub(in crate::structure::syntax::imports) fn public_type_exposures(
    ty: &Type,
    generic_types: &BTreeSet<String>,
    context: &PublicTypeExposureContext<'_>,
) -> Result<Vec<PendingPublicReexport>> {
    let fingerprint = syntax_fingerprint(&without_documentation(&ty.to_token_stream()));
    collect_type_exposures(generic_types.clone(), &fingerprint, "type", context, |collector| {
        collector.visit_type(ty);
    })
}

pub(in crate::structure::syntax::imports) fn public_path_argument_type_exposures(
    path: &Path,
    generic_types: &BTreeSet<String>,
    context: &PublicTypeExposureContext<'_>,
) -> Result<Vec<PendingPublicReexport>> {
    let fingerprint = syntax_fingerprint(&without_documentation(&path.to_token_stream()));
    collect_type_exposures(generic_types.clone(), &fingerprint, "path-arguments", context, |collector| {
        for segment in &path.segments {
            collector.visit_path_arguments(&segment.arguments);
        }
    })
}

pub(in crate::structure::syntax::imports) fn public_generic_default_type_exposures(
    generics: &Generics,
    inherited_generic_types: &BTreeSet<String>,
    context: &PublicTypeExposureContext<'_>,
) -> Result<Vec<PendingPublicReexport>> {
    let mut generic_types = inherited_generic_types.clone();
    generic_types.extend(generics.type_params().map(|parameter| normalized_ident(&parameter.ident)));
    let fingerprint = syntax_fingerprint(&without_documentation(&generics.to_token_stream()));
    collect_type_exposures(generic_types, &fingerprint, "generic-defaults", context, |collector| {
        for default in generics.type_params().filter_map(|parameter| parameter.default.as_ref()) {
            collector.visit_type(default);
        }
    })
}

fn collect_type_exposures(
    generic_types: BTreeSet<String>,
    boundary_fingerprint: &str,
    syntax_kind: &str,
    context: &PublicTypeExposureContext<'_>,
    visit: impl FnOnce(&mut SignatureTypeCollector<'_>),
) -> Result<Vec<PendingPublicReexport>> {
    let mut collector = SignatureTypeCollector {
        module: context.module,
        generic_types,
        targets: BTreeSet::new(),
        rust_2015_absolute_paths: context.rust_2015_absolute_paths,
        source_revision: context.source_revision,
        error: None,
    };
    visit(&mut collector);
    if let Some(error) = collector.error {
        return Err(error);
    }

    Ok(collector
        .targets
        .into_iter()
        .map(|target_path| {
            let identity = format!(
                "public-signature-type:{}\0boundary:{}\0{syntax_kind}:{}\0target:{}\0visibility:{}\0cfg:{}\0ancestors:{}",
                context.boundary_kind,
                context.exported_path.join("::"),
                boundary_fingerprint,
                target_path.join("::"),
                syntax_fingerprint(context.visibility),
                context.cfg.identity(),
                context.ancestors,
            );
            PendingPublicReexport {
                evidence: PublicReexportEvidence {
                    exported_path: context.exported_path.to_vec(),
                    target_path,
                    fingerprint: syntax_fingerprint(&identity),
                    cfg: context.cfg.clone(),
                    direct_exposure_cfg: context.direct_exposure_cfg.clone(),
                    required_trait_path: context.required_trait_path.map(<[String]>::to_vec),
                },
                cfg: context.cfg.clone(),
                source_path: context.source_path.map(<[String]>::to_vec),
            }
        })
        .collect())
}

struct SignatureTypeCollector<'a> {
    module: &'a [String],
    generic_types: BTreeSet<String>,
    targets: BTreeSet<Vec<String>>,
    rust_2015_absolute_paths: bool,
    source_revision: ProductionSourceRevision,
    error: Option<anyhow::Error>,
}

impl SignatureTypeCollector<'_> {
    fn record_path(&mut self, path: &Path) {
        if self.error.is_some() || path.leading_colon.is_some() && !self.rust_2015_absolute_paths {
            return;
        }
        let mut segments = path.segments.iter().map(|segment| normalized_ident(&segment.ident)).collect::<Vec<_>>();
        if path.leading_colon.is_some() {
            segments.insert(0, "crate".to_owned());
        }
        let Some(first) = segments.first() else {
            return;
        };
        if first == "Self" || self.generic_types.contains(first) {
            return;
        }
        match resolve_path(self.module, &segments, false) {
            Ok(Some(target)) => {
                self.targets.insert(target);
            }
            Ok(None) => {}
            Err(error) => self.error = Some(error),
        }
    }
}

impl<'ast> Visit<'ast> for SignatureTypeCollector<'_> {
    fn visit_type_path(&mut self, path: &'ast TypePath) {
        if path.qself.is_some() {
            if self.source_revision == ProductionSourceRevision::Historical {
                // Historical evidence is only a ceiling for current exposure.
                // Omitting an unresolvable historical edge cannot authorize a
                // new current edge; current source still fails closed below.
                return;
            }
            self.error = Some(anyhow::anyhow!(
                "exposed qualified signature type `{}` cannot be resolved to a concrete target",
                path.to_token_stream()
            ));
            return;
        }
        self.record_path(&path.path);
        visit::visit_type_path(self, path);
    }

    fn visit_trait_bound(&mut self, bound: &'ast TraitBound) {
        self.record_path(&bound.path);
        visit::visit_trait_bound(self, bound);
    }
}
