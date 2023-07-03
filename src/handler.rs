mod internal;
pub mod js;

use axum::{extract, http::StatusCode, response, routing, Router};
use json_patch::Patch;
use kube::{
    core::{
        admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, SerializePatchError},
        DynamicObject,
    },
    Api,
};
use serde::Deserialize;
use tokio::task::JoinError;

use crate::types::rule::{MutatingRule, RuleSpec, ValidatingRule};

#[derive(Clone)]
pub struct AppState {
    kube_client: kube::Client,
}

/// Prepare HTTP router
pub fn create_app(kube_client: kube::Client) -> Router {
    let app_state = AppState { kube_client };

    let internal = internal::create_router();

    Router::new()
        .route("/validate/:rule_name", routing::post(validate_handler))
        .route("/mutate/:rule_name", routing::post(mutate_handler))
        .nest("/internal", internal)
        .with_state(app_state)
        .route("/ping", routing::get(ping))
        .layer(tower_http::trace::TraceLayer::new_for_http())
}

/// Errors can be raised within HTTP handler
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Rule is not found")]
    RuleNotFound,
    #[error("Kubernetes error: {0}")]
    Kubernetes(#[source] kube::Error),
    #[error("Kubernetes Kubeconfig error: {0}")]
    KubernetesKubeconfig(#[source] kube::config::KubeconfigError),
    #[error("failed to create Tokio runtime: {0}")]
    CreateTokioRuntime(#[source] std::io::Error),
    #[error("failed to receive from JavaScript thread: {0}")]
    RecvJsThread(#[source] tokio::sync::oneshot::error::RecvError),
    #[error("failed to serialize Patch object: {0}")]
    SerializePatch(#[source] SerializePatchError),
    #[error("failed to join JavaScript task: {0}")]
    JoinJsTask(#[source] JoinError),
    #[error("failed to prepare JavaScript runtime: {0}")]
    PrepareJsRuntime(#[source] anyhow::Error),
    #[error("failed to evaluate JavaScript code: {0}")]
    EvalJs(#[source] anyhow::Error),
    #[error("failed to deserialize JavaScript value: {0}")]
    DeserializeJsValue(#[source] serde_v8::Error),
}

impl response::IntoResponse for Error {
    fn into_response(self) -> response::Response {
        let status_code = match self {
            Self::RuleNotFound => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status_code, self.to_string()).into_response()
    }
}

async fn ping() -> &'static str {
    "ok"
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsOutput {
    #[serde(default)]
    deny_reason: Option<String>,
    #[serde(default)]
    patch: Option<Patch>,
}

/// Validate HTTP API handler
async fn validate_handler(
    extract::State(state): extract::State<AppState>,
    extract::Path(rule_name): extract::Path<String>,
    extract::Json(req): extract::Json<AdmissionReview<DynamicObject>>,
) -> Result<response::Json<AdmissionReview<DynamicObject>>, Error> {
    // Convert AdmissionReview into AdmissionRequest
    // and reject if fails
    let req: AdmissionRequest<_> = match req.try_into() {
        Ok(req) => req,
        Err(error) => {
            tracing::error!(%error, "invalid request");
            return Ok(response::Json(
                AdmissionResponse::invalid(error.to_string()).into_review(),
            ));
        }
    };

    // Prepare Kubernetes API
    let vr_api = Api::<ValidatingRule>::all(state.kube_client.clone());

    // Get matching ValidatingRule
    let vr = vr_api
        .get_opt(&rule_name)
        .await
        .map_err(Error::Kubernetes)?
        .ok_or(Error::RuleNotFound)?;

    let resp = validate(&vr.spec.0, &req, String::new()).await;

    // Log if error happens
    if let Err(error) = &resp {
        tracing::error!(%req.name, ?req.namespace, %rule_name, %error, "failed to validate");
    }

    Ok(response::Json(resp?.into_review()))
}

/// Actual validating function
pub async fn validate(
    rule_spec: &RuleSpec,
    req: &AdmissionRequest<DynamicObject>,
    js_context: String, // required for CLI
) -> Result<AdmissionResponse, Error> {
    // Evaluate JS code
    let output = js::eval_js_code(
        rule_spec.service_account.clone(),
        rule_spec.timeout_seconds,
        rule_spec.code.clone(),
        req.clone(),
        js_context,
    )
    .await?;

    // Prepare AdmissionResponse from AddmissionRequest
    let resp: AdmissionResponse = req.into();

    // Set deny reason if exists
    let resp = if let Some(deny_reason) = output.deny_reason {
        resp.deny(deny_reason)
    } else {
        resp
    };

    Ok(resp)
}

async fn mutate_handler(
    extract::State(state): extract::State<AppState>,
    extract::Path(rule_name): extract::Path<String>,
    extract::Json(req): extract::Json<AdmissionReview<DynamicObject>>,
) -> Result<response::Json<AdmissionReview<DynamicObject>>, Error> {
    // Convert AdmissionReview into AdmissionRequest
    // and reject if fails
    let req: AdmissionRequest<_> = match req.try_into() {
        Ok(req) => req,
        Err(error) => {
            tracing::error!(%error, "invalid request");
            return Ok(response::Json(
                AdmissionResponse::invalid(error.to_string()).into_review(),
            ));
        }
    };

    // Prepare Kubernetes API
    let mr_api = Api::<MutatingRule>::all(state.kube_client.clone());

    // Get matching MutatingRule
    let mr = mr_api
        .get_opt(&rule_name)
        .await
        .map_err(Error::Kubernetes)?
        .ok_or(Error::RuleNotFound)?;

    let resp = mutate(&mr.spec.0, &req, String::new()).await;

    // Log if error happens
    if let Err(error) = &resp {
        tracing::error!(%req.name, ?req.namespace, %rule_name, %error, "failed to mutate");
    }

    Ok(response::Json(resp?.into_review()))
}

/// Actual mutating function
pub async fn mutate(
    rule_spec: &RuleSpec,
    req: &AdmissionRequest<DynamicObject>,
    js_context: String, // required for CLI
) -> Result<AdmissionResponse, Error> {
    // Evaluate JS code
    let output = js::eval_js_code(
        rule_spec.service_account.clone(),
        rule_spec.timeout_seconds,
        rule_spec.code.clone(),
        req.clone(),
        js_context,
    )
    .await?;

    // Prepare AdmissionResponse from AdmissionRequest
    let resp: AdmissionResponse = req.into();

    // Set deny reason if exists
    let resp = if let Some(deny_reason) = output.deny_reason {
        resp.deny(deny_reason)
    } else {
        resp
    };

    // Set patch if exists
    let resp = if let Some(patch) = output.patch {
        resp.with_patch(Patch(patch.0))
            .map_err(Error::SerializePatch)?
    } else {
        resp
    };

    Ok(resp)
}
