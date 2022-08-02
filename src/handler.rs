use axum::{extract, http::StatusCode, response, routing, Router};
use json_patch::{Patch, PatchOperation};
use kube::{
    core::{
        admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, SerializePatchError},
        DynamicObject,
    },
    Api,
};
use rlua::Lua;

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
    #[error("failed to convert admission request to Lua value: {0}")]
    ConvertAdmissionRequestToLuaValue(#[source] rlua::Error),
    #[error("failed to set admission request to global Lua value: {0}")]
    SetGlobalAdmissionRequestValue(#[source] rlua::Error),
    #[error("failed to set name for Lua code: {0}")]
    SetLuaCodeName(#[source] rlua::Error),
    #[error("failed to execute Lua code: {0}")]
    LuaExec(#[source] rlua::Error),
    #[error("failed to get `deny_reason` Lua variable value: {0}")]
    GetDenyReasonValue(#[source] rlua::Error),
    #[error("failed to get `patch` Lua variable value: {0}")]
    GetPatchValue(#[source] rlua::Error),
    #[error("failed to convert `patch` Lua variable value to Patch object: {0}")]
    ConvertPatchFromLuaValue(#[source] rlua::Error),
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
    let vr = vr_api.get(rule_name).await.map_err(|error| {
        if let kube::Error::Api(ref api_error) = error {
            if api_error.code == 404 {
                return Error::RuleNotFound;
            }
        }
        Error::Kubernetes(error)
    })?;

    let lua = Lua::new();
    let deny_reason = lua.context(|lua_ctx| -> Result<Option<String>, Error> {
        let globals = lua_ctx.globals();

        globals
            .set(
                "request",
                rlua_serde::to_value(lua_ctx, req.clone())
                    .map_err(Error::ConvertAdmissionRequestToLuaValue)?,
            )
            .map_err(Error::SetGlobalAdmissionRequestValue)?;

        lua_ctx
            .load(&vr.spec.code)
            .set_name("rule code")
            .map_err(Error::SetLuaCodeName)?
            .exec()
            .map_err(Error::LuaExec)?;

        let deny_reason = globals
            .get::<_, Option<String>>("deny_reason")
            .map_err(Error::GetDenyReasonValue)?;

        Ok(deny_reason)
    })?;

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

async fn mutate(
    mr_api: Api<MutatingRule>,
    rule_name: &str,
    req: &AdmissionRequest<DynamicObject>,
) -> Result<AdmissionResponse, Error> {
    let mr = mr_api.get(rule_name).await.map_err(|error| {
        if let kube::Error::Api(ref api_error) = error {
            if api_error.code == 404 {
                return Error::RuleNotFound;
            }
        }
        Error::Kubernetes(error)
    })?;

    let lua = Lua::new();
    let (deny_reason, patch) = lua.context(
        |lua_ctx| -> Result<(Option<String>, Option<Vec<PatchOperation>>), Error> {
            let globals = lua_ctx.globals();

            globals
                .set(
                    "request",
                    rlua_serde::to_value(lua_ctx, req.clone())
                        .map_err(Error::ConvertAdmissionRequestToLuaValue)?,
                )
                .map_err(Error::SetGlobalAdmissionRequestValue)?;

            lua_ctx
                .load(&mr.spec.code)
                .set_name("rule code")
                .map_err(Error::SetLuaCodeName)?
                .exec()
                .map_err(Error::LuaExec)?;

            let deny_reason = globals
                .get::<_, Option<String>>("deny_reason")
                .map_err(Error::GetDenyReasonValue)?;

            let patch = globals
                .get::<_, Option<rlua::Value>>("patch")
                .map_err(Error::GetPatchValue)?;
            let patch: Option<Vec<PatchOperation>> = patch
                .map(rlua_serde::from_value)
                .transpose()
                .map_err(Error::ConvertPatchFromLuaValue)?;

            Ok((deny_reason, patch))
        },
    )?;

    let resp: AdmissionResponse = req.into();
    let resp = if let Some(deny_reason) = deny_reason {
        resp.deny(deny_reason)
    } else {
        resp
    };
    let resp = if let Some(patch) = patch {
        resp.with_patch(Patch(patch))
            .map_err(Error::SerializePatch)?
    } else {
        resp
    };

    Ok(resp)
}
