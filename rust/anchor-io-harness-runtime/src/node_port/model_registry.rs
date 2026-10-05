//! Host-neutral Graph model selection. The endpoint configuration and credentials
//! belong to the host; the NodePort records only an opaque public-binding digest.

use std::collections::BTreeMap;

use rig_core::DynModel;
use rig_core::operation::Completion;
use sha2::{Digest, Sha256};

use crate::adapter::RigProviderAdapter;

#[derive(Clone)]
pub struct RigModelRegistry {
    default: ModelBinding,
    aliases: BTreeMap<String, ModelBinding>,
}

#[derive(Clone)]
pub(super) struct ModelBinding {
    model: DynModel<Completion>,
    pub(super) provider: RigProviderAdapter,
    pub(super) fingerprint: String,
    pub(super) accepts_images: bool,
}

impl ModelBinding {
    fn new(model: DynModel<Completion>, public_identity: &str, accepts_images: bool) -> Self {
        // The host supplies endpoint, wire and model identity, never credentials.
        // Hash it here so persistence cannot accidentally include an endpoint URL.
        let fingerprint = format!("{:x}", Sha256::digest(public_identity.as_bytes()));
        Self {
            provider: RigProviderAdapter::new(model.clone(), accepts_images),
            model,
            fingerprint,
            accepts_images,
        }
    }
}

impl RigModelRegistry {
    /// `public_identity` must describe the endpoint, wire and model binding.
    /// It must not contain credentials; changing credentials alone may resume
    /// the same model, while changing this binding fails closed at the NodePort.
    pub fn new(default: DynModel<Completion>, public_identity: &str) -> Self {
        Self::new_with_image_capability(default, public_identity, false)
    }

    pub fn new_with_image_capability(
        default: DynModel<Completion>,
        public_identity: &str,
        accepts_images: bool,
    ) -> Self {
        Self {
            default: ModelBinding::new(default, public_identity, accepts_images),
            aliases: BTreeMap::new(),
        }
    }

    /// Preserve the existing single-model constructors. Their available public
    /// identity is Rig's provider name and model id; production hosts should
    /// use `new` with their complete endpoint/wire/model identity.
    pub(super) fn single(default: DynModel<Completion>) -> Self {
        let identity = serde_json::to_string(&(default.name(), default.id()))
            .expect("provider names and model ids are JSON strings");
        Self::new(default, &identity)
    }

    pub fn with_alias(
        self,
        reference: impl Into<String>,
        model: DynModel<Completion>,
        public_identity: &str,
    ) -> Result<Self, String> {
        self.with_alias_and_image_capability(reference, model, public_identity, false)
    }

    pub fn with_alias_and_image_capability(
        mut self,
        reference: impl Into<String>,
        model: DynModel<Completion>,
        public_identity: &str,
        accepts_images: bool,
    ) -> Result<Self, String> {
        let reference = reference.into();
        if !reference.starts_with("models.") || reference == "models.default" {
            return Err("model aliases require a non-default models.* name".into());
        }
        if self.aliases.contains_key(&reference) {
            return Err("model alias is declared more than once".into());
        }
        self.aliases.insert(
            reference,
            ModelBinding::new(model, public_identity, accepts_images),
        );
        Ok(self)
    }

    /// Explicit aliases take precedence; every unknown reference selects the
    /// default, matching Python env_model_profiles' fallback_for_unknown_refs.
    pub fn model(&self, reference: Option<&str>) -> DynModel<Completion> {
        self.select(reference).model.clone()
    }

    /// Deployment-owned capability for the selected actual model binding.
    pub fn accepts_images(&self, reference: Option<&str>) -> bool {
        self.select(reference).accepts_images
    }

    pub(super) fn select(&self, reference: Option<&str>) -> &ModelBinding {
        reference
            .and_then(|reference| self.aliases.get(reference))
            .unwrap_or(&self.default)
    }
}
