use super::*;
use anchor_library::InstallRequest;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallBody {
    source: String,
    id: Option<String>,
    #[serde(default, rename = "replace")]
    replace_existing: bool,
}

pub(crate) async fn install_plugin(
    State(state): State<ApiState>,
    Json(body): Json<InstallBody>,
) -> Result<HttpResponse, HttpResponse> {
    #[cfg(test)]
    let checkout = state.plugin_checkout.clone();
    #[cfg(not(test))]
    let checkout = None;
    let outcome = state
        .application
        .install_plugin(
            library_root(&state.catalog_root),
            InstallRequest {
                source: body.source,
                id: body.id,
                replace_existing: body.replace_existing,
            },
            checkout,
        )
        .await
        .map_err(application_error)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id":outcome.id,"digest":outcome.digest})),
    )
        .into_response())
}
