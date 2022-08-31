//! CRD generator
//!
//! Usage: `cargo run --bin crdgen > helm/template/customresourcedefinition.yaml`

use std::collections::BTreeMap;

use itertools::Itertools;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::CustomResourceExt;

use checkpoint::types::rule::{MutatingRule, ValidatingRule};

static LABEL_PLACEHOLDER: &str = "CHECKPOINT_LABEL_PLACEHOLDER";
static LABEL_REPLACE_TARGET: &str = "    {{- include \"checkpoint.labels\" . | nindent 4 }}";

fn main() {
    let mut crds = vec![ValidatingRule::crd(), MutatingRule::crd()];

    println!("# This file is autogenerated by `src/bin/crdgen.rs`");
    for crd in crds.iter_mut() {
        add_label_placeholder(crd);
        let yaml_string = serde_yaml::to_string(crd).unwrap();
        let yaml_string = replace_placeholder(yaml_string);
        println!("{}", yaml_string);
    }
}

fn add_label_placeholder(crd: &mut CustomResourceDefinition) {
    if let Some(map) = &mut crd.metadata.labels {
        map.insert(LABEL_PLACEHOLDER.into(), LABEL_PLACEHOLDER.into());
    } else {
        let mut map = BTreeMap::<String, String>::new();
        map.insert(LABEL_PLACEHOLDER.into(), LABEL_PLACEHOLDER.into());
        crd.metadata.labels = Some(map);
    }
}

fn replace_placeholder(yaml_string: String) -> String {
    yaml_string
        .split("\n")
        .map(|line| {
            if line.contains(LABEL_PLACEHOLDER) {
                LABEL_REPLACE_TARGET
            } else {
                line
            }
        })
        .join("\n")
}
