mod internal;
pub mod lua;

use axum::{extract, http::StatusCode, response, routing, Router};
use json_patch::{Patch, PatchOperation};
use kube::{
    core::{
        admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, SerializePatchError},
        DynamicObject,
    },
    Api,
};
use mlua::{Lua, LuaSerdeExt};

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
    #[error("Lua app data not found. This is a bug.")]
    LuaAppDataNotFound,
    #[error("serviceAccount field is not provided. You should provide serviceAccount field in Rule spec if you want to use `kubeGet` or `kubeList` function in Lua code.")]
    ServiceAccountInfoNotProvided,
    #[error("provided ServiceAccount is not found")]
    ServiceAccountNotFound,
    #[error("failed to request ServiceAccount token")]
    RequestServiceAccountToken,
    #[error("Kubernetes error: {0}")]
    Kubernetes(#[source] kube::Error),
    #[error("Kubernetes in-cluster config error: {0}")]
    KubernetesInClusterConfig(#[source] kube::config::InClusterError),
    #[error("Kubernetes Kubeconfig error: {0}")]
    KubernetesKubeconfig(#[source] kube::config::KubeconfigError),
    #[error("failed to prepare Lua context: {0}")]
    PrepareLuaContext(#[source] mlua::Error),
    #[error("failed to convert admission request to Lua value: {0}")]
    ConvertAdmissionRequestToLuaValue(#[source] mlua::Error),
    #[error("failed to create Tokio runtime: {0}")]
    CreateRuntime(#[source] std::io::Error),
    #[error("failed to set name for Lua code: {0}")]
    SetLuaCodeName(#[source] mlua::Error),
    #[error("failed to execute Lua code: {0}")]
    LuaEval(#[source] mlua::Error),
    #[error("failed to receive from Lua thread: {0}")]
    RecvLuaThread(#[source] tokio::sync::oneshot::error::RecvError),
    #[error("failed to serialize Patch object: {0}")]
    SerializePatch(#[source] SerializePatchError),
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

    let lua = lua::prepare_lua_ctx(
        state.kube_client,
        &vr.spec.0.service_account,
        vr.spec.0.timeout_seconds,
    )
    .await?;

    let resp = validate(&vr.spec.0, &req, lua).await;

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
    lua: Lua,
) -> Result<AdmissionResponse, Error> {
    // Evaluate Lua code and get `deny_reason`
    let deny_reason: Option<String> =
        lua::eval_lua_code(lua, rule_spec.code.clone(), req.clone()).await?;

    // Prepare AdmissionResponse from AddmissionRequest
    let resp: AdmissionResponse = req.into();

    // Set deny reason if exists
    let resp = if let Some(deny_reason) = deny_reason {
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

    let lua = lua::prepare_lua_ctx(
        state.kube_client,
        &mr.spec.0.service_account,
        mr.spec.0.timeout_seconds,
    )
    .await?;

    let resp = mutate(&mr.spec.0, &req, lua).await;

    // Log if error happens
    if let Err(error) = &resp {
        tracing::error!(%req.name, ?req.namespace, %rule_name, %error, "failed to mutate");
    }

    Ok(response::Json(resp?.into_review()))
}

/// Wrapper to implement FromLua
struct VecPatchOperation(Vec<PatchOperation>);

impl<'lua> mlua::FromLua<'lua> for VecPatchOperation {
    fn from_lua(lua_value: mlua::Value<'lua>, lua: &'lua mlua::Lua) -> mlua::Result<Self> {
        let v = lua.from_value(lua_value)?;
        Ok(Self(v))
    }
}

/// Actual mutating function
pub async fn mutate(
    rule_spec: &RuleSpec,
    req: &AdmissionRequest<DynamicObject>,
    lua: Lua,
) -> Result<AdmissionResponse, Error> {
    // Evaluate Lua code and get `deny_reason` and `patch`
    let (deny_reason, patch): (Option<String>, Option<VecPatchOperation>) =
        lua::eval_lua_code(lua, rule_spec.code.clone(), req.clone()).await?;

    // Prepare AdmissionResponse from AdmissionRequest
    let resp: AdmissionResponse = req.into();

    // Set deny reason if exists
    let resp = if let Some(deny_reason) = deny_reason {
        resp.deny(deny_reason)
    } else {
        resp
    };

    // Set patch if exists
    let resp = if let Some(patch) = patch {
        resp.with_patch(Patch(patch.0))
            .map_err(Error::SerializePatch)?
    } else {
        resp
    };

    Ok(resp)
}
