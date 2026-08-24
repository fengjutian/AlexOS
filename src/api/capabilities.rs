//! Desktop capability registry loaded from the same schema shipped by the SDK.

use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Schema {
    capabilities: CapabilityGroups,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CapabilityGroups {
    always: Vec<String>,
    native_desktop: Vec<String>,
    experimental: Vec<String>,
}

static SCHEMA: OnceLock<Schema> = OnceLock::new();

fn schema() -> &'static Schema {
    SCHEMA.get_or_init(|| {
        serde_json::from_str(include_str!("../../packages/sdk/desktop-api.schema.json"))
            .expect("packages/sdk/desktop-api.schema.json must be valid")
    })
}

pub fn available(native_desktop: bool) -> Vec<String> {
    let schema = schema();
    let mut result = schema.capabilities.always.clone();
    if native_desktop { result.extend(schema.capabilities.native_desktop.clone()); }
    result
}

pub fn experimental() -> Vec<String> { schema().capabilities.experimental.clone() }

