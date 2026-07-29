use std::collections::BTreeSet;

use anyhow::Result;
use proc_macro2::{Ident, TokenStream};
use syn::{
    Field, ForeignItemStatic, Generics, ImplItemConst, ImplItemFn, ImplItemType, ItemConst, ItemImpl, ItemStatic, ItemTrait, ItemType, Signature, TraitBoundModifier,
    TraitItemConst, TraitItemType, TypeParamBound, Visibility,
};

use crate::scan::syntax_fingerprint;

use super::concrete::production_generics;
use super::reexports::{
    PendingPublicReexport, PublicTypeExposureContext, public_generic_default_type_exposures, public_path_argument_type_exposures, public_signature_type_exposures,
    public_type_exposures,
};
use super::{ProductionSyntaxCollector, PublicReexportEvidence, normalized_ident, visibility_is_exposed};

#[derive(Clone, Copy)]
struct TypeExposureBoundary<'a> {
    kind: &'a str,
    exported_path: &'a [String],
    source_path: Option<&'a [String]>,
    required_trait_path: Option<&'a [String]>,
    visibility: &'a Visibility,
}

fn self_supertrait_bounds(predicate: &syn::WherePredicate) -> Option<&syn::punctuated::Punctuated<TypeParamBound, syn::token::Plus>> {
    let syn::WherePredicate::Type(predicate) = predicate else {
        return None;
    };
    let syn::Type::Path(bounded_type) = &predicate.bounded_ty else {
        return None;
    };
    (bounded_type.qself.is_none() && bounded_type.path.is_ident("Self")).then_some(&predicate.bounds)
}

impl ProductionSyntaxCollector {
    pub(super) fn record_exposed_generic_default_types(&mut self, boundary_kind: &str, ident: &Ident, visibility: &Visibility, generics: &Generics) -> Result<()> {
        let generics = production_generics(generics, &self.cfg_context)?;
        let mut exported_path = self.module.clone();
        exported_path.push(normalized_ident(ident));
        let ancestors = self.declaration_ancestor_identity();
        let exposures = public_generic_default_type_exposures(
            &generics,
            &self.active_generic_types(),
            &PublicTypeExposureContext {
                boundary_kind,
                exported_path: &exported_path,
                source_path: Some(&exported_path),
                required_trait_path: None,
                module: &self.module,
                visibility,
                cfg: &self.cfg_context,
                direct_exposure_cfg: self.direct_exposure_cfg(),
                ancestors: &ancestors,
                rust_2015_absolute_paths: self.rust_2015_absolute_paths,
                source_revision: self.source_revision,
            },
        )?;
        self.public_reexports.extend(exposures);
        Ok(())
    }

    fn record_signature_type_exposures(&mut self, boundary: TypeExposureBoundary<'_>, signature: &Signature) -> Result<()> {
        let ancestors = self.declaration_ancestor_identity();
        let generic_types = self.active_generic_types();
        let exposures = public_signature_type_exposures(
            signature,
            &generic_types,
            &PublicTypeExposureContext {
                boundary_kind: boundary.kind,
                exported_path: boundary.exported_path,
                source_path: boundary.source_path,
                required_trait_path: boundary.required_trait_path,
                module: &self.module,
                visibility: boundary.visibility,
                cfg: &self.cfg_context,
                direct_exposure_cfg: self.direct_exposure_cfg(),
                ancestors: &ancestors,
                rust_2015_absolute_paths: self.rust_2015_absolute_paths,
                source_revision: self.source_revision,
            },
        )?;
        self.public_reexports.extend(exposures);
        Ok(())
    }

    fn record_type_exposures(&mut self, boundary: TypeExposureBoundary<'_>, ty: &syn::Type) -> Result<()> {
        let ancestors = self.declaration_ancestor_identity();
        let generic_types = self.active_generic_types();
        let exposures = public_type_exposures(
            ty,
            &generic_types,
            &PublicTypeExposureContext {
                boundary_kind: boundary.kind,
                exported_path: boundary.exported_path,
                source_path: boundary.source_path,
                required_trait_path: boundary.required_trait_path,
                module: &self.module,
                visibility: boundary.visibility,
                cfg: &self.cfg_context,
                direct_exposure_cfg: self.direct_exposure_cfg(),
                ancestors: &ancestors,
                rust_2015_absolute_paths: self.rust_2015_absolute_paths,
                source_revision: self.source_revision,
            },
        )?;
        self.public_reexports.extend(exposures);
        Ok(())
    }

    pub(super) fn record_exposed_function_signature(&mut self, boundary_kind: &str, visibility: &Visibility, signature: &Signature) -> Result<()> {
        let signature = self.production_signature(signature)?;
        let mut exported_path = self.module.clone();
        exported_path.push(normalized_ident(&signature.ident));
        self.record_signature_type_exposures(
            TypeExposureBoundary {
                kind: boundary_kind,
                exported_path: &exported_path,
                source_path: None,
                required_trait_path: None,
                visibility,
            },
            &signature,
        )?;
        self.record_concrete_stores_in_visible_signature(boundary_kind, visibility, &signature);
        Ok(())
    }

    pub(super) fn record_exposed_type_alias_exposures(&mut self, item: &ItemType, direct_target: Option<&[String]>) -> Result<()> {
        if !visibility_is_exposed(&item.vis) {
            return Ok(());
        }
        let mut exported_path = self.module.clone();
        exported_path.push(normalized_ident(&item.ident));
        let mut generic_types = self.active_generic_types();
        generic_types.extend(item.generics.type_params().map(|parameter| normalized_ident(&parameter.ident)));
        let ancestors = self.declaration_ancestor_identity();
        let exposures = public_type_exposures(
            &item.ty,
            &generic_types,
            &PublicTypeExposureContext {
                boundary_kind: "type-alias-target",
                exported_path: &exported_path,
                source_path: None,
                required_trait_path: None,
                module: &self.module,
                visibility: &item.vis,
                cfg: &self.cfg_context,
                direct_exposure_cfg: self.direct_exposure_cfg(),
                ancestors: &ancestors,
                rust_2015_absolute_paths: self.rust_2015_absolute_paths,
                source_revision: self.source_revision,
            },
        )?;
        self.public_reexports
            .extend(exposures.into_iter().filter(|exposure| direct_target != Some(exposure.evidence.target_path.as_slice())));
        Ok(())
    }

    fn record_exposed_method_signature(&mut self, boundary_kind: &str, visibility: &Visibility, signature: &Signature) -> Result<()> {
        let source_path = self.impl_item_paths.last().filter(|path| !path.is_empty()).cloned();
        let mut fallback_path = self.module.clone();
        fallback_path.push(normalized_ident(&signature.ident));
        let exported_path = source_path.as_deref().unwrap_or(&fallback_path);
        let required_trait_path = self.impl_trait_paths.last().cloned().flatten();
        self.record_signature_type_exposures(
            TypeExposureBoundary {
                kind: boundary_kind,
                exported_path,
                source_path: source_path.as_deref(),
                required_trait_path: required_trait_path.as_deref(),
                visibility,
            },
            signature,
        )
    }

    pub(super) fn record_impl_method_exposure(&mut self, item: &ImplItemFn) -> Result<()> {
        let signature = self.production_signature(&item.sig)?;
        self.record_impl_header_for_visible_member("inherent-impl-method", &item.vis, &signature);
        if self.impl_member_is_exposed(&item.vis) {
            self.record_exposed_method_signature("method-signature", &item.vis, &signature)?;
            self.record_concrete_stores_in_visible_signature("method-signature", &item.vis, &signature);
        }
        Ok(())
    }

    pub(super) fn record_exposed_trait_signature(&mut self, signature: &Signature) -> Result<()> {
        let signature = self.production_signature(signature)?;
        let source_path = self.signature_item_path();
        let visibility = Visibility::Inherited;
        self.record_signature_type_exposures(
            TypeExposureBoundary {
                kind: "trait-method-signature",
                exported_path: &source_path,
                source_path: (!source_path.is_empty()).then_some(source_path.as_slice()),
                required_trait_path: None,
                visibility: &visibility,
            },
            &signature,
        )?;
        self.record_concrete_stores_in_signature("trait-method-signature", &signature);
        Ok(())
    }

    pub(super) fn record_exposed_supertraits(&mut self, item: &ItemTrait) -> Result<()> {
        let mut source_path = self.module.clone();
        source_path.push(normalized_ident(&item.ident));
        let boundary = TypeExposureBoundary {
            kind: "supertrait",
            exported_path: &source_path,
            source_path: Some(&source_path),
            required_trait_path: None,
            visibility: &item.vis,
        };
        self.record_trait_bound_exposures(boundary, &item.supertraits)?;
        let predicates = item.generics.where_clause.iter().flat_map(|clause| &clause.predicates);
        for bounds in predicates.filter_map(self_supertrait_bounds) {
            self.record_trait_bound_exposures(boundary, bounds)?;
        }
        Ok(())
    }

    fn record_trait_bound_exposures<'a>(&mut self, boundary: TypeExposureBoundary<'_>, bounds: impl IntoIterator<Item = &'a TypeParamBound>) -> Result<()> {
        for bound in bounds {
            let TypeParamBound::Trait(bound) = bound else {
                continue;
            };
            if matches!(bound.modifier, TraitBoundModifier::Maybe(_)) {
                continue;
            }
            let ty = syn::Type::Path(syn::TypePath {
                qself: None,
                path: bound.path.clone(),
            });
            self.record_type_exposures(boundary, &ty)?;
        }
        Ok(())
    }

    pub(super) fn record_trait_associated_type_bound_exposures(&mut self, item: &TraitItemType) -> Result<()> {
        let source_path = self.signature_item_path();
        if source_path.is_empty() {
            return Ok(());
        }
        let visibility = Visibility::Inherited;
        let boundary_kind = format!("trait-associated-type-bound:{}", normalized_ident(&item.ident));
        let boundary = TypeExposureBoundary {
            kind: &boundary_kind,
            exported_path: &source_path,
            source_path: Some(&source_path),
            required_trait_path: None,
            visibility: &visibility,
        };
        self.enter_generic_scope(&item.generics);
        let result = self.record_trait_bound_exposures(boundary, &item.bounds);
        self.leave_generic_scope();
        result
    }

    pub(super) fn record_trait_implementation_exposures(&mut self, item: &ItemImpl, item_path: &[String], trait_path: &[String], header: &TokenStream) -> Result<()> {
        let Some((negative, implemented_trait, _)) = &item.trait_ else {
            return Ok(());
        };
        if negative.is_some() || item_path.is_empty() || trait_path.is_empty() {
            return Ok(());
        }
        let identity = format!(
            "trait-implementation-exposure:{}\0self:{}\0trait:{}\0cfg:{}\0ancestors:{}",
            syntax_fingerprint(header),
            item_path.join("::"),
            trait_path.join("::"),
            self.cfg_context.identity(),
            self.declaration_ancestor_identity()
        );
        self.public_reexports.push(PendingPublicReexport {
            evidence: PublicReexportEvidence {
                exported_path: item_path.to_vec(),
                target_path: trait_path.to_vec(),
                fingerprint: syntax_fingerprint(&identity),
                cfg: self.cfg_context.clone(),
                direct_exposure_cfg: self.direct_exposure_cfg(),
                required_trait_path: Some(trait_path.to_vec()),
            },
            cfg: self.cfg_context.clone(),
            source_path: Some(item_path.to_vec()),
        });
        let mut generic_types = self.active_generic_types();
        generic_types.extend(item.generics.type_params().map(|parameter| normalized_ident(&parameter.ident)));
        let visibility = Visibility::Inherited;
        let ancestors = self.declaration_ancestor_identity();
        let argument_exposures = public_path_argument_type_exposures(
            implemented_trait,
            &generic_types,
            &PublicTypeExposureContext {
                boundary_kind: "trait-implementation-arguments",
                exported_path: item_path,
                source_path: Some(item_path),
                required_trait_path: Some(trait_path),
                module: &self.module,
                visibility: &visibility,
                cfg: &self.cfg_context,
                direct_exposure_cfg: self.direct_exposure_cfg(),
                ancestors: &ancestors,
                rust_2015_absolute_paths: self.rust_2015_absolute_paths,
                source_revision: self.source_revision,
            },
        )?;
        self.public_reexports.extend(argument_exposures);
        Ok(())
    }

    pub(super) fn record_impl_self_type_exposures(&mut self, item: &ItemImpl, item_path: &[String], required_trait_path: Option<&[String]>) -> Result<()> {
        if item_path.is_empty() {
            return Ok(());
        }
        let mut generic_types = self.active_generic_types();
        generic_types.extend(item.generics.type_params().map(|parameter| normalized_ident(&parameter.ident)));
        let visibility = Visibility::Inherited;
        let ancestors = self.declaration_ancestor_identity();
        let exposures = public_type_exposures(
            &item.self_ty,
            &generic_types,
            &PublicTypeExposureContext {
                boundary_kind: "impl-self-type-constituents",
                exported_path: item_path,
                source_path: Some(item_path),
                required_trait_path,
                module: &self.module,
                visibility: &visibility,
                cfg: &self.cfg_context,
                direct_exposure_cfg: self.direct_exposure_cfg(),
                ancestors: &ancestors,
                rust_2015_absolute_paths: self.rust_2015_absolute_paths,
                source_revision: self.source_revision,
            },
        )?;
        self.public_reexports.extend(exposures.into_iter().filter_map(|mut exposure| {
            let constituent_path = std::mem::take(&mut exposure.evidence.target_path);
            if constituent_path == item_path {
                return None;
            }
            let identity = format!(
                "impl-self-type-constituent-exposure:{}\0source:{}\0target:{}",
                exposure.evidence.fingerprint,
                constituent_path.join("::"),
                item_path.join("::")
            );
            exposure.evidence.exported_path.clone_from(&constituent_path);
            exposure.evidence.target_path = item_path.to_vec();
            exposure.evidence.fingerprint = syntax_fingerprint(&identity);
            exposure.source_path = Some(constituent_path);
            Some(exposure)
        }));
        Ok(())
    }

    pub(super) fn record_field_type_exposure(&mut self, field: &Field) -> Result<()> {
        self.record_concrete_stores_in_visible_signature("field-type", &field.vis, &field.ty);
        let source_path = self.signature_item_path();
        self.record_type_exposures(
            TypeExposureBoundary {
                kind: "field-type",
                exported_path: &source_path,
                source_path: (!source_path.is_empty()).then_some(source_path.as_slice()),
                required_trait_path: None,
                visibility: &field.vis,
            },
            &field.ty,
        )
    }

    pub(super) fn record_item_const_type_exposure(&mut self, item: &ItemConst) -> Result<()> {
        self.record_concrete_stores_in_visible_signature("const-type", &item.vis, &item.ty);
        let exported_path = self.signature_item_path();
        self.record_type_exposures(
            TypeExposureBoundary {
                kind: "const-type",
                exported_path: &exported_path,
                source_path: None,
                required_trait_path: None,
                visibility: &item.vis,
            },
            &item.ty,
        )
    }

    pub(super) fn record_item_static_type_exposure(&mut self, item: &ItemStatic) -> Result<()> {
        self.record_concrete_stores_in_visible_signature("static-type", &item.vis, &item.ty);
        let exported_path = self.signature_item_path();
        self.record_type_exposures(
            TypeExposureBoundary {
                kind: "static-type",
                exported_path: &exported_path,
                source_path: None,
                required_trait_path: None,
                visibility: &item.vis,
            },
            &item.ty,
        )
    }

    pub(super) fn record_impl_const_type_exposure(&mut self, item: &ImplItemConst) -> Result<()> {
        self.record_impl_header_for_visible_member("inherent-impl-const", &item.vis, &item.ty);
        if !self.impl_member_is_exposed(&item.vis) {
            return Ok(());
        }
        self.record_concrete_stores_in_visible_signature("associated-const-type", &item.vis, &item.ty);
        let source_path = self.impl_item_paths.last().filter(|path| !path.is_empty()).cloned().unwrap_or_default();
        let required_trait_path = self.impl_trait_paths.last().cloned().flatten();
        self.record_type_exposures(
            TypeExposureBoundary {
                kind: "associated-const-type",
                exported_path: &source_path,
                source_path: (!source_path.is_empty()).then_some(source_path.as_slice()),
                required_trait_path: required_trait_path.as_deref(),
                visibility: &item.vis,
            },
            &item.ty,
        )
    }

    pub(super) fn record_trait_const_type_exposure(&mut self, item: &TraitItemConst) -> Result<()> {
        self.record_concrete_stores_in_signature("trait-const-type", &item.ty);
        let source_path = self.signature_item_path();
        let visibility = Visibility::Inherited;
        self.record_type_exposures(
            TypeExposureBoundary {
                kind: "trait-const-type",
                exported_path: &source_path,
                source_path: (!source_path.is_empty()).then_some(source_path.as_slice()),
                required_trait_path: None,
                visibility: &visibility,
            },
            &item.ty,
        )
    }

    pub(super) fn record_foreign_static_type_exposure(&mut self, item: &ForeignItemStatic) -> Result<()> {
        self.record_concrete_stores_in_visible_signature("foreign-static-type", &item.vis, &item.ty);
        let exported_path = self.signature_item_path();
        self.record_type_exposures(
            TypeExposureBoundary {
                kind: "foreign-static-type",
                exported_path: &exported_path,
                source_path: None,
                required_trait_path: None,
                visibility: &item.vis,
            },
            &item.ty,
        )
    }

    pub(super) fn record_impl_associated_type_exposure(&mut self, item: &ImplItemType) -> Result<()> {
        let Some(required_trait_path) = self.impl_trait_paths.last().cloned().flatten() else {
            return Ok(());
        };
        let source_path = self.impl_item_paths.last().filter(|path| !path.is_empty()).cloned().unwrap_or_default();
        self.record_type_exposures(
            TypeExposureBoundary {
                kind: "trait-associated-type",
                exported_path: &source_path,
                source_path: (!source_path.is_empty()).then_some(source_path.as_slice()),
                required_trait_path: Some(&required_trait_path),
                visibility: &item.vis,
            },
            &item.ty,
        )
    }

    fn active_generic_types(&self) -> BTreeSet<String> {
        self.generic_type_scopes.iter().flatten().cloned().collect()
    }

    pub(super) fn enter_generic_scope(&mut self, generics: &Generics) {
        self.generic_type_scopes
            .push(generics.type_params().map(|parameter| normalized_ident(&parameter.ident)).collect());
    }

    pub(super) fn leave_generic_scope(&mut self) {
        self.generic_type_scopes.pop();
    }
}
