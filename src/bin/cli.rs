use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use itertools::Itertools;
use json_patch::PatchOperation;
use kube::{
    core::{admission::AdmissionRequest, DynamicObject, ObjectList},
    ResourceExt,
};
use tracing::Instrument;

use checkpoint::{
    checker::fetch_resources,
    handler::{
        js::helper::{KubeGetArgument, KubeListArgument, KubeListArgumentListParamsVersionMatch},
        mutate, validate,
    },
    js::eval,
    types::{
        policy::CronPolicy,
        rule::{MutatingRule, ValidatingRule},
        testcase::{Case, TestCase},
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
    Check(CheckArgs),
}

#[derive(Args, Debug)]
struct TestArgs {
    #[clap(value_parser)]
    test_case_paths: Vec<PathBuf>,
}

#[derive(Args, Debug)]
struct CheckArgs {
    #[clap(value_parser)]
    cron_policy_paths: Vec<PathBuf>,
}

#[derive(Debug)]
struct CaseResult {
    allowed: bool,
    message: String,
    final_object: Option<DynamicObject>,
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
        Commands::Check(args) => cli_check(args).await,
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
                    "failed to test for test case file `{}`",
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
        message: String::new(),
        final_object: request.object.clone(),
    };

    for rule in mutating_rules {
        let rule_name = rule
            .metadata
            .name
            .as_ref()
            .ok_or_else(|| anyhow!("rule does not have name"))?;
        let rule_span = tracing::info_span!("mutating-rule", rule = rule_name);

        actual = run_mutating_rule(rule, &mut request, &kube_get_stub_map, &kube_list_stub_map)
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

        actual = run_validating_rule(rule, &request, &kube_get_stub_map, &kube_list_stub_map)
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
    request: &mut AdmissionRequest<DynamicObject>,
    kube_get: &HashMap<KubeGetArgument, Option<DynamicObject>>,
    kube_list: &HashMap<KubeListArgument, ObjectList<DynamicObject>>,
) -> Result<CaseResult> {
    let js_context = prepare_js_context_for_test_case(kube_get, kube_list)
        .context("failed to prepare JavaScript stub code")?;

    let response = mutate(&rule.spec.0, request, js_context)
        .await
        .context("failed to mutate")?;
    let patch = response
        .patch
        .map(|patch| serde_json::from_slice::<Vec<PatchOperation>>(&patch))
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
    request: &AdmissionRequest<DynamicObject>,
    kube_get: &HashMap<KubeGetArgument, Option<DynamicObject>>,
    kube_list: &HashMap<KubeListArgument, ObjectList<DynamicObject>>,
) -> Result<CaseResult> {
    let js_context = prepare_js_context_for_test_case(kube_get, kube_list)
        .context("failed to prepare JavaScript stub code")?;

    let response = validate(&rule.spec.0, request, js_context)
        .await
        .context("failed to validate")?;

    Ok(CaseResult {
        allowed: response.allowed,
        message: response.result.message,
        final_object: request.object.clone(),
    })
}

/// Prepare test JS context with stubs
fn prepare_js_context_for_test_case(
    kube_get: &HashMap<KubeGetArgument, Option<DynamicObject>>,
    kube_list: &HashMap<KubeListArgument, ObjectList<DynamicObject>>,
) -> Result<String> {
    let mut code = r#"function kubeGet(args) {
    if (false) {
        // Nothing
    }"#
    .to_string();

    // Populate kubeGet
    for (args, object) in kube_get {
        code += &format!(
            r#" else if (args.kind === "{}" && args.version === "{}" && {} && {} && args.name === "{}") {{
        return {};
    }}"#,
            args.kind,
            args.version,
            if let Some(plural) = &args.plural {
                format!("args.plural === \"{}\"", plural)
            } else {
                "args.plural === undefined".to_string()
            },
            if let Some(namespace) = &args.namespace {
                format!("args.namespace === \"{}\"", namespace)
            } else {
                "args.namespace === undefined".to_string()
            },
            args.name,
            serde_json::to_string(&object).context("failed to serialize Kubernetes object")?,
        );
    }

    code += r#" else {
        throw new Error("kubeGet stub not found");
    }
}
function kubeList(args) {
    if (false) {
        // Nothing
    }"#;

    // Populate kubeList
    for (args, object_list) in kube_list {
        code += &format!(
            r#" else if (args.kind === "{}" && args.version === "{}" && {} && {} && {}) {{
        return {};
    }}"#,
            args.kind,
            args.version,
            if let Some(plural) = &args.plural {
                format!("args.plural === \"{}\"", plural)
            } else {
                "args.plural === undefined".to_string()
            },
            if let Some(namespace) = &args.namespace {
                format!("args.namespace === \"{}\"", namespace)
            } else {
                "args.namespace === undefined".to_string()
            },
            if let Some(list_params) = &args.list_params {
                format!(
                    "{} && {} && {} && {} && {} && {} && {}",
                    if let Some(label_selector) = &list_params.label_selector {
                        format!("args.listParams.labelSelector === \"{}\"", label_selector)
                    } else {
                        "args.listParams.labelSelector === undefined".to_string()
                    },
                    if let Some(field_selector) = &list_params.field_selector {
                        format!("args.listParams.fieldSelector === \"{}\"", field_selector)
                    } else {
                        "args.listParams.fieldSelector === undefined".to_string()
                    },
                    if let Some(timeout) = list_params.timeout {
                        format!("args.listParams.timeout === {}", timeout)
                    } else {
                        "args.listParams.timeout === undefined".to_string()
                    },
                    if let Some(limit) = list_params.limit {
                        format!("args.listParams.limit === {}", limit)
                    } else {
                        "args.listParams.limit === undefined".to_string()
                    },
                    if let Some(continue_token) = &list_params.continue_token {
                        format!("args.listParams.continueToken === {}", continue_token)
                    } else {
                        "args.listParams.continueToken === undefined".to_string()
                    },
                    if let Some(version_match) = &list_params.version_match {
                        format!(
                            "args.listParams.versionMatch === {}",
                            match version_match {
                                KubeListArgumentListParamsVersionMatch::NotOlderThan =>
                                    "NotOlderThan",
                                KubeListArgumentListParamsVersionMatch::Exact => "Exact",
                            }
                        )
                    } else {
                        "args.listParams.versionMatch === undefined".to_string()
                    },
                    if let Some(resource_version) = &list_params.resource_version {
                        format!("args.listParams.resourceVersion === {}", resource_version)
                    } else {
                        "args.listParams.resourceVersion === undefined".to_string()
                    },
                )
            } else {
                "(args.list_params === undefined || Object.keys(args.list_params).length === 0)"
                    .to_string()
            },
            serde_json::to_string(&object_list)
                .context("failed to serialize Kubernetes object list")?,
        );
    }

    code += r#" else {
        throw new Error("kubeList stub not found");
    }
}
"#;

    Ok(code)
}

async fn cli_check(args: CheckArgs) -> Result<()> {
    for cronpolicy_path in args.cron_policy_paths {
        let cronpolicy_path_span =
            tracing::info_span!("cronpolicy-file", path = %cronpolicy_path.display());
        check_cronpolicy_path(&cronpolicy_path)
            .instrument(cronpolicy_path_span)
            .await
            .with_context(|| {
                format!(
                    "failed to check for cronpolicy file `{}`",
                    cronpolicy_path.display()
                )
            })?;
    }
    Ok(())
}

async fn check_cronpolicy_path(cronpolicy_path: &Path) -> Result<()> {
    // Open and deserialize cronpolicy file
    let cronpolicy_file =
        fs::File::open(cronpolicy_path).context("failed to open cronpolicy file")?;
    let cronpolicy: CronPolicy =
        serde_yaml::from_reader(cronpolicy_file).context("failed to deserialize cronpolicy")?;

    let cronpolicy_name = cronpolicy.name_any();

    let cronpolicy_span = tracing::info_span!("cronpolicy", name = %cronpolicy_name);
    check_cronpolicy(cronpolicy)
        .instrument(cronpolicy_span)
        .await
        .with_context(|| format!("faild to check for cronpolicy `{}`", cronpolicy_name))?;

    Ok(())
}

async fn check_cronpolicy(cronpolicy: CronPolicy) -> Result<()> {
    let kube_config = kube::Config::infer()
        .await
        .context("failed to infer Kubernetes config")?;
    let kube_client: kube::Client = kube_config
        .try_into()
        .context("failed to make Kubernetes client")?;

    let resources = fetch_resources(kube_client, &cronpolicy.spec.resources).await?;

    let mut js_runtime = checkpoint::checker::prepare_js_runtime(resources)
        .context("failed to prepare JavaScript runtime")?;

    js_runtime
        .execute_script("<checkpoint>", cronpolicy.spec.code.into())
        .context("failed to execute JavaScript code")?;

    let output: Option<HashMap<String, String>> =
        eval(&mut js_runtime, "__checkpoint_get_context(\"output\")")
            .context("failed to evaluate JavaScript code")?;

    if let Some(output) = output {
        tracing::error!(output = ?output, "JavaScript code exited with output");
        Err(anyhow!("JavaScript code exited with output: {:?}", output))
    } else {
        tracing::info!("JavaScript code exited with no output");
        Ok(())
    }
}
