pub mod helper;

use std::cell::Ref;

use k8s_openapi::api::authentication::v1::{TokenRequest, TokenRequestSpec};
use kube::{
    config::AuthInfo,
    core::{admission::AdmissionRequest, DynamicObject},
    Api, Client,
};
use mlua::Lua;

use crate::{lua::lua_to_value, types::rule::ServiceAccountInfo};

use super::Error;

struct LuaContextAppData {
    kube_client: Option<Client>,
}

/// Evaluate Lua code and return its output
pub(super) async fn eval_lua_code<T>(
    lua: Lua,
    code: String,
    admission_req: AdmissionRequest<DynamicObject>,
) -> Result<T, Error>
where
    for<'a> T: mlua::FromLuaMulti<'a> + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();

    // Spawn a thread dedicated to Lua
    // Lua context is not Sync and returned future from Chunk::call_async is not Send.
    // So we use a dedicated single thread for Lua context and block on that thread.
    // But with a help of oneshot channel above, the HTTP handler thread is not blocked.
    std::thread::spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread() // Prepare tokio single-threaded runtime
            .enable_all()
            .build()
            .map_err(Error::CreateRuntime)
            .and_then(|runtime| {
                // Block on current thread
                runtime.block_on(async move {
                    // Serialize AdmissionRequest to Lua value
                    let admission_req_lua_value = lua_to_value(&lua, &admission_req)
                        .map_err(Error::ConvertAdmissionRequestToLuaValue)?;

                    // Load Lua code chunk
                    let lua_chunk = lua
                        .load(&code)
                        .set_name("rule code")
                        .map_err(Error::SetLuaCodeName)?;

                    // Evaluate Lua code chunk as a function
                    let output = lua_chunk
                        .call_async(admission_req_lua_value)
                        .await
                        .map_err(Error::LuaEval)?;
                    Ok(output)
                })
            });
        // Send result into oneshot channel
        let _ = tx.send(result);
    });

    // Receive result from oneshot channel
    rx.await.map_err(Error::RecvLuaThread)?
}

/// Prepare Kubernetes client with specified ServiceAccount info in Rule spec
async fn prepare_kube_client(
    client: Client,
    serviceaccount_info: &ServiceAccountInfo,
    timeout_seconds: Option<i32>,
) -> Result<kube::Client, Error> {
    let sa_api = Api::namespaced(client, &serviceaccount_info.namespace);

    // Retrieve token from ServiceAccount
    let tr = sa_api
        .create_token_request(
            &serviceaccount_info.name,
            &Default::default(),
            &TokenRequest {
                metadata: Default::default(),
                spec: TokenRequestSpec {
                    audiences: vec!["https://kubernetes.default.svc.cluster.local".to_string()],
                    // expirationSeconds should greater than 10 minutes
                    expiration_seconds: Some(std::cmp::max(
                        timeout_seconds.unwrap_or(10).into(),
                        10 * 60,
                    )),
                    ..Default::default()
                },
                status: None,
            },
        )
        .await
        .map_err(|error| {
            if let kube::Error::Api(ref api_error) = error {
                if api_error.code == 404 {
                    return Error::ServiceAccountNotFound;
                }
            }
            Error::Kubernetes(error)
        })?;
    let token = tr.status.ok_or(Error::RequestServiceAccountToken)?.token;

    let mut kube_config = kube::Config::incluster().map_err(Error::KubernetesInClusterConfig)?;

    // Set auth info with token
    kube_config.auth_info = AuthInfo {
        token: Some(secrecy::SecretString::new(token)),
        ..Default::default()
    };

    let new_client = Client::try_from(kube_config).map_err(Error::Kubernetes)?;

    Ok(new_client)
}

pub(super) async fn prepare_lua_ctx(
    client: Client,
    serviceaccount_info: &Option<ServiceAccountInfo>,
    timeout_seconds: Option<i32>,
) -> Result<Lua, Error> {
    let lua = crate::lua::prepare_lua_ctx().map_err(Error::PrepareLuaContext)?;

    // Prepare app data
    // Create Kubernetes client which is restricted with provided ServiceAccount
    let restricted_client = if let Some(serviceaccount_info) = serviceaccount_info {
        Some(prepare_kube_client(client, serviceaccount_info, timeout_seconds).await?)
    } else {
        None
    };
    let app_data = LuaContextAppData {
        kube_client: restricted_client,
    };

    lua.set_app_data(app_data);

    helper::register_lua_helper_functions(&lua).map_err(Error::PrepareLuaContext)?;

    Ok(lua)
}

fn extract_kube_client_from_lua_ctx(lua: &Lua) -> mlua::Result<Client> {
    let app_data: Ref<LuaContextAppData> = lua
        .app_data_ref()
        .ok_or_else(|| mlua::Error::external(Error::LuaAppDataNotFound))?;
    app_data
        .kube_client
        .clone()
        .ok_or_else(|| mlua::Error::external(Error::ServiceAccountInfoNotProvided))
}
