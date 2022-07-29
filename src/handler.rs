use axum::{extract, http::StatusCode, response, routing, Router};
use kube::{
    core::{
        admission::{AdmissionRequest, AdmissionResponse, AdmissionReview},
        DynamicObject,
    },
    Api,
};
use rlua::Lua;

use crate::types::ValidatingRule;

pub fn create_app(kube_client: kube::Client) -> Router {
    Router::new()
        .route("/validate/:rule_name", routing::post(validate_handler))
        .route("/ping", routing::get(ping))
        .layer(extract::Extension(kube_client))
}

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("rule not found")]
    RuleNotFound,
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
}

struct ResponseError(anyhow::Error);

impl From<anyhow::Error> for ResponseError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

impl response::IntoResponse for ResponseError {
    fn into_response(self) -> response::Response {
        let status_code = if let Some(error) = self.0.downcast_ref::<Error>() {
            match error {
                Error::RuleNotFound => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            }
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (status_code, self.0.to_string()).into_response()
    }
}

type ResultResponse<T> = Result<T, ResponseError>;

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
    let validatingrule_api = Api::<ValidatingRule>::all(kube_client);
    let resp = validate(validatingrule_api, &rule_name, &req).await;
    if let Err(error) = &resp {
        tracing::error!(%req.name, ?req.namespace, %rule_name, %error, "failed to validate");
    }
    Ok(response::Json(resp?.into_review()))
}

async fn validate(
    validatingrule_api: Api<ValidatingRule>,
    rule_name: &str,
    req: &AdmissionRequest<DynamicObject>,
) -> anyhow::Result<AdmissionResponse> {
    let validatingrule =
        validatingrule_api
            .get(rule_name)
            .await
            .map_err(|error| -> anyhow::Error {
                if let kube::Error::Api(ref api_error) = error {
                    if api_error.code == 404 {
                        return Error::RuleNotFound.into();
                    }
                }
                error.into()
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
            .load(&validatingrule.spec.code)
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
