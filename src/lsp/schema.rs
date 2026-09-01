//! Keeping the JSON Schemas honest.
//!
//! The schemas in `editors/schemas/` are hand-written, and they describe the
//! same types as the serde structs in this crate. Two descriptions of one
//! format drift — a field is added here and never reaches the schema, and the
//! editor quietly starts flagging valid config as an error.
//!
//! So this compares them, in both directions, for every type the schemas
//! cover. The Rust side of each comparison is a struct literal with no
//! `..Default::default()`, which means adding a field anywhere in the config
//! types stops this file from compiling until someone has looked at the schema.

#![cfg(test)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

/// The schema directory, or `None` in a published crate — `Cargo.toml` ships
/// `src/` and not `editors/`, so the files this checks against aren't always
/// there. A missing directory means "nothing to check", not a failure.
fn schemas() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("editors/schemas");
    dir.is_dir().then_some(dir)
}

fn load(file: &str) -> Value {
    let path = schemas().expect("checked by the caller").join(file);
    let text = std::fs::read_to_string(&path).expect("schema file should be readable");
    serde_json::from_str(&text).expect("schema file should be valid JSON")
}

/// The property names an object node in a schema declares.
fn declared(schema: &Value, pointer: &str) -> BTreeSet<String> {
    schema
        .pointer(pointer)
        .unwrap_or_else(|| panic!("schema has no node at {pointer}"))
        .as_object()
        .unwrap_or_else(|| panic!("{pointer} is not an object"))
        .keys()
        .cloned()
        .collect()
}

/// The field names serde emits for a fully-populated value.
///
/// Every field must be set to something `skip_serializing_if` won't drop, or
/// it won't appear here and the comparison will report it as missing from the
/// Rust side — which is the failure mode this is built to catch, so it is
/// worth the verbosity at the call sites.
fn serialized<T: Serialize>(value: &T) -> BTreeSet<String> {
    serde_json::to_value(value)
        .expect("config types are serializable")
        .as_object()
        .expect("a struct serializes to an object")
        .keys()
        .cloned()
        .collect()
}

/// Compare one type against one schema node, naming both sides of any gap.
fn agree<T: Serialize>(what: &str, value: &T, schema: &Value, pointer: &str) {
    let rust = serialized(value);
    let json = declared(schema, pointer);

    let missing_from_schema: Vec<_> = rust.difference(&json).collect();
    let missing_from_rust: Vec<_> = json.difference(&rust).collect();

    assert!(
        missing_from_schema.is_empty(),
        "{what}: these fields exist in Rust but not in the schema at {pointer}: {missing_from_schema:?}\n\
         Add them to editors/schemas/, or the editor will flag valid config as an error."
    );
    assert!(
        missing_from_rust.is_empty(),
        "{what}: the schema at {pointer} declares fields ciabatta does not read: {missing_from_rust:?}\n\
         Remove them from editors/schemas/, or the editor will offer config that does nothing."
    );
}

fn one<T: From<&'static str>>() -> T {
    T::from("x")
}

fn map() -> BTreeMap<String, String> {
    [("K".to_string(), "v".to_string())].into_iter().collect()
}

fn list() -> Vec<String> {
    vec!["x".to_string()]
}

fn cache() -> crate::cache::CacheConfig {
    crate::cache::CacheConfig {
        enabled: Some(true),
        inputs: list(),
        outputs: list(),
        env: list(),
        exclude: list(),
        remote: Some(remote()),
    }
}

fn remote() -> crate::cache::RemoteRef {
    crate::cache::RemoteRef {
        url: one(),
        name: Some(one()),
        project: Some(one()),
        read_only: true,
        tls_verify: true,
        enabled: true,
    }
}

fn step() -> crate::run::RunStep {
    crate::run::RunStep {
        name: one(),
        script: Some(one()),
        run: Some(one()),
        description: Some(one()),
        owner: Some(one()),
        tags: list(),
        requires: list(),
        timeout: Some(one()),
        retries: 1,
        persistent: true,
        continue_on_error: true,
        kind: Some(one()),
        registry: Some(one()),
        artifact: Some(one()),
        local_image: Some(one()),
        publish_path: Some(crate::config::PublishPath::Single(one())),
        strip_prefix: Some(one()),
        from: Some(one()),
        cwd: Some(one()),
        workspace: Some(one()),
        env: map(),
        env_files: list(),
        needs: list(),
        on_error: Some(one()),
        when: list(),
        skip_if: list(),
        recover: true,
        message: Some(one()),
        retry: Some(one()),
        options: vec![fix_option()],
        cache: Some(cache()),
        // Not a config key — `#[serde(skip)]`, set by the compiler on entries
        // of a workflow's `background:` array — so the schemas don't name it.
        background: false,
    }
}

fn fix_option() -> crate::run::FixOption {
    crate::run::FixOption {
        label: one(),
        script: Some(one()),
        run: Some(one()),
        default: true,
    }
}

fn workflow() -> crate::workspace::Workflow {
    crate::workspace::Workflow {
        description: Some(one()),
        owner: Some(one()),
        needs: list(),
        requires: list(),
        env_file: list(),
        required_env: list(),
        env: map(),
        tags: list(),
        cache: Some(cache()),
        steps: vec![step()],
        background: vec![step()],
    }
}

fn workspace_meta() -> crate::workspace::WorkspaceMeta {
    crate::workspace::WorkspaceMeta {
        name: Some(one()),
        description: Some(one()),
        owner: Some(one()),
        depends_on: list(),
        tags: list(),
        requires: list(),
        env_file: list(),
        env_default: Some(one()),
        env: map(),
        umbrella: true,
    }
}

fn tool_spec() -> crate::workspace::ToolSpec {
    crate::workspace::ToolSpec {
        hint: Some(one()),
        check: Some(one()),
        description: Some(one()),
    }
}

fn registry() -> crate::config::RegistryConfig {
    crate::config::RegistryConfig {
        url: one(),
        tls_verify: true,
        needs_auth: true,
        login_script: Some(one()),
        registry_type: Some(one()),
        repository: Some(one()),
        base_path: Some(one()),
        format: Some(one()),
    }
}

fn ai() -> crate::config::AiConfig {
    crate::config::AiConfig {
        provider: Some(one()),
        endpoint: Some(one()),
        model: Some(one()),
        api_key_env: Some(one()),
        tls_verify: true,
        images: list(),
        verify: Some(one()),
        max_tokens: Some(1),
        max_tool_rounds: Some(1),
    }
}

fn config() -> crate::config::CiabattaConfig {
    crate::config::CiabattaConfig {
        system: Some(crate::config::SystemConfig {
            ci: Some(one()),
            containers: Some(one()),
        }),
        workspace: Some(workspace_meta()),
        workflows: HashMap::from([("build".to_string(), workflow())]),
        toolchain: HashMap::from([("cargo".to_string(), tool_spec())]),
        registries: HashMap::from([("nexus".to_string(), registry())]),
        analyze: Some(crate::config::AnalyzeConfig {
            requirements: Some(one()),
            trace: Some(one()),
        }),
        ai: Some(ai()),
        cache: Some(cache()),
    }
}

#[test]
fn the_schemas_describe_exactly_the_fields_ciabatta_reads() {
    let Some(_) = schemas() else {
        return; // a published crate ships src/ without editors/
    };
    let root = load("ciabatta.schema.json");
    let flow = load("workflow.schema.json");
    let common = load("common.schema.json");

    agree("CiabattaConfig", &config(), &root, "/properties");
    agree(
        "WorkspaceMeta",
        &workspace_meta(),
        &root,
        "/properties/workspace/properties",
    );
    agree(
        "ToolSpec",
        &tool_spec(),
        &root,
        "/properties/toolchain/additionalProperties/properties",
    );
    agree(
        "RegistryConfig",
        &registry(),
        &root,
        "/properties/registries/additionalProperties/properties",
    );
    agree("AiConfig", &ai(), &root, "/properties/ai/properties");
    agree("Workflow", &workflow(), &flow, "/properties");
    agree("RunStep", &step(), &common, "/$defs/step/properties");
    agree(
        "FixOption",
        &fix_option(),
        &common,
        "/$defs/fixOption/properties",
    );
    agree("CacheConfig", &cache(), &common, "/$defs/cache/properties");
    agree(
        "RemoteRef",
        &remote(),
        &common,
        "/$defs/remoteCache/properties",
    );
}

/// The `ai:` block is the one section left open (`additionalProperties` is not
/// forbidden there), because it grows a field per provider and a schema that
/// lags behind would reject working config. Everything else is closed, which
/// is what turns a typo into an error instead of a silently ignored line.
#[test]
fn every_section_but_ai_rejects_unknown_fields() {
    let Some(_) = schemas() else { return };
    let root = load("ciabatta.schema.json");
    let flow = load("workflow.schema.json");
    let common = load("common.schema.json");

    for (name, schema, pointer) in [
        ("config", &root, ""),
        ("workspace", &root, "/properties/workspace"),
        ("workflow", &flow, ""),
        ("step", &common, "/$defs/step"),
        ("cache", &common, "/$defs/cache"),
    ] {
        let node = schema.pointer(pointer).expect("node should exist");
        assert_eq!(
            node.get("additionalProperties"),
            Some(&Value::Bool(false)),
            "{name} should reject unknown fields",
        );
    }

    assert!(
        root.pointer("/properties/ai/additionalProperties")
            .is_none(),
        "the ai: block is deliberately open",
    );
}
