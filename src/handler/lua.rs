use std::cell::Ref;

use k8s_openapi::api::core::v1::{Secret, ServiceAccount};
use kube::{
    api::{ApiResource, ListParams},
    core::{admission::AdmissionRequest, DynamicObject, GroupVersionKind, Object},
    Api, Client,
};
use mlua::{Lua, LuaSerdeExt, Value};
use serde::Deserialize;
use serde_json::Value as JsonValue;

use crate::types::ServiceAccountInfo;

use super::Error;

struct LuaContextAppData {
    kube_client: Option<Client>,
}

/// Evaluate Lua code and return its output
pub(super) async fn eval_lua_code<T>(
    client: Client,
    serviceaccount_info: Option<ServiceAccountInfo>,
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
                    let lua = prepare_lua_ctx(client, serviceaccount_info).await?;

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

/// Prepare Kubernetes client with specified ServiceAccount info in Rule spec
async fn prepare_kube_client(
    client: Client,
    serviceaccount_info: ServiceAccountInfo,
) -> Result<kube::Client, Error> {
    // Prepare Kubernetes APIs
    let serviceaccount_api =
        Api::<ServiceAccount>::namespaced(client.clone(), &serviceaccount_info.namespace);
    let secret_api = Api::<Secret>::namespaced(client, &serviceaccount_info.namespace);

    let serviceaccount = serviceaccount_api
        .get(&serviceaccount_info.name)
        .await
        .map_err(Error::Kubernetes)?;
    // Extract Secret name from ServiceAccount
    let secret_name = serviceaccount
        .secrets
        .unwrap_or_default()
        .get(0)
        .and_then(|or| or.name.clone())
        .ok_or(Error::ServiceAccountDoesNotHaveSecretReference)?;
    let secret = secret_api
        .get(&secret_name)
        .await
        .map_err(Error::Kubernetes)?;

    // Extract Secret data
    let secret_data = secret.data.unwrap_or_default();
    macro_rules! get {
        ($key:expr) => {
            secret_data
                .get($key)
                .ok_or(Error::ServiceAccountSecretDataDoesNotHaveKey($key))?
        };
    }
    let ca_crt = base64::encode(&get!("ca.crt").0);
    let namespace = String::from_utf8_lossy(&get!("namespace").0);
    let token = String::from_utf8_lossy(&get!("token").0);

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
                namespace: Some(namespace.to_string()),
                extensions: None,
            },
        }],
        auth_infos: vec![kube::config::NamedAuthInfo {
            name: DEFAULT.to_string(),
            auth_info: kube::config::AuthInfo {
                token: Some(secrecy::SecretString::new(token.to_string())),
                ..Default::default()
            },
        }],
        clusters: vec![kube::config::NamedCluster {
            name: DEFAULT.to_string(),
            cluster: kube::config::Cluster {
                server: env_default_config.cluster_url.to_string(),
                insecure_skip_tls_verify: Some(env_default_config.accept_invalid_certs),
                certificate_authority: None,
                certificate_authority_data: Some(ca_crt),
                proxy_url: env_default_config.proxy_url.map(|url| url.to_string()),
                extensions: None,
            },
        }],
        ..Default::default()
    };
    // Convert it back to Config
    let config = kube::Config::from_custom_kubeconfig(kube_config, &Default::default())
        .await
        .map_err(Error::KubernetesKubeconfig)?;

    let new_client = Client::try_from(config).map_err(Error::Kubernetes)?;

    Ok(new_client)
}

async fn prepare_lua_ctx(
    client: Client,
    serviceaccount_info: Option<ServiceAccountInfo>,
) -> Result<Lua, Error> {
    // Prepare app data
    // Create Kubernetes client which is restricted with provided ServiceAccount
    let restricted_client = if let Some(serviceaccount_info) = serviceaccount_info {
        Some(prepare_kube_client(client, serviceaccount_info).await?)
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

    {
        let globals = lua.globals();

        // Macro to register Lua helper functions
        macro_rules! register_lua_function {
            ($name:literal, $func:ident) => {
                let f = lua
                    .create_function($func)
                    .map_err(Error::CreateLuaFunction)?;
                globals.set($name, f).map_err(Error::SetLuaValue)?;
            };
            ($name:literal, $func:ident, async) => {
                let f = lua
                    .create_async_function($func)
                    .map_err(Error::CreateLuaFunction)?;
                globals.set($name, f).map_err(Error::SetLuaValue)?;
            };
        }

        // Register all Lua helper functions
        register_lua_function!("debug_print", lua_debug_print);
        register_lua_function!("deepcopy", lua_deepcopy);
        register_lua_function!("jsonpatch_diff", lua_jsonpatch_diff);
        register_lua_function!("kube_get", lua_kube_get, async);
        register_lua_function!("kube_list", lua_kube_list, async);
    }

    Ok(lua)
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

#[derive(Deserialize, Debug, Clone)]
struct KubeGetArgument {
    group: String,
    version: String,
    kind: String,
    plural: Option<String>,
    namespace: Option<String>,
    name: String,
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

#[derive(Deserialize, Debug, Clone)]
struct KubeListArgument {
    group: String,
    version: String,
    kind: String,
    plural: Option<String>,
    namespace: Option<String>,
    list_params: Option<KubeListArgumentListParams>,
}

fn default_kube_list_argument_list_params_bookmarks() -> bool {
    true
}

#[derive(Deserialize, Debug, Clone)]
struct KubeListArgumentListParams {
    label_selector: Option<String>,
    field_selector: Option<String>,
    timeout: Option<u32>,
    #[serde(default = "default_kube_list_argument_list_params_bookmarks")]
    bookmarks: bool,
    limit: Option<u32>,
    continue_token: Option<String>,
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
