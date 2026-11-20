#![deny(clippy::indexing_slicing)]

pub mod enums {
    #[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, Default)]
    #[serde(rename_all = "snake_case")]
    pub enum McpInitStrategy {
        #[default]
        Blocking,
        Progressive,
    }

    impl<S: AsRef<str>> From<S> for McpInitStrategy {
        fn from(s: S) -> Self {
            match s.as_ref() {
                "progressive" => McpInitStrategy::Progressive,
                _ => McpInitStrategy::Blocking,
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    pub enum PrCreationSource {
        Bash,
        Mcp,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, strum::IntoStaticStr)]
    #[serde(rename_all = "snake_case")]
    #[strum(serialize_all = "snake_case")]
    pub enum PermissionMode {
        #[default]
        Ask,
        AlwaysApprove,
        Auto,
    }

    impl PermissionMode {
        pub fn is_always_approve(self) -> bool {
            matches!(self, Self::AlwaysApprove)
        }

        pub fn is_auto(self) -> bool {
            matches!(self, Self::Auto)
        }

        pub fn from_flags(yolo: bool, auto: bool) -> Self {
            match (yolo, auto) {
                (true, _) => Self::AlwaysApprove,
                (false, true) => Self::Auto,
                (false, false) => Self::Ask,
            }
        }
    }
}

pub mod events {
    pub use crate::enums::McpInitStrategy as McpStrategy;
    use serde::Serialize;

    pub trait TelemetryEvent: Serialize {}

    #[derive(Serialize, Clone, Copy, strum::IntoStaticStr)]
    #[serde(rename_all = "snake_case")]
    #[strum(serialize_all = "snake_case")]
    pub enum McpTransport {
        Stdio,
        Sse,
        Http,
    }

    #[derive(Serialize, Clone, Copy, strum::AsRefStr, strum::IntoStaticStr)]
    #[serde(rename_all = "snake_case")]
    #[strum(serialize_all = "snake_case")]
    pub enum McpErrorType {
        Connection,
        Auth,
        Protocol,
        Timeout,
        SpawnFailed,
        HandshakeFailed,
    }

    #[derive(Serialize)]
    pub struct McpServerConnected {
        pub server_name: String,
        pub tool_count: u32,
        pub transport: McpTransport,
        pub duration_ms: u64,
    }
    impl TelemetryEvent for McpServerConnected {}

    #[derive(Serialize)]
    pub struct McpServerFailed {
        pub server_name: String,
        pub error_type: McpErrorType,
        pub duration_ms: u64,
        pub timeout_sec: u64,
        #[serde(skip)]
        pub error_message: Option<String>,
    }
    impl TelemetryEvent for McpServerFailed {}

    #[derive(Serialize)]
    pub struct McpInitCompleted {
        pub total_duration_ms: u64,
        pub spawn_duration_ms: u64,
        pub server_count: u32,
        pub servers_succeeded: u32,
        pub servers_failed: u32,
        pub servers_auth_required: u32,
        pub total_tools_registered: u32,
        pub strategy: McpStrategy,
        pub is_reinit: bool,
    }
    impl TelemetryEvent for McpInitCompleted {}

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum McpCallFailure {
        Timeout,
        Transport,
    }

    #[derive(Serialize)]
    pub struct McpToolCalled {
        pub server_name: String,
        pub tool_name: String,
        pub qualified_name: String,
        pub success: bool,
        pub duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub failure: Option<McpCallFailure>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub outcome: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub mode: Option<String>,
    }
    impl TelemetryEvent for McpToolCalled {}
}

pub mod session_ctx {
    pub fn log_event<E: serde::Serialize>(event: E) {
        if tracing::enabled!(tracing::Level::DEBUG)
            && let Ok(payload) = serde_json::to_string(&event)
        {
            tracing::debug!(target: "agent_kernel_events", event = %payload);
        }
    }

    pub fn emit_event<E: serde::Serialize>(event: E) {
        log_event(event);
    }
}

pub use session_ctx::{emit_event, log_event};

pub mod activity {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Mutex, Once, PoisonError};

    pub const SESSIONS_ACTIVE_KEY: &str = "sessions_active";
    pub const SUBAGENTS_ACTIVE_KEY: &str = "subagents_active";
    pub const COMPACTIONS_ACTIVE_KEY: &str = "compaction_active";
    pub const MCP_SERVERS_CONNECTED_KEY: &str = "mcp_servers_connected";
    pub const TURNS_ACTIVE_KEY: &str = "turns_active";
    pub const WORKFLOW_RUNS_ACTIVE_KEY: &str = "workflow_runs_active";

    static GAUGES: Mutex<Vec<&'static ActivityGauge>> = Mutex::new(Vec::new());

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum GaugeKind {
        Work,
        Residency,
    }

    pub struct ActivityGauge {
        name: &'static str,
        value: AtomicU32,
        kind: GaugeKind,
        registered: Once,
    }

    impl ActivityGauge {
        pub const fn work(name: &'static str) -> Self {
            Self::new(name, GaugeKind::Work)
        }

        pub const fn residency(name: &'static str) -> Self {
            Self::new(name, GaugeKind::Residency)
        }

        const fn new(name: &'static str, kind: GaugeKind) -> Self {
            Self {
                name,
                value: AtomicU32::new(0),
                kind,
                registered: Once::new(),
            }
        }

        pub fn get(&self) -> u32 {
            self.value.load(Ordering::Relaxed)
        }

        fn register(&'static self) {
            self.registered.call_once(|| {
                GAUGES
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(self);
            });
        }

        fn dec(&self) {
            let _ = self
                .value
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                    Some(v.saturating_sub(1))
                });
        }

        pub fn enter(&'static self) -> ActivityGaugeGuard {
            self.register();
            self.value.fetch_add(1, Ordering::Relaxed);
            ActivityGaugeGuard { gauge: self }
        }
    }

    #[must_use]
    pub struct ActivityGaugeGuard {
        gauge: &'static ActivityGauge,
    }

    impl Drop for ActivityGaugeGuard {
        fn drop(&mut self) {
            self.gauge.dec();
        }
    }

    pub fn gauge_value(name: &str) -> u32 {
        GAUGES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|gauge| gauge.name == name)
            .map_or(0, |gauge| gauge.get())
    }

    pub fn work_is_idle() -> bool {
        GAUGES
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|gauge| gauge.kind == GaugeKind::Work)
            .all(|gauge| gauge.get() == 0)
    }
}

pub mod region {
    pub enum Parent<'a> {
        Inherit,
        Explicit(&'a tracing::Span),
        Root,
    }

    #[must_use = "dropping a Region immediately closes its span as a zero-length frame"]
    pub struct Region(tracing::Span);

    impl Region {
        pub fn from_span(span: tracing::Span) -> Self {
            Self(span)
        }

        pub fn close(self) {}

        pub fn span(&self) -> &tracing::Span {
            &self.0
        }
    }
}

#[macro_export]
macro_rules! region {
    ($name:literal, $parent:expr) => {
        $crate::region::Region::from_span(match $parent {
            $crate::region::Parent::Inherit => tracing::info_span!($name),
            $crate::region::Parent::Explicit(span) => tracing::info_span!(parent: span, $name),
            $crate::region::Parent::Root => tracing::info_span!(parent: None, $name),
        })
    };
    (debug, $name:literal, $parent:expr) => {
        $crate::region::Region::from_span(match $parent {
            $crate::region::Parent::Inherit => tracing::debug_span!($name),
            $crate::region::Parent::Explicit(span) => tracing::debug_span!(parent: span, $name),
            $crate::region::Parent::Root => tracing::debug_span!(parent: None, $name),
        })
    };
}

#[macro_export]
macro_rules! instrument_task {
    ($name:literal, $parent:expr, $fut:expr) => {
        tracing::Instrument::instrument($fut, match $parent {
            $crate::region::Parent::Inherit => tracing::info_span!($name),
            $crate::region::Parent::Explicit(span) => tracing::info_span!(parent: span, $name),
            $crate::region::Parent::Root => tracing::info_span!(parent: None, $name),
        })
    };
    (debug, $name:literal, $parent:expr, $fut:expr) => {
        tracing::Instrument::instrument($fut, match $parent {
            $crate::region::Parent::Inherit => tracing::debug_span!($name),
            $crate::region::Parent::Explicit(span) => tracing::debug_span!(parent: span, $name),
            $crate::region::Parent::Root => tracing::debug_span!(parent: None, $name),
        })
    };
}

#[macro_export]
macro_rules! event_span {
    ($($args:tt)+) => {
        tracing::info_span!($($args)+).in_scope(|| {})
    };
}

pub mod startup {
    pub fn is_active() -> bool {
        false
    }
}

pub mod instrumentation {
    use std::time::Instant;

    pub const TARGET: &str = "xai_grok_instrumentation";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum InstrumentationMode {
        Disabled,
        Log,
        Chrome,
        Server,
    }

    pub fn current_mode() -> InstrumentationMode {
        InstrumentationMode::Disabled
    }

    pub struct InstrumentationTimer {
        name: &'static str,
        start: Instant,
        mode: InstrumentationMode,
    }

    impl InstrumentationTimer {
        pub fn new(name: &'static str) -> Self {
            Self::new_with_span(name, current_mode(), None)
        }

        pub fn new_with_span(
            name: &'static str,
            mode: InstrumentationMode,
            _span_guard: Option<tracing::span::EnteredSpan>,
        ) -> Self {
            Self {
                name,
                start: Instant::now(),
                mode,
            }
        }

        pub fn with_field(
            &mut self,
            _key: impl Into<String>,
            _value: impl Into<serde_json::Value>,
        ) -> &mut Self {
            self
        }

        pub fn with_server(&mut self, _server: &str) -> &mut Self {
            self
        }
    }

    impl Drop for InstrumentationTimer {
        fn drop(&mut self) {
            if self.mode == InstrumentationMode::Log {
                let elapsed_us = self.start.elapsed().as_micros() as u64;
                tracing::debug!(
                    target: TARGET,
                    event = "timing",
                    name = self.name,
                    elapsed_us = elapsed_us
                );
            }
        }
    }

    pub fn timer(name: &'static str) -> InstrumentationTimer {
        InstrumentationTimer::new(name)
    }

    pub fn emit_startup_timing(_name: &'static str, _elapsed: std::time::Duration) {}
}

#[macro_export]
#[doc(hidden)]
macro_rules! __startup_step_timer {
    ($active:expr, $mode:expr, $active_name:expr, $idle_name:expr $(,)?) => {{
        let active = $active;
        let mode = $mode;
        let name = if active { $active_name } else { $idle_name };
        $crate::instrumentation::InstrumentationTimer::new_with_span(name, mode, None)
    }};
}

#[macro_export]
macro_rules! startup_step_timer {
    ($active_name:expr, $idle_name:expr $(,)?) => {
        $crate::__startup_step_timer!(
            $crate::startup::is_active(),
            $crate::instrumentation::current_mode(),
            $active_name,
            $idle_name,
        )
    };
}

#[macro_export]
macro_rules! startup_step_timer_grouped {
    ($group:literal, $step:literal $(,)?) => {
        $crate::startup_step_timer!(
            concat!("startup.", $group, ".", $step),
            concat!("session.", $group, ".", $step),
        )
    };
}

#[macro_export]
macro_rules! startup_step_grouped {
    ($group:literal, $step:literal $(, $field:ident = $value:expr)* $(,)?) => {{
        let active = $crate::startup::is_active();
        let mode = $crate::instrumentation::current_mode();
        let timer = $crate::__startup_step_timer!(
            active,
            mode,
            concat!("startup.", $group, ".", $step),
            concat!("session.", $group, ".", $step),
        );
        let span = if active {
            tracing::info_span!(concat!("startup.", $group, ".", $step) $(, $field = $value)*)
        } else {
            tracing::info_span!(concat!("session.", $group, ".", $step) $(, $field = $value)*)
        };
        (timer, span)
    }};
}

#[macro_export]
macro_rules! instrumentation_timer {
    ($name:literal) => {
        $crate::instrumentation::InstrumentationTimer::new($name)
    };
}
