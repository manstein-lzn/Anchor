//! The Pilot describes non-sensitive bot settings. Mutations are executed
//! only by the existing native confirmation adapter, never by model consent.
use super::*;
use crate::application::channel_management::{ChannelBinding, ChannelMembers};

fn only(arguments: &Value, allowed: &[&str], message: &str) -> Result<(), ToolError> {
    let object = arguments
        .as_object()
        .ok_or_else(|| rejected("channel arguments must be an object"))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(rejected(message));
    }
    Ok(())
}

fn members(arguments: &Value, name: &str) -> Result<Vec<String>, ToolError> {
    serde_json::from_value(
        arguments
            .get(name)
            .cloned()
            .ok_or_else(|| rejected(format!("{name} must be explicitly provided")))?,
    )
    .map_err(|_| rejected(format!("{name} must be a string array")))
}

pub(crate) fn configuration_arguments(
    arguments: &Value,
) -> Result<(String, ChannelMembers), ToolError> {
    only(
        arguments,
        &["graph", "users", "send_users", "private_only"],
        "provide only the bot's graph and members; credentials and approval are not model arguments",
    )?;
    let graph = parameter(arguments, "graph")?;
    let private_only = match arguments.get("private_only") {
        None => false,
        Some(value) => value
            .as_bool()
            .ok_or_else(|| rejected("private_only must be a boolean"))?,
    };
    let members = ChannelMembers {
        users: members(arguments, "users")?,
        send_users: members(arguments, "send_users")?,
        private_only,
    };
    // Validate the member lists before asking the user; the Plugin is the
    // Graph's and is checked again by the application use case.
    ChannelBinding {
        plugin: "unchecked".into(),
        users: members.users.clone(),
        send_users: members.send_users.clone(),
        private_only: members.private_only,
    }
    .validate()
    .map_err(channel_error)?;
    Ok((graph, members))
}

pub(crate) fn enable_arguments(arguments: &Value) -> Result<(String, bool), ToolError> {
    only(
        arguments,
        &["graph", "enabled"],
        "provide only graph and enabled; approval is not a model argument",
    )?;
    let graph = parameter(arguments, "graph")?;
    let enabled = arguments
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| rejected("enabled must be a boolean"))?;
    Ok((graph, enabled))
}

impl PilotTools {
    pub(super) async fn channel_status(&self, arguments: &Value) -> Result<Value, ToolError> {
        only(
            arguments,
            &["graph"],
            "provide graph only, or nothing to list every bot",
        )?;
        match arguments.get("graph") {
            None => self.application.channel_list(&self.owner).await,
            Some(_) => {
                self.application
                    .channel_status(&self.owner, &parameter(arguments, "graph")?)
                    .await
            }
        }
        .map_err(channel_error)
    }

    pub(crate) async fn configure_channel_approved(
        &self,
        graph: &str,
        members: ChannelMembers,
    ) -> Result<Value, ToolError> {
        self.require_active_turn()?;
        self.application
            .configure_channel(&self.owner, graph, members)
            .await
            .map_err(channel_error)
    }

    pub(crate) async fn enable_channel_approved(
        &self,
        graph: &str,
        enabled: bool,
        expected: &ChannelBinding,
    ) -> Result<Value, ToolError> {
        self.require_active_turn()?;
        self.application
            .set_channel_enabled_checked(&self.owner, graph, enabled, expected)
            .await
            .map_err(channel_error)
    }
}

fn channel_error(error: crate::application::ApplicationError) -> ToolError {
    match error {
        crate::application::ApplicationError::Missing => {
            rejected("bot Graph, Plugin, or binding is unavailable")
        }
        crate::application::ApplicationError::Invalid(reason)
        | crate::application::ApplicationError::Conflict(reason)
        | crate::application::ApplicationError::Storage(reason) => rejected(reason),
    }
}

#[cfg(test)]
mod tests;
