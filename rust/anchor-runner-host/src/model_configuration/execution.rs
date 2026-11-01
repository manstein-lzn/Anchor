//! Per-invocation model selection. A returned registry owns its public fields and
//! credential; later settings edits cannot change an execution already in flight.
use super::{
    providers::{ProviderFields, Slot},
    *,
};

pub(crate) const MODEL_REFERENCES: &str = "models.default.flash, models.default.pro, models.backup.flash, models.backup.pro (omitted or models.default = default Flash)";

const DEFAULT_REFERENCE: &str = "models.default.flash";
const RETAINED_UNAVAILABLE: &str = "the retained model provider credential is unavailable; open Settings to restore its key or start a new Session";

#[derive(Clone, Copy)]
enum Tier {
    Flash,
    Pro,
}
impl Tier {
    fn model(self, fields: &ProviderFields) -> &str {
        match self {
            Self::Flash => &fields.flash,
            Self::Pro => &fields.pro,
        }
    }
}

fn selection(reference: Option<&str>) -> Option<(Slot, Tier, &'static str)> {
    match reference {
        None | Some("models.default" | "models.default.flash") => {
            Some((Slot::Default, Tier::Flash, DEFAULT_REFERENCE))
        }
        Some("models.default.pro") => Some((Slot::Default, Tier::Pro, "models.default.pro")),
        Some("models.backup.flash") => Some((Slot::Backup, Tier::Flash, "models.backup.flash")),
        Some("models.backup.pro") => Some((Slot::Backup, Tier::Pro, "models.backup.pro")),
        _ => None,
    }
}

impl ModelConfiguration {
    /// New invocations read the latest published settings. A retained invocation
    /// supplies its binding and original reference, so only that slot's public
    /// revisions can provide credentials for its original endpoint/wire/model.
    pub(crate) fn execution_snapshot(
        &self,
        binding: Option<&str>,
        reference: Option<&str>,
    ) -> Result<ModelRegistry, String> {
        self.load()
            .and_then(|managed| self.execution_from(&managed, binding, reference))
            .map_err(|error| {
                format!(
                    "agent model reference {:?}: {error}",
                    reference.unwrap_or(DEFAULT_REFERENCE)
                )
            })
    }

    /// Check reference resolution, not credentials or provider reachability.
    pub(crate) fn validate_reference(&self, reference: Option<&str>) -> Result<(), String> {
        self.validate_reference_from(&self.load()?, reference)
    }

    fn validate_reference_from(
        &self,
        managed: &ManagedModel,
        reference: Option<&str>,
    ) -> Result<(), String> {
        if selection(reference).is_some() {
            return Ok(());
        }
        // Reuse the existing managed mapping, including deterministic providers.
        if self
            .registry(managed, &managed.current, true)
            .and_then(|registry| registry.resolve(reference))
            .is_ok()
        {
            return Ok(());
        }
        let aliases = managed.current.aliases.keys().cloned().collect::<Vec<_>>();
        Err(format!(
            "unknown agent model reference {:?}; accepted references: {MODEL_REFERENCES}{}",
            reference.unwrap_or_default(),
            if aliases.is_empty() {
                String::new()
            } else {
                format!("; configured aliases: {}", aliases.join(", "))
            }
        ))
    }

    fn execution_from(
        &self,
        managed: &ManagedModel,
        binding: Option<&str>,
        reference: Option<&str>,
    ) -> Result<ModelRegistry, String> {
        if binding.is_none() {
            self.validate_reference_from(managed, reference)?;
        }
        let Some((slot, tier, canonical)) = selection(reference) else {
            // Existing explicit aliases retain their original legacy meaning.
            return self.legacy_snapshot(managed, binding, reference);
        };
        let legacy_default = matches!((slot, tier), (Slot::Default, Tier::Flash));
        if let Some(binding) = binding {
            let candidates = managed
                .providers
                .get(slot)
                .map(|fields| (fields, true))
                .into_iter()
                .chain(
                    managed
                        .providers
                        .history(slot)
                        .map(|fields| (fields, false)),
                );
            let mut matched = false;
            for (fields, current) in candidates {
                let Ok(validation) =
                    self.slot_registry(fields, tier, canonical, "validation-placeholder")
                else {
                    continue;
                };
                if !validation
                    .resolve(reference)
                    .is_ok_and(|candidate| candidate.identity == binding)
                {
                    continue;
                }
                matched = true;
                let key = if current {
                    self.provider_key(managed, slot)?
                } else {
                    self.retained_slot_key(managed, slot, fields)
                };
                if let Some(key) = key {
                    return self.slot_registry(fields, tier, canonical, key);
                }
            }
            if matched {
                return Err(RETAINED_UNAVAILABLE.into());
            }
            // These names were valid explicit legacy aliases before named
            // providers existed. An existing unscoped identity must preserve
            // that exact alias; named identities cannot match this fallback.
            if let Ok(registry) = self.legacy_snapshot(managed, Some(binding), reference) {
                return Ok(registry);
            }
            // A default invocation can also predate models.default.flash as a
            // reference for the primary model. Never consult another named slot.
            return if legacy_default {
                self.legacy_default_snapshot(managed, Some(binding), reference)
            } else {
                Err(RETAINED_UNAVAILABLE.into())
            };
        }
        let Some(fields) = managed.providers.get(slot) else {
            return if legacy_default {
                self.legacy_default_snapshot(managed, None, reference)
            } else {
                Err("the selected model reference has no saved provider configured".into())
            };
        };
        let key = self
            .provider_key(managed, slot)?
            .ok_or("the selected provider has no API key configured")?;
        self.slot_registry(fields, tier, canonical, key)
    }

    fn slot_registry(
        &self,
        fields: &ProviderFields,
        tier: Tier,
        reference: &str,
        key: &str,
    ) -> Result<ModelRegistry, String> {
        let model = tier.model(fields);
        if model.is_empty() {
            return Err("the selected tier has no model configured".into());
        }
        let mut selected = fields.model_fields(model);
        selected.aliases.insert(reference.into(), model.into());
        let (slot, _, _) = selection(Some(reference)).ok_or("invalid named model reference")?;
        ModelRegistry::from_fields(
            &selected,
            key,
            self.external
                .get("ANCHOR_MODEL_DIRECT")
                .is_some_and(|value| value == "1"),
        )
        .map(|registry| registry.with_credential_scope(slot.credential_id()))
    }

    fn legacy_default_snapshot(
        &self,
        managed: &ManagedModel,
        binding: Option<&str>,
        reference: Option<&str>,
    ) -> Result<ModelRegistry, String> {
        let current = self.effective(&managed.current)?;
        let registry = |fields: &ModelFields| {
            let mut fields = fields.clone();
            fields
                .aliases
                .insert(DEFAULT_REFERENCE.into(), fields.model.clone());
            self.registry(managed, &fields, false)
        };
        if let Some(binding) = binding {
            return std::iter::once(&current)
                .chain(&managed.history)
                .filter_map(|fields| registry(fields).ok())
                .find(|registry| {
                    registry
                        .resolve(reference)
                        .is_ok_and(|candidate| candidate.identity == binding)
                })
                .ok_or_else(|| RETAINED_UNAVAILABLE.into());
        }
        registry(&current).map(|current| {
            current.with_retained(
                managed
                    .history
                    .iter()
                    .filter_map(|fields| registry(fields).ok())
                    .collect(),
            )
        })
    }

    pub(super) fn execution_status(&self, managed: &ManagedModel) -> Result<Value, String> {
        let (fields, source) = if let Some(fields) = managed.providers.get(Slot::Default) {
            (Some(fields.model_fields(&fields.flash)), "host")
        } else {
            let fields = self.effective(&managed.current)?;
            if fields.endpoint.is_empty() {
                (None, "empty")
            } else {
                (Some(fields), "legacy")
            }
        };
        let result = self.execution_from(managed, None, None);
        Ok(json!({
            "default_reference": DEFAULT_REFERENCE,
            "source": source,
            "configured": result.is_ok(),
            "fields": fields,
            "api_key_present": self.provider_key(managed, Slot::Default)?.is_some(),
            "error": result.err(),
        }))
    }
}

#[cfg(test)]
mod tests;
