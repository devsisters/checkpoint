mod lua;

use axum::{extract, http::StatusCode, response, routing, Router};
use json_patch::{Patch, PatchOperation};
use kube::{
    core::{
        admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, SerializePatchError},
        DynamicObject,
    },
    Api,
};
use mlua::LuaSerdeExt;

use crate::types::{MutatingRule, ValidatingRule};

pub fn create_app(kube_client: kube::Client) -> Router {
    Router::new()
        .route("/ping", routing::get(ping))
        .route("/validate/:rule_name", routing::post(validate_handler))
        .route("/mutate/:rule_name", routing::post(mutate_handler))
        .layer(extract::Extension(kube_client))
}

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("rule not found")]
    RuleNotFound,
    #[error("Kubernetes error: {0}")]
    Kubernetes(#[source] kube::Error),
    #[error("failed to set Lua sandbox mode: {0}")]
    SetLuaSandbox(#[source] mlua::Error),
    #[error("failed to create Lua function: {0}")]
    CreateLuaFunction(#[source] mlua::Error),
    #[error("failed to set Lua value: {0}")]
    SetLuaValue(#[source] mlua::Error),
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

type ResultResponse<T> = Result<T, Error>;

async fn ping() -> &'static str {
    "ok"
}

async fn validate_handler(
    extract::Extension(kube_client): extract::Extension<kube::Client>,
    extract::Path(rule_name): extract::Path<String>,
    extract::Json(req): extract::Json<AdmissionReview<DynamicObject>>,
) -> ResultResponse<response::Json<AdmissionReview<DynamicObject>>> {
    let req: AdmissionRequest<_> = match req.try_into() {
        Ok(req) => req,
        Err(error) => {
            tracing::error!(%error, "invalid request");
            return Ok(response::Json(
                AdmissionResponse::invalid(error.to_string()).into_review(),
            ));
        }
    };
    let vr_api = Api::<ValidatingRule>::all(kube_client);
    let resp = validate(vr_api, &rule_name, &req).await;
    if let Err(error) = &resp {
        tracing::error!(%req.name, ?req.namespace, %rule_name, %error, "failed to validate");
    }
    Ok(response::Json(resp?.into_review()))
}

async fn validate(
    vr_api: Api<ValidatingRule>,
    rule_name: &str,
    req: &AdmissionRequest<DynamicObject>,
) -> Result<AdmissionResponse, Error> {
    let vr = vr_api
        .get_opt(rule_name)
        .await
        .map_err(Error::Kubernetes)?
        .ok_or(Error::RuleNotFound)?;

    let deny_reason: Option<String> = lua::eval_lua_code(vr.spec.code, req.clone()).await?;

    let resp: AdmissionResponse = req.into();
    let resp = if let Some(deny_reason) = deny_reason {
        resp.deny(deny_reason)
    } else {
        resp
    };

    Ok(resp)
}

async fn mutate_handler(
    extract::Extension(kube_client): extract::Extension<kube::Client>,
    extract::Path(rule_name): extract::Path<String>,
    extract::Json(req): extract::Json<AdmissionReview<DynamicObject>>,
) -> ResultResponse<response::Json<AdmissionReview<DynamicObject>>> {
    let req: AdmissionRequest<_> = match req.try_into() {
        Ok(req) => req,
        Err(error) => {
            tracing::error!(%error, "invalid request");
            return Ok(response::Json(
                AdmissionResponse::invalid(error.to_string()).into_review(),
            ));
        }
    };
    let mr_api = Api::<MutatingRule>::all(kube_client);
    let resp = mutate(mr_api, &rule_name, &req).await;
    if let Err(error) = &resp {
        tracing::error!(%req.name, ?req.namespace, %rule_name, %error, "failed to mutate");
    }
    Ok(response::Json(resp?.into_review()))
}

struct VecPatchOperation(Vec<PatchOperation>);

impl<'lua> mlua::FromLua<'lua> for VecPatchOperation {
    fn from_lua(lua_value: mlua::Value<'lua>, lua: &'lua mlua::Lua) -> mlua::Result<Self> {
        let v = lua.from_value(lua_value)?;
        Ok(Self(v))
    }
}

async fn mutate(
    mr_api: Api<MutatingRule>,
    rule_name: &str,
    req: &AdmissionRequest<DynamicObject>,
) -> Result<AdmissionResponse, Error> {
    let mr = mr_api
        .get_opt(rule_name)
        .await
        .map_err(Error::Kubernetes)?
        .ok_or(Error::RuleNotFound)?;

    let (deny_reason, patch): (Option<String>, Option<VecPatchOperation>) =
        lua::eval_lua_code(mr.spec.code, req.clone()).await?;

    let resp: AdmissionResponse = req.into();
    let resp = if let Some(deny_reason) = deny_reason {
        resp.deny(deny_reason)
    } else {
        resp
    };
    let resp = if let Some(patch) = patch {
        resp.with_patch(Patch(patch.0))
            .map_err(Error::SerializePatch)?
    } else {
        resp
    };

    Ok(resp)
}
