use std::cell::Ref;

use k8s_openapi::api::{
    authentication::v1::{TokenRequest, TokenRequestSpec},
    core::v1::ServiceAccount,
};
use kube::{
    api::{ApiResource, ListParams},
    core::{admission::AdmissionRequest, GroupVersionKind, Object},
    Api, Client, Resource,
};
use mlua::{Lua, LuaSerdeExt, Value};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::{
    handler::Error,
    types::{rule::ServiceAccountInfo, DynamicObjectWithOptionalMetadata},
};

struct LuaContextAppData {
    kube_client: Option<Client>,
}

/// Evaluate Lua code and return its output
pub(super) async fn eval_lua_code<T>(
    lua: Lua,
    code: String,
    admission_req: AdmissionRequest<DynamicObjectWithOptionalMetadata>,
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
                    let admission_req_lua_value = lua
                        .to_value_with(
                            &admission_req,
                            mlua::SerializeOptions::new()
                                .serialize_none_to_null(false)
                                .serialize_unit_to_null(false),
                        )
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

/// Create a TokenRequest of a ServiceAccount
///
/// Workaround before https://github.com/kube-rs/kube-rs/pull/989 is merged and released.
async fn create_token_request(
    client: Client,
    name: &str,
    namespace: &str,
    token_request: &TokenRequest,
) -> kube::Result<TokenRequest> {
    let url_path = format!(
        "{}/{}/token",
        ServiceAccount::url_path(
            &<ServiceAccount as Resource>::DynamicType::default(),
            Some(namespace)
        ),
        name
    );
    let body = serde_json::to_vec(token_request).map_err(kube::Error::SerdeError)?;
    let mut req = http::Request::post(url_path)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(body)
        .map_err(|e| kube::Error::BuildRequest(kube::core::request::Error::BuildRequest(e)))?;
    req.extensions_mut().insert("create_token_request");
    client.request(req).await
}

/// Prepare Kubernetes client with specified ServiceAccount info in Rule spec
async fn prepare_kube_client(
    client: Client,
    serviceaccount_info: &ServiceAccountInfo,
    timeout_seconds: Option<i32>,
) -> Result<kube::Client, Error> {
    // Retrieve token from ServiceAccount
    let tr = create_token_request(
        client,
        &serviceaccount_info.name,
        &serviceaccount_info.namespace,
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

    // Default config from env
    let env_default_config =
        kube::Config::from_cluster_env().map_err(Error::KubernetesInClusterConfig)?;

    // Populate Kubeconfig, and convert it back to Config
    // We should use this hack because kube crate does not allow modifying AuthInfo of Config
    // Reference: https://github.com/kube-rs/kube-rs/discussions/957
    // We can directly modify AuthInfo when https://github.com/kube-rs/kube-rs/pull/959 is released

    // Populated Kubeconfig from ServiceAccount Secret data and default env config
    const DEFAULT: &str = "default";
    let kube_config = kube::config::Kubeconfig {
        current_context: Some(DEFAULT.to_string()),
        contexts: vec![kube::config::NamedContext {
            name: DEFAULT.to_string(),
            context: kube::config::Context {
                cluster: DEFAULT.to_string(),
                user: DEFAULT.to_string(),
                namespace: Some(serviceaccount_info.namespace.clone()),
                extensions: None,
            },
        }],
        auth_infos: vec![kube::config::NamedAuthInfo {
            name: DEFAULT.to_string(),
            auth_info: kube::config::AuthInfo {
                token: Some(secrecy::SecretString::new(token)),
                ..Default::default()
            },
        }],
        clusters: vec![kube::config::NamedCluster {
            name: DEFAULT.to_string(),
            cluster: kube::config::Cluster {
                server: env_default_config.cluster_url.to_string(),
                insecure_skip_tls_verify: Some(env_default_config.accept_invalid_certs),
                certificate_authority: None,
                certificate_authority_data: None,
                proxy_url: env_default_config.proxy_url.map(|url| url.to_string()),
                extensions: None,
            },
        }],
        ..Default::default()
    };
    // Convert it back to Config
    let mut config = kube::Config::from_custom_kubeconfig(kube_config, &Default::default())
        .await
        .map_err(Error::KubernetesKubeconfig)?;
    // Set missing fields
    config.root_cert = env_default_config.root_cert;

    let new_client = Client::try_from(config).map_err(Error::Kubernetes)?;

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
    register_lua_function!("deepcopy", lua_deepcopy);
    register_lua_function!("jsonpatchDiff", lua_jsonpatch_diff);
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
    lua.to_value_with(
        &v_json,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}

// Lua helper function to generate jsonpatch with diff of two table
fn lua_jsonpatch_diff<'lua>(
    lua: &'lua Lua,
    (v1, v2): (Value<'lua>, Value<'lua>),
) -> mlua::Result<Value<'lua>> {
    let v1_json: serde_json::Value = lua.from_value(v1)?;
    let v2_json: serde_json::Value = lua.from_value(v2)?;
    let patch = json_patch::diff(&v1_json, &v2_json);
    lua.to_value_with(
        &patch,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
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
    lua.to_value_with(
        &object,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}

#[derive(Deserialize, Debug, Clone, Hash, PartialEq, Eq)]
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
    lua.to_value_with(
        &object_list,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}
