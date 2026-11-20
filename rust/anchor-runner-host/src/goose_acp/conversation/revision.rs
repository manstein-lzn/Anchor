// This is a reviewed, one-way native-history compatibility pair, not permission
// to resume an existing invocation with another executable. The caller keeps
// current invocation identity strict and verifies the previous binding/fact.
// All reviewed revisions use Goose 1.53.0's native session schema; the patch only adds
// optional provider metadata to Thinking blocks and groups tool calls only in
// Responses request projection. The completion patch changes executor boundaries
// and transient tool context without changing the native history schema. No
// history is rewritten. Provider errors reuse existing Error blocks and
// completion reminders reuse ordinary model-visible messages.
pub(super) const BEFORE_REASONING_FIX: &str =
    "71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7";
pub(super) const WITH_REASONING_FIX: &str =
    "0fba6a3339fe5425c3934deda8715d0a557ad60419ac2fded5a0bc2429e354b6";

pub(super) const WITH_MULTICALL_FIX: &str =
    "ddb5e2cff8dfa5bb323e577b87fc04e6db43d61be3110516a278e1ac74ec734c";

pub(super) const WITH_COMPLETION_FIX: &str =
    "dc2a4352279c16f3c665b280e4fc8fedbec3a67cce596bf460aa9fd416c27a15";

pub(super) const WITH_PROVIDER_ERROR_FIX: &str =
    "17e065a79cb0508413af714724aed51d6b76972dda9feebdda701330134fb640";

pub(super) const WITH_COMPLETION_REMINDERS: &str =
    "4f9baa8a42be4d05bb0056f8e7e6e0a4daa8a6fc1aeb38a578a3310461abaee3";

pub(super) fn can_load_history(previous: &str, current: &str) -> bool {
    previous == current
        || (previous == BEFORE_REASONING_FIX && current == WITH_REASONING_FIX)
        || (matches!(previous, BEFORE_REASONING_FIX | WITH_REASONING_FIX)
            && current == WITH_MULTICALL_FIX)
        || (matches!(
            previous,
            BEFORE_REASONING_FIX | WITH_REASONING_FIX | WITH_MULTICALL_FIX
        ) && current == WITH_COMPLETION_FIX)
        || (matches!(
            previous,
            BEFORE_REASONING_FIX | WITH_REASONING_FIX | WITH_MULTICALL_FIX | WITH_COMPLETION_FIX
        ) && current == WITH_PROVIDER_ERROR_FIX)
        || (matches!(
            previous,
            BEFORE_REASONING_FIX
                | WITH_REASONING_FIX
                | WITH_MULTICALL_FIX
                | WITH_COMPLETION_FIX
                | WITH_PROVIDER_ERROR_FIX
        ) && current == WITH_COMPLETION_REMINDERS)
}
