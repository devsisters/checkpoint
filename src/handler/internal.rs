use axum::{extract, response, routing, Json, Router};
use http::StatusCode;
use itertools::join;
use kube::core::{
    admission::{AdmissionRequest, AdmissionResponse, AdmissionReview, SerializePatchError},
    DynamicObject,
};

use crate::{types::policy::CronPolicy, util::find_group_version_pairs_by_kind};

use super::AppState;

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("object field in admission request does not exists")]
    ObjectNotExists,
    #[error("Kubernetes error: {0}")]
    Kubernetes(#[source] kube::Error),
    #[error("failed to serialize to JSON value: {0}")]
    SerializeToJson(#[source] serde_json::Error),
    #[error("failed to serialize JSON patch: {0}")]
    SerializePatch(SerializePatchError),
}

impl response::IntoResponse for Error {
    fn into_response(self) -> response::Response {
        let status_code = match self {
            Self::ObjectNotExists => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status_code, self.to_string()).into_response()
    }
}

pub fn create_router() -> Router<AppState> {
    Router::new().route(
        "/mutate/cronpolicies",
        routing::post(post_mutate_cronpolicy),
    )
}

async fn mutate_cronpolicy(
    req: AdmissionRequest<CronPolicy>,
    kube_client: kube::Client,
) -> Result<AdmissionResponse, Error> {
    let resp: AdmissionResponse = (&req).into();

    let mut cp = req.object.ok_or(Error::ObjectNotExists)?;
    // Original cronpolicy spec to be diffed after
    let orig_cp = cp.clone();

    for resource in cp.spec.resources.iter_mut() {
        if resource.group.is_some() && resource.version.is_some() {
            // The user specified group and version
            // Check the GVK actually exists
            let gvs = find_group_version_pairs_by_kind(&resource.kind, false, kube_client.clone())
                .await
                .map_err(Error::Kubernetes)?;
            if !gvs
                .into_iter()
                .any(|(g, v)| Some(g) == resource.group && Some(v) == resource.version)
            {
                // It does not exists!
                return Ok(resp.deny(format!(
                    "specified group, version, and kind (`{}`, `{}`, `{}`) do not exists",
                    resource.group.as_ref().unwrap(),
                    resource.version.as_ref().unwrap(),
                    resource.kind
                )));
            }
        } else if resource.group.is_none() && resource.version.is_some() {
            return Ok(resp.deny("only specifying version is not allowed"));
        } else {
            // The user did not specify group or version
            // checkpoint have to find it
            let gvs = find_group_version_pairs_by_kind(&resource.kind, true, kube_client.clone())
                .await
                .map_err(Error::Kubernetes)?;
            if gvs.is_empty() {
                return Ok(resp.deny(format!(
                    "specified kind ({}) does not have matching group/versions",
                    resource.kind
                )));
            } else if let Some(group) = &resource.group {
                let gv = gvs.into_iter().find(|(g, _)| g == group);
                if let Some(gv) = gv {
                    resource.group = Some(gv.0);
                    resource.version = Some(gv.1);
                } else {
                    return Ok(resp.deny(format!(
                        "specified group and kind ({}, {}) does not have matching group/versions",
                        group, resource.kind
                    )));
                }
            } else if gvs.len() > 1 {
                return Ok(resp.deny(format!(
                    "specified kind ({}) has multiple matching group/versions: {}",
                    resource.kind,
                    join(gvs.into_iter().map(|(g, v)| format!("`{}/{}`", g, v)), ",")
                )));
            } else {
                let mut gvs = gvs;
                let gv = gvs.pop().unwrap();
                resource.group = Some(gv.0);
                resource.version = Some(gv.1);
            }
        }
    }

    let patch = json_patch::diff(
        &serde_json::to_value(&orig_cp).map_err(Error::SerializeToJson)?,
        &serde_json::to_value(&cp).map_err(Error::SerializeToJson)?,
    );

    resp.with_patch(patch).map_err(Error::SerializePatch)
}

async fn post_mutate_cronpolicy(
    extract::State(state): extract::State<AppState>,
    Json(req): Json<AdmissionReview<CronPolicy>>,
) -> Result<Json<AdmissionReview<DynamicObject>>, Error> {
    // Convert AdmissionReview into AdmissionRequest
    // and reject if fails
    let req: AdmissionRequest<_> = match req.try_into() {
        Ok(req) => req,
        Err(error) => {
            tracing::error!(%error, "invalid request");
            return Ok(Json(
                AdmissionResponse::invalid(error.to_string()).into_review(),
            ));
        }
    };

    // Clones name and namespace needed when error occurs
    let req_name = req.name.clone();
    let req_namespace = req.namespace.clone();

    // Mutate cronpolicy and check error
    match mutate_cronpolicy(req, state.kube_client).await {
        Ok(resp) => Ok(Json(resp.into_review())),
        Err(error) => {
            // Log error
            tracing::error!(%req_name, ?req_namespace, %error, "failed to mutate cronpolicy");
            Err(error)
        }
    }
}
