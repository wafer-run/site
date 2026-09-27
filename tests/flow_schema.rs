//! The WaferFlow JSON Schema and the flow examples in the docs, held to
//! wafer-run's flow model.
//!
//! `public/schema/waferflow/v0.1.0/flow.schema.json` is generated from
//! `wafer_flow`'s types (`wafer_flow::json_schema()`), not written by hand.
//! After a wafer-run change to the flow document, regenerate it with
//!
//! ```text
//! WAFER_SITE_REGENERATE_FLOW_SCHEMA=1 cargo test --test flow_schema
//! ```
//!
//! and review the diff (the docs in `content/docs/waferflow-spec.html`
//! describe the same fields).

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Where the schema is served from, relative to the site root.
const SCHEMA_PATH: &str = "public/schema/waferflow/v0.1.0/flow.schema.json";

/// The schema's `$id`: the URL it is published at.
const SCHEMA_ID: &str = "https://wafer.run/schema/waferflow/v0.1.0/flow.schema.json";

/// Docs pages whose `<pre><code>` flow documents must parse and validate.
const FLOW_DOC_PAGES: &[&str] = &[
    "content/docs/waferflow.html",
    "content/docs/waferflow-spec.html",
    "content/docs/waferflow-examples.html",
    "content/docs/waferflow-blocks.html",
];

fn site_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// The schema as the site publishes it: wafer-flow's schema plus the `$id`
/// naming its URL.
fn generated_schema() -> Value {
    let mut schema = wafer_flow::json_schema();
    schema
        .as_object_mut()
        .expect("the schema is a JSON object")
        .insert("$id".to_string(), Value::String(SCHEMA_ID.to_string()));
    schema
}

#[test]
fn published_flow_schema_is_generated_from_wafer_flow() {
    let path = site_path(SCHEMA_PATH);
    let generated = generated_schema();
    if std::env::var_os("WAFER_SITE_REGENERATE_FLOW_SCHEMA").is_some() {
        let mut text = serde_json::to_string_pretty(&generated).expect("serialize schema");
        text.push('\n');
        std::fs::write(&path, text).expect("write schema");
        return;
    }
    let published: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read the published schema"))
            .expect("the published schema is JSON");
    assert!(
        published == generated,
        "{SCHEMA_PATH} differs from wafer_flow::json_schema(); regenerate it with \
         WAFER_SITE_REGENERATE_FLOW_SCHEMA=1 cargo test --test flow_schema"
    );
}

/// Undo the HTML escaping a `<pre><code>` block carries.
fn unescape_html(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Every `<pre><code>` block on `page` that is a complete flow document: a
/// JSON object with `id` and `steps`. Blocks that are fragments or
/// annotated type sketches are not JSON and are skipped.
fn flow_documents(page: &str) -> Vec<String> {
    let html = std::fs::read_to_string(site_path(page)).expect("read docs page");
    let mut documents = Vec::new();
    let mut rest = html.as_str();
    while let Some(start) = rest.find("<pre><code>") {
        rest = &rest[start + "<pre><code>".len()..];
        let end = rest
            .find("</code></pre>")
            .expect("an unterminated <pre><code>");
        let text = unescape_html(&rest[..end]);
        rest = &rest[end..];
        if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(&text) {
            if object.contains_key("id") && object.contains_key("steps") {
                documents.push(text);
            }
        }
    }
    documents
}

#[test]
fn docs_flow_examples_parse_and_validate() {
    let mut checked = 0;
    for page in FLOW_DOC_PAGES {
        for document in flow_documents(page) {
            let flow = wafer_flow::parse(&document)
                .unwrap_or_else(|e| panic!("a flow example in {page} does not parse: {e}"));
            if let Err(errors) = wafer_flow::validate(&flow) {
                panic!(
                    "flow example {:?} in {page} is invalid: {errors:?}",
                    flow.id
                );
            }
            checked += 1;
        }
    }
    // Guards the extraction: the examples page alone holds several flows.
    assert!(
        checked >= 3,
        "found only {checked} flow examples in the docs"
    );
}

#[test]
fn site_flow_parses_and_validates() {
    let flow = wafer_flow::parse(wafer_site::flows::site::JSON).expect("the site flow parses");
    wafer_flow::validate(&flow).expect("the site flow validates");
}
