use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use itertools::Itertools;
use kube::core::{admission::AdmissionRequest, ObjectList};
use mlua::{Lua, LuaSerdeExt};
use tracing::Instrument;

use checkpoint::{
    handler::{
        lua::{register_lua_helper_functions, KubeGetArgument, KubeListArgument},
        mutate, validate,
    },
    types::{
        rule::{MutatingRule, ValidatingRule},
        testcase::{Case, TestCase},
        DynamicObjectWithOptionalMetadata,
    },
};

#[derive(Parser, Debug)]
struct Cli {
    #[clap(subcommand)]
    subcommand: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Test(TestArgs),
}

#[derive(Args, Debug)]
struct TestArgs {
    #[clap(value_parser)]
    test_case_paths: Vec<PathBuf>,
}

#[derive(Debug)]
struct CaseResult {
    allowed: bool,
    message: Option<String>,
    final_object: Option<DynamicObjectWithOptionalMetadata>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(tracing_subscriber::filter::LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .compact()
        .without_time()
        .init();

    let cli = Cli::parse();

    match cli.subcommand {
        Commands::Test(args) => cli_test(args).await,
    }
}

async fn cli_test(args: TestArgs) -> Result<()> {
    for test_case_path in args.test_case_paths {
        let test_case_span =
            tracing::info_span!("test-case-file", path = %test_case_path.display());
        run_test_case(&test_case_path)
            .instrument(test_case_span)
            .await
            .with_context(|| {
                format!(
                    "failed to test for test case file {}",
                    test_case_path.display()
                )
            })?;
    }
    Ok(())
}

async fn run_test_case(test_case_path: &Path) -> Result<()> {
    // Open and deserialize test case file
    let test_case_file = fs::File::open(test_case_path).context("failed to open test case file")?;
    let test_case: TestCase =
        serde_yaml::from_reader(test_case_file).context("failed to deserialize test case")?;

    let test_case_base_path = test_case_path.parent().unwrap();

    // Make mutating and validating rules
    let mutating_rules: Vec<MutatingRule> = test_case
        .mutating_rules
        .into_iter()
        .map(|fnoo| fnoo.into_objects(test_case_base_path))
        .flatten_ok()
        .try_collect()
        .context("failed to load mutating rules")?;
    let validating_rules: Vec<ValidatingRule> = test_case
        .validating_rules
        .into_iter()
        .map(|fnoo| fnoo.into_objects(test_case_base_path))
        .flatten_ok()
        .try_collect()
        .context("failed to load validating rules")?;

    // Evaulate cases
    for (i, case) in test_case.cases.into_iter().enumerate() {
        let case_name = case.name.clone().unwrap_or_else(|| format!("{}", i));
        let case_span = tracing::info_span!("case", case = case_name);
        run_case(
            case,
            test_case_base_path,
            &mutating_rules,
            &validating_rules,
        )
        .instrument(case_span)
        .await
        .with_context(|| format!("failed to test for case \"{}\"", case_name))?;
    }

    Ok(())
}

async fn run_case(
    case: Case,
    test_case_base_path: &Path,
    mutating_rules: &[MutatingRule],
    validating_rules: &[ValidatingRule],
) -> Result<()> {
    let mut request = case
        .request
        .into_object(test_case_base_path)
        .context("failed to load request")?;

    // Make stub map
    let kube_get_stub_map = case
        .stubs
        .kube_get
        .into_iter()
        .map(|stub| {
            stub.output
                .into_object(test_case_base_path)
                .map(|object| (stub.parameter, object))
        })
        .try_collect()
        .context("failed to load kubeGet stub map")?;
    let kube_list_stub_map = case
        .stubs
        .kube_list
        .into_iter()
        .map(|stub| {
            stub.output
                .into_object(test_case_base_path)
                .map(|object| (stub.parameter, object))
        })
        .try_collect()
        .context("failed to load kubeList stub map")?;

    let lua_app_data = LuaContextAppData {
        kube_get_stub_map: Arc::new(kube_get_stub_map),
        kube_list_stub_map: Arc::new(kube_list_stub_map),
    };

    let expected = CaseResult {
        allowed: case.expected.allowed,
        message: case.expected.message,
        final_object: case
            .expected
            .final_object
            .map(|fnoo| fnoo.into_object(test_case_base_path))
            .transpose()
            .context("failed to load final object")?
            .or_else(|| request.object.clone()),
    };
    let mut actual = CaseResult {
        allowed: true,
        message: None,
        final_object: request.object.clone(),
    };

    for rule in mutating_rules {
        let rule_name = rule
            .metadata
            .name
            .as_ref()
            .ok_or_else(|| anyhow!("rule does not have name"))?;
        let rule_span = tracing::info_span!("mutating-rule", rule = rule_name);

        actual = run_mutating_rule(rule, &mut request, lua_app_data.clone())
            .instrument(rule_span.clone())
            .await
            .with_context(|| format!("failed to test for rule \"{}\"", rule_name))?;

        let _enter = rule_span.enter();
        if !actual.allowed {
            tracing::info!("disallowed");
            break;
        } else {
            tracing::info!("allowed");
        }
    }

    for rule in validating_rules {
        let rule_name = rule
            .metadata
            .name
            .as_ref()
            .ok_or_else(|| anyhow!("rule does not have name"))?;
        let rule_span = tracing::info_span!("validating-rule", rule = rule_name);

        actual = run_validating_rule(rule, &request, lua_app_data.clone())
            .instrument(rule_span.clone())
            .await
            .with_context(|| format!("failed to test for rule \"{}\"", rule_name))?;

        let _enter = rule_span.enter();
        if !actual.allowed {
            tracing::info!("disallowed");
            break;
        } else {
            tracing::info!("allowed");
        }
    }

    if expected.allowed != actual.allowed {
        return Err(anyhow!(
            "test failed. `allowed` expected: {}, actual: {}",
            expected.allowed,
            actual.allowed
        ));
    }
    if expected.message != actual.message {
        return Err(anyhow!(
            "test failed. `message` expected: {:?}, actual: {:?}",
            expected.message,
            actual.message
        ));
    }
    if expected.final_object != actual.final_object {
        return Err(anyhow!(
            "test failed. `finalObject` expected: {}, actual: {}",
            serde_json::to_string(&expected.final_object)
                .context("failed to serialize expected final object of failed test")?,
            serde_json::to_string(&actual.final_object)
                .context("failed to serialize actual final object of failed test")?,
        ));
    }
    tracing::info!("passed");

    Ok(())
}

async fn run_mutating_rule(
    rule: &MutatingRule,
    request: &mut AdmissionRequest<DynamicObjectWithOptionalMetadata>,
    lua_app_data: LuaContextAppData,
) -> Result<CaseResult> {
    let lua = prepare_lua_ctx(lua_app_data).context("failed to prepare Lua context")?;

    let response = mutate(&rule.spec.0, request, lua)
        .await
        .context("failed to mutate")?;
    let patch = response
        .patch
        .map(|patch| serde_json::from_slice(&patch))
        .transpose()
        .context("failed to deserialize patch")?;

    // Apply patch
    let object = if let Some(patch) = patch {
        let object = std::mem::take(&mut request.object);
        let object = object
            .map(|object| -> Result<_> {
                let mut value =
                    serde_json::to_value(object).context("failed to serialize request object")?;
                json_patch::patch(&mut value, &patch).context("failed to apply patch")?;
                serde_json::from_value(value).context("failed to deserialize patched object")
            })
            .transpose()
            .context("failed to patch object")?;
        request.object = object.clone();

        object
    } else {
        request.object.clone()
    };

    Ok(CaseResult {
        allowed: response.allowed,
        message: response.result.message,
        final_object: object,
    })
}

async fn run_validating_rule(
    rule: &ValidatingRule,
    request: &AdmissionRequest<DynamicObjectWithOptionalMetadata>,
    lua_app_data: LuaContextAppData,
) -> Result<CaseResult> {
    let lua = prepare_lua_ctx(lua_app_data).context("failed to prepare Lua context")?;

    let response = validate(&rule.spec.0, request, lua)
        .await
        .context("failed to validate")?;

    Ok(CaseResult {
        allowed: response.allowed,
        message: response.result.message,
        final_object: request.object.clone(),
    })
}

#[derive(Clone)]
struct LuaContextAppData {
    kube_get_stub_map: Arc<HashMap<KubeGetArgument, Option<DynamicObjectWithOptionalMetadata>>>,
    kube_list_stub_map:
        Arc<HashMap<KubeListArgument, ObjectList<DynamicObjectWithOptionalMetadata>>>,
}

/// Prepare test Lua context with stubs
fn prepare_lua_ctx(app_data: LuaContextAppData) -> Result<Lua> {
    let lua = Lua::new();
    lua.set_app_data(app_data);

    lua.sandbox(true)
        .context("failed to set sandbox mode for Lua context")?;

    register_lua_helper_functions(&lua).context("failed to register Lua helper functions")?;

    {
        let globals = lua.globals();

        let kube_get = lua
            .create_function(lua_kube_get_stub)
            .context("failed to create Lua helper function")?;
        globals
            .set("kubeGet", kube_get)
            .context("failed to set Lua helper function")?;
        let kube_list = lua
            .create_function(lua_kube_list_stub)
            .context("failed to create Lua helper function")?;
        globals
            .set("kubeList", kube_list)
            .context("failed to set Lua helper function")?;
    }

    Ok(lua)
}

fn lua_kube_get_stub<'lua>(
    lua: &'lua Lua,
    argument: mlua::Value<'lua>,
) -> mlua::Result<mlua::Value<'lua>> {
    let args: KubeGetArgument = lua.from_value(argument)?;
    let app_data = lua.app_data_ref::<LuaContextAppData>().unwrap();
    let output = app_data
        .kube_get_stub_map
        .get(&args)
        .ok_or_else(|| mlua::Error::external(anyhow!("kubeGet stub not found for {:?}", args)))?;
    lua.to_value_with(
        output,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}

fn lua_kube_list_stub<'lua>(
    lua: &'lua Lua,
    argument: mlua::Value<'lua>,
) -> mlua::Result<mlua::Value<'lua>> {
    let args: KubeListArgument = lua.from_value(argument)?;
    let app_data = lua.app_data_ref::<LuaContextAppData>().unwrap();
    let output = app_data
        .kube_list_stub_map
        .get(&args)
        .ok_or_else(|| mlua::Error::external(anyhow!("kubeList stub not found for {:?}", args)))?;
    lua.to_value_with(
        output,
        mlua::SerializeOptions::new()
            .serialize_none_to_null(false)
            .serialize_unit_to_null(false),
    )
}
