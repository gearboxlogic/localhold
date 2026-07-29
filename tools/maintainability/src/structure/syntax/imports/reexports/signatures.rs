use std::collections::BTreeSet;

use anyhow::Result;
use quote::ToTokens as _;
use syn::visit::{self, Visit};
use syn::{Path, Signature, TraitBound, TypePath, Visibility};

use crate::scan::syntax_fingerprint;

use super::super::super::{ProductionCfgContext, normalized_ident};
use super::super::concrete::without_documentation;
use super::super::resolution::resolve_path;
use super::{PendingPublicReexport, PublicReexportEvidence};

pub(in crate::structure::syntax::imports) struct PublicSignatureExposureContext<'a> {
    pub(in crate::structure::syntax::imports) boundary_kind: &'a str,
    pub(in crate::structure::syntax::imports) exported_path: &'a [String],
    pub(in crate::structure::syntax::imports) module: &'a [String],
    pub(in crate::structure::syntax::imports) visibility: &'a Visibility,
    pub(in crate::structure::syntax::imports) cfg: &'a ProductionCfgContext,
    pub(in crate::structure::syntax::imports) direct_exposure_cfg: Option<ProductionCfgContext>,
    pub(in crate::structure::syntax::imports) ancestors: &'a str,
    pub(in crate::structure::syntax::imports) rust_2015_absolute_paths: bool,
}

pub(in crate::structure::syntax::imports) fn public_signature_type_exposures(
    signature: &Signature,
    context: &PublicSignatureExposureContext<'_>,
) -> Result<Vec<PendingPublicReexport>> {
    let generic_types = signature.generics.type_params().map(|parameter| normalized_ident(&parameter.ident)).collect();
    let mut collector = SignatureTypeCollector {
        module: context.module,
        generic_types,
        targets: BTreeSet::new(),
        rust_2015_absolute_paths: context.rust_2015_absolute_paths,
        error: None,
    };
    collector.visit_signature(signature);
    if let Some(error) = collector.error {
        return Err(error);
    }

    let signature_fingerprint = syntax_fingerprint(&without_documentation(&signature.to_token_stream()));
    Ok(collector
        .targets
        .into_iter()
        .map(|target_path| {
            let identity = format!(
                "public-signature-type:{}\0boundary:{}\0signature:{}\0target:{}\0visibility:{}\0cfg:{}\0ancestors:{}",
                context.boundary_kind,
                context.exported_path.join("::"),
                signature_fingerprint,
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
                },
                cfg: context.cfg.clone(),
            }
        })
        .collect())
}

struct SignatureTypeCollector<'a> {
    module: &'a [String],
    generic_types: BTreeSet<String>,
    targets: BTreeSet<Vec<String>>,
    rust_2015_absolute_paths: bool,
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
        if path.qself.is_none() {
            self.record_path(&path.path);
        }
        visit::visit_type_path(self, path);
    }

    fn visit_trait_bound(&mut self, bound: &'ast TraitBound) {
        self.record_path(&bound.path);
        visit::visit_trait_bound(self, bound);
    }
}
