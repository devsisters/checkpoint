pub mod helper;

use kube::core::{admission::AdmissionRequest, DynamicObject};

use crate::{
    js::{eval, set_context},
    types::rule::ServiceAccountInfo,
};

use super::{Error, JsOutput};

/// Evaluate JavaScript code and return its output
async fn eval_js_code_inner<T>(
    serviceaccount_info: Option<ServiceAccountInfo>,
    timeout_seconds: Option<i32>,
    code: String,
    admission_req: AdmissionRequest<DynamicObject>,
    js_context: String,
) -> Result<T, Error>
where
    for<'a> T: serde::Deserialize<'a> + Send + 'static,
{
    // Prepare JS runtime
    let mut js_runtime = crate::js::prepare_js_runtime(vec![helper::checkpoint_rule::init_ops()])
        .map_err(Error::PrepareJsRuntime)?;

    // Set context for kubeGet and kubeList
    set_context(&mut js_runtime, "serviceAccountInfo", &serviceaccount_info)
        .map_err(Error::PrepareJsRuntime)?;
    set_context(&mut js_runtime, "timeoutSeconds", &timeout_seconds)
        .map_err(Error::PrepareJsRuntime)?;
    set_context(&mut js_runtime, "admissionRequest", &admission_req)
        .map_err(Error::PrepareJsRuntime)?;

    // Prepare context
    js_runtime
        .execute_script_static("<checkpoint>", include_str!("runtime.js"))
        .map_err(Error::PrepareJsRuntime)?;

    // Add additional context
    if !js_context.is_empty() {
        js_runtime
            .execute_script("<checkpoint>", js_context.into())
            .map_err(Error::PrepareJsRuntime)?;
    }

    // Run code
    js_runtime
        .execute_script("<checkpoint>", code.into())
        .map_err(Error::EvalJs)?;
    js_runtime
        .run_event_loop(false)
        .await
        .map_err(Error::EvalJs)?;

    // Get output
    eval::<T>(&mut js_runtime, "__checkpoint_get_context(\"output\")").map_err(Error::EvalJs)
}

/// wrapper function to spawn JS runtime into local thread
pub(super) async fn eval_js_code(
    serviceaccount_info: Option<ServiceAccountInfo>,
    timeout_seconds: Option<i32>,
    code: String,
    admission_req: AdmissionRequest<DynamicObject>,
    js_context: String,
) -> Result<JsOutput, Error> {
    let (sender, receiver) = tokio::sync::oneshot::channel();

    // Build tokio runtime
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::CreateTokioRuntime)?;

    // Spawn JS runtime into dedicated thread
    std::thread::spawn(move || {
        let local = tokio::task::LocalSet::new();

        local.spawn_local(async move {
            let res = eval_js_code_inner(
                serviceaccount_info,
                timeout_seconds,
                code,
                admission_req,
                js_context,
            )
            .await;
            let _ = sender.send(res);
        });

        rt.block_on(local);
    });

    receiver.await.map_err(Error::RecvJsThread)?
}
