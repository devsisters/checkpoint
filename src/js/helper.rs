//! JS common helper functions

use deno_core::op;
use json_patch::Patch;

deno_core::extension!(
    checkpoint_common,
    ops = [ops_print, ops_jsonpatch_diff, ops_json_clone],
);

/// JS helper function to debug-print JS value with JSON format
#[op]
fn ops_print(v: serde_json::Value) {
    tracing::info!(
        "debug print fron JS code: {}",
        serde_json::to_string(&v).expect("failed to serialize JSON value"),
    );
}

/// JS helper function to generate jsonpatch with diff of two table
#[op]
fn ops_jsonpatch_diff(v1: serde_json::Value, v2: serde_json::Value) -> Patch {
    json_patch::diff(&v1, &v2)
}

/// JS helper function to clone object as JSON value
#[op]
fn ops_json_clone(value: serde_json::Value) -> serde_json::Value {
    value
}
