use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;

pub const ALLOWED_A2UI_COMPONENTS: &[&str] = &[
    "Card", "Column", "Row", "Tabs", "Collapse", "Divider", "Badge", "Text", "PhotoItem",
    "Chart", "Heatmap", "Metric", "Progress", "Table", "Timeline",
    "Button", "ActionGroup", "Checkbox", "CheckboxGroup", "Switch", "ChoiceChips", "Slider", "Stepper", "TextField",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum A2uiTriggerPolicy {
    Immediate {
        #[serde(default = "default_action_name")]
        action: String,
    },
    Debounce {
        #[serde(default = "default_debounce_ms")]
        delay_ms: u64,
        #[serde(default = "default_action_name")]
        action: String,
    },
    OnAllChecked {
        #[serde(default)]
        path: String,
        #[serde(default = "default_action_name")]
        action: String,
    },
    Submit {
        #[serde(default = "default_submit_label")]
        submit_label: String,
        #[serde(default = "default_action_name")]
        action: String,
    },
}

fn default_action_name() -> String {
    "ui_response".to_string()
}
fn default_debounce_ms() -> u64 {
    1400
}
fn default_submit_label() -> String {
    "选好啦".to_string()
}

impl Default for A2uiTriggerPolicy {
    fn default() -> Self {
        Self::Submit {
            submit_label: default_submit_label(),
            action: default_action_name(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2uiComponentNode {
    pub id: String,
    pub component: String,
    #[serde(default)]
    pub children: Vec<String>,
    #[serde(default)]
    pub bind_path: Option<String>,
    #[serde(default)]
    pub props: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2uiSurfacePayload {
    pub surface_id: String,
    #[serde(default)]
    pub title: Option<String>,
    pub root_id: String,
    pub components: Vec<A2uiComponentNode>,
    #[serde(default)]
    pub data_model: Map<String, Value>,
    #[serde(default)]
    pub trigger: A2uiTriggerPolicy,
    #[serde(default)]
    pub submitted: bool,
    #[serde(default)]
    pub submitted_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2uiUserAction {
    pub surface_id: String,
    pub action_name: String,
    #[serde(default)]
    pub component_id: Option<String>,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(default)]
    pub data_model: Map<String, Value>,
    #[serde(default)]
    pub summary_label: Option<String>,
}

pub fn validate_and_sanitize_surface(
    mut surface: A2uiSurfacePayload,
) -> Result<A2uiSurfacePayload, String> {
    if surface.components.is_empty() {
        return Err("A2UI surface 至少需要包含 1 个组件节点".to_string());
    }
    if surface.components.len() > 64 {
        return Err("单个 A2UI surface 组件节点不得超过 64 个".to_string());
    }
    let allowed: HashSet<&str> = ALLOWED_A2UI_COMPONENTS.iter().copied().collect();
    let mut seen_ids = HashSet::with_capacity(surface.components.len());

    for node in &mut surface.components {
        let comp = node.component.trim();
        if !allowed.contains(comp) {
            return Err(format!("不允许的 A2UI 组件类型 `{comp}`"));
        }
        if node.id.trim().is_empty() || !seen_ids.insert(node.id.clone()) {
            return Err(format!("非法或重复的 A2UI 节点 id `{}`", node.id));
        }
        node.props.remove("dangerouslySetInnerHTML");
        node.props.remove("html");
        scrub_dangerous_urls_in_map(&mut node.props);
    }
    scrub_dangerous_urls_in_map(&mut surface.data_model);

    if surface.root_id.trim().is_empty() || !seen_ids.contains(&surface.root_id) {
        if let Some(first) = surface.components.first() {
            surface.root_id = first.id.clone();
        }
    }

    // Reject cyclic or overly deep (> 16 levels) component graphs so recursive UI renderers never stack-overflow
    let children_map: std::collections::HashMap<&str, &[String]> = surface
        .components
        .iter()
        .map(|n| (n.id.as_str(), n.children.as_slice()))
        .collect();
    let mut active_stack = HashSet::new();
    validate_acyclic_depth(&surface.root_id, &children_map, &mut active_stack, 0)?;

    let generic_sid = surface.surface_id.trim();
    if generic_sid.is_empty() || generic_sid == "surf_root" || generic_sid == "root" {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        surface.surface_id = format!("surf_{}_{:x}", surface.root_id, now_ns & 0xFFFF_FFFF);
    }

    let approx_bytes = serde_json::to_vec(&surface).map(|v| v.len()).unwrap_or(0);
    if approx_bytes > 48 * 1024 {
        return Err("单个 A2UI surface 载荷过大（上限 48KB）".to_string());
    }

    Ok(surface)
}

fn validate_acyclic_depth<'a>(
    node_id: &'a str,
    children_map: &std::collections::HashMap<&'a str, &'a [String]>,
    visiting: &mut HashSet<&'a str>,
    depth: usize,
) -> Result<(), String> {
    if depth > 16 {
        return Err("A2UI 组件嵌套层级过深（上限 16 层）".to_string());
    }
    if !visiting.insert(node_id) {
        return Err(format!("检测到循环引用的 A2UI 节点 `{node_id}`"));
    }
    if let Some(children) = children_map.get(node_id) {
        for child_id in *children {
            if let Some((&canonical_id, _)) = children_map.get_key_value(child_id.as_str()) {
                validate_acyclic_depth(canonical_id, children_map, visiting, depth + 1)?;
            }
        }
    }
    visiting.remove(node_id);
    Ok(())
}

fn scrub_dangerous_urls_in_map(map: &mut Map<String, Value>) {
    for val in map.values_mut() {
        scrub_dangerous_urls_in_value(val);
    }
}

fn scrub_dangerous_urls_in_value(val: &mut Value) {
    match val {
        Value::String(s) => {
            let lower = s.trim_start().to_ascii_lowercase();
            if lower.starts_with("javascript:") || lower.starts_with("vbscript:") {
                s.clear();
            }
        }
        Value::Array(arr) => {
            for item in arr {
                scrub_dangerous_urls_in_value(item);
            }
        }
        Value::Object(m) => scrub_dangerous_urls_in_map(m),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_cyclic_a2ui_component_graph_and_scrubs_js_urls() {
        let cyclic: A2uiSurfacePayload = serde_json::from_value(json!({
            "surface_id": "",
            "root_id": "a",
            "components": [
                { "id": "a", "component": "Column", "children": ["b"] },
                { "id": "b", "component": "Row", "children": ["a"] }
            ]
        }))
        .unwrap();
        assert!(validate_and_sanitize_surface(cyclic).is_err());

        let valid: A2uiSurfacePayload = serde_json::from_value(json!({
            "surface_id": "surf_root",
            "root_id": "root",
            "components": [
                {
                    "id": "root",
                    "component": "Card",
                    "props": { "href": "javascript:alert(1)", "title": "今日推荐" }
                }
            ]
        }))
        .unwrap();
        let cleaned = validate_and_sanitize_surface(valid).unwrap();
        assert_ne!(cleaned.surface_id, "surf_root");
        assert_eq!(cleaned.components[0].props["href"], json!(""));
        assert_eq!(cleaned.components[0].props["title"], json!("今日推荐"));
    }
}
