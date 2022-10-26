use std::cell::Ref;

use k8s_openapi::api::authentication::v1::{TokenRequest, TokenRequestSpec};
use kube::{
    api::{ApiResource, ListParams},
    config::AuthInfo,
    core::{admission::AdmissionRequest, DynamicObject, GroupVersionKind, Object},
    Api, Client,
};
use mlua::{Lua, LuaSerdeExt, Value};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::{handler::Error, types::rule::ServiceAccountInfo};

fn lua_to_value<'lua, T>(lua: &'lua Lua, value: &T) -> mlua::Result<Value<'lua>>
where
    T: Serialize + ?Sized,
{
    lua.to_value_with(
        value,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}

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

    // Create config from env
    // TODO: Use incluster_env when https://github.com/kube-rs/kube/issues/153 is resolved
    let mut kube_config =
        kube::Config::incluster_dns().map_err(Error::KubernetesInClusterConfig)?;
    // let mut kube_config =
    //     kube::Config::incluster_env().map_err(Error::KubernetesInClusterConfig)?;

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

    let lua = Lua::new();
    lua.set_app_data(app_data);

    // Enable sandbox mode
    lua.sandbox(true).map_err(Error::SetLuaSandbox)?;

    register_lua_helper_functions(&lua).map_err(Error::RegisterHelperFunction)?;

    Ok(lua)
}

pub fn register_lua_helper_functions(lua: &Lua) -> Result<(), mlua::Error> {
    let globals = lua.globals();

    macro_rules! register_lua_function {
        ($name:literal, $func:ident) => {
            let f = lua.create_function($func)?;
            globals.set($name, f)?;
        };
        ($name:literal, $func:ident, async) => {
            let f = lua.create_async_function($func)?;
            globals.set($name, f)?;
        };
    }

    // Register all Lua helper functions
    register_lua_function!("debugPrint", lua_debug_print);
    register_lua_function!("deepCopy", lua_deepcopy);
    register_lua_function!("jsonPatchDiff", lua_jsonpatch_diff);
    register_lua_function!("startsWith", lua_starts_with);
    register_lua_function!("endsWith", lua_ends_with);
    register_lua_function!("kubeGet", lua_kube_get, async);
    register_lua_function!("kubeList", lua_kube_list, async);

    Ok(())
}

/// Lua helper function to debug-print Lua value with JSON format
fn lua_debug_print<'lua>(lua: &'lua Lua, v: Value<'lua>) -> mlua::Result<()> {
    let v_json: serde_json::Value = lua.from_value(v)?;
    tracing::info!(
        "debug print fron Lua code: {}",
        serde_json::to_string(&v_json).map_err(mlua::Error::external)?
    );
    Ok(())
}

// Lua helper function to deep-copy a Lua value
fn lua_deepcopy<'lua>(lua: &'lua Lua, v: Value<'lua>) -> mlua::Result<Value<'lua>> {
    // Convert Lua value to JSON value and convert back to deep-copy
    let v_json: serde_json::Value = lua.from_value(v)?;
    lua_to_value(lua, &v_json)
}

// Lua helper function to generate jsonpatch with diff of two table
fn lua_jsonpatch_diff<'lua>(
    lua: &'lua Lua,
    (v1, v2): (Value<'lua>, Value<'lua>),
) -> mlua::Result<Value<'lua>> {
    let v1_json: serde_json::Value = lua.from_value(v1)?;
    let v2_json: serde_json::Value = lua.from_value(v2)?;
    let patch = json_patch::diff(&v1_json, &v2_json);
    lua_to_value(lua, &patch)
}

// Lua helper function to check first string starts with second string
fn lua_starts_with(_lua: &Lua, (s1, s2): (String, String)) -> mlua::Result<bool> {
    Ok(s1.starts_with(&s2))
}

// Lua helper function to check first string ends with second string
fn lua_ends_with(_lua: &Lua, (s1, s2): (String, String)) -> mlua::Result<bool> {
    Ok(s1.ends_with(&s2))
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

#[derive(Deserialize, Debug, Clone, Hash, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KubeGetArgument {
    pub group: String,
    pub version: String,
    pub kind: String,
    pub plural: Option<String>,
    pub namespace: Option<String>,
    pub name: String,
}

/// Lua helper function to get a Kubernetes resource
async fn lua_kube_get<'lua>(lua: &'lua Lua, argument: Value<'lua>) -> mlua::Result<Value<'lua>> {
    // Unpack argument
    let KubeGetArgument {
        group,
        version,
        kind,
        plural,
        namespace,
        name,
    } = lua.from_value(argument)?;

    // Prepare GroupVersionKind and ApiResource from argument
    let gvk = GroupVersionKind::gvk(&group, &version, &kind);
    let ar = if let Some(plural) = plural {
        ApiResource::from_gvk_with_plural(&gvk, &plural)
    } else {
        ApiResource::from_gvk(&gvk)
    };

    let client = extract_kube_client_from_lua_ctx(lua)?;

    // Prepare Kubernetes API with or without namespace
    let api = if let Some(namespace) = namespace {
        Api::<Object<JsonValue, JsonValue>>::namespaced_with(client, &namespace, &ar)
    } else {
        Api::<Object<JsonValue, JsonValue>>::all_with(client, &ar)
    };

    // Get object
    let object = api.get_opt(&name).await.map_err(mlua::Error::external)?;

    // Serialize object into Lua value
    lua_to_value(lua, &object)
}

#[derive(Deserialize, Debug, Clone, Hash, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KubeListArgument {
    pub group: String,
    pub version: String,
    pub kind: String,
    pub plural: Option<String>,
    pub namespace: Option<String>,
    pub list_params: Option<KubeListArgumentListParams>,
}

fn default_kube_list_argument_list_params_bookmarks() -> bool {
    true
}

#[derive(Deserialize, Debug, Clone, Hash, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct KubeListArgumentListParams {
    pub label_selector: Option<String>,
    pub field_selector: Option<String>,
    pub timeout: Option<u32>,
    #[serde(default = "default_kube_list_argument_list_params_bookmarks")]
    pub bookmarks: bool,
    pub limit: Option<u32>,
    pub continue_token: Option<String>,
}

/// Lua helper function to list Kubernetes resources
async fn lua_kube_list<'lua>(lua: &'lua Lua, argument: Value<'lua>) -> mlua::Result<Value<'lua>> {
    // Unpack argument
    let KubeListArgument {
        group,
        version,
        kind,
        plural,
        namespace,
        list_params,
    } = lua.from_value(argument)?;
    let list_params = list_params
        .map(
            |KubeListArgumentListParams {
                 label_selector,
                 field_selector,
                 timeout,
                 bookmarks,
                 limit,
                 continue_token,
             }| ListParams {
                label_selector,
                field_selector,
                timeout,
                bookmarks,
                limit,
                continue_token,
            },
        )
        .unwrap_or_default();

    // Prepare GroupVersionKind and ApiResource from argument
    let gvk = GroupVersionKind::gvk(&group, &version, &kind);
    let ar = if let Some(plural) = plural {
        ApiResource::from_gvk_with_plural(&gvk, &plural)
    } else {
        ApiResource::from_gvk(&gvk)
    };

    let client = extract_kube_client_from_lua_ctx(lua)?;

    // Prepare Kubernetes API with or without namespace
    let api = if let Some(namespace) = namespace {
        Api::<Object<JsonValue, JsonValue>>::namespaced_with(client, &namespace, &ar)
    } else {
        Api::<Object<JsonValue, JsonValue>>::all_with(client, &ar)
    };

    // List objects
    let object_list = api
        .list(&list_params)
        .await
        .map_err(mlua::Error::external)?;

    // Serialize object list into Lua value
    lua_to_value(lua, &object_list)
}
