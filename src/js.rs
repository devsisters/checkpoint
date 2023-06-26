pub mod helper;

use deno_core::{Extension, JsRuntime, RuntimeOptions};
use serde::Serialize;

pub fn prepare_js_runtime(mut extra_extensions: Vec<Extension>) -> anyhow::Result<JsRuntime> {
    let mut extensions = Vec::with_capacity(1 + extra_extensions.len());

    extensions.push(helper::checkpoint_common::init_ops());
    extensions.append(&mut extra_extensions);

    let options = RuntimeOptions {
        extensions,
        ..Default::default()
    };
    let mut js_runtime = JsRuntime::new(options);

    // Set up context
    js_runtime.execute_script_static("<checkpoint>", include_str!("js/runtime.js"))?;

    Ok(js_runtime)
}

pub fn eval<T>(js_runtime: &mut JsRuntime, code: &'static str) -> anyhow::Result<T>
where
    for<'a> T: serde::Deserialize<'a>,
{
    let global = js_runtime.execute_script_static("<checkpoint>", code)?;
    let scope = &mut js_runtime.handle_scope();
    let local = deno_core::v8::Local::new(scope, global);
    Ok(serde_v8::from_v8::<T>(scope, local)?)
}

pub fn set_context<T>(
    js_runtime: &mut JsRuntime,
    key: &'static str,
    value: &T,
) -> anyhow::Result<()>
where
    T: Serialize,
{
    js_runtime.execute_script(
        "<checkpoint>",
        format!(
            "globalThis.__checkpoint_context[\"{}\"]={};",
            key,
            serde_json::to_string(value)?
        )
        .into(),
    )?;
    Ok(())
}
