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
    }

    if surface.root_id.trim().is_empty() || !seen_ids.contains(&surface.root_id) {
        if let Some(first) = surface.components.first() {
            surface.root_id = first.id.clone();
        }
    }
    if surface.surface_id.trim().is_empty() {
        surface.surface_id = format!("surf_{}", surface.root_id);
    }
    Ok(surface)
}
