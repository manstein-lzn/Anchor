//! Factory advisory data. Activated data is supplied by composition, not read here.
pub(crate) use anchor_facts::guidance::InputGuidance;

pub(crate) fn factory() -> InputGuidance {
    let content = include_str!("../../../resources/harness/input-discovery.md")
        .trim_end()
        .to_owned();
    InputGuidance {
        schema_version: 1,
        slot: anchor_facts::guidance::INPUT_DISCOVERY.into(),
        content_digest: anchor_facts::text_digest(&content),
        content,
        scope: None,
        activation: None,
        candidate: None,
    }
}
