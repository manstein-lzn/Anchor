use super::{pilot::PilotTurn, transport::ServerRequestHandler};
use agent_client_protocol_schema::v1::{
    CreateElicitationRequest, CreateElicitationResponse, ElicitationMode, ElicitationScope,
};
use anchor_platform_session::QuestionStatus;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

pub(super) struct PilotElicitation {
    pub(super) request: Arc<PilotTurn>,
    pub(super) native_session: String,
}

impl ServerRequestHandler for PilotElicitation {
    fn handle<'request>(
        &'request self,
        params: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'request>> {
        Box::pin(async move {
            let requested_schema = params["requestedSchema"].clone();
            if params["mode"] == "form" {
                anchor_platform_session::validate_question_schema(&params["requestedSchema"])
                    .map_err(|_| "Goose elicitation schema is unsupported")?;
            }
            let request: CreateElicitationRequest = serde_json::from_value(params)
                .map_err(|_| "Goose elicitation request is malformed")?;
            let ElicitationMode::Form(form) = request.mode else {
                return Ok(json!({"action":"cancel"}));
            };
            let ElicitationScope::Session(scope) = form.scope else {
                return Err("Goose elicitation is not scoped to the active Session".into());
            };
            if scope.session_id.0.as_ref() != self.native_session {
                return Err("Goose elicitation belongs to another Session".into());
            }
            let question = self
                .request
                .sessions
                .create_question(
                    &self.request.owner,
                    &self.request.session,
                    &self.request.turn,
                    &request.message,
                    requested_schema,
                )
                .map_err(|_| "Pilot question persistence failed")?;
            loop {
                let question = self
                    .request
                    .sessions
                    .get_question(
                        &self.request.owner,
                        &self.request.session,
                        &self.request.turn,
                        &question.id,
                    )
                    .map_err(|_| "Pilot question is unavailable")?;
                match question.status {
                    QuestionStatus::Pending => tokio::time::sleep(Duration::from_millis(50)).await,
                    QuestionStatus::Interrupted => return Ok(json!({"action":"cancel"})),
                    QuestionStatus::Answered => {
                        let answer = question
                            .answer
                            .ok_or("Pilot answered question has no answer")?;
                        let value = serde_json::to_value(answer)
                            .map_err(|_| "Pilot answer serialization failed")?;
                        let response: CreateElicitationResponse = serde_json::from_value(value)
                            .map_err(|_| "Pilot answer does not conform to ACP")?;
                        return serde_json::to_value(response)
                            .map_err(|_| "ACP answer serialization failed".into());
                    }
                }
            }
        })
    }
}
