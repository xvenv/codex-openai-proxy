#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskKind {
    Chat,
    Transform,
    CodeEditLocal,
    DebugSimple,
    DebugComplex,
    Review,
    Design,
    Migration,
    ToolWorkflow,
}

impl TaskKind {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Transform => "transform",
            Self::CodeEditLocal => "code_edit_local",
            Self::DebugSimple => "debug_simple",
            Self::DebugComplex => "debug_complex",
            Self::Review => "review",
            Self::Design => "design",
            Self::Migration => "migration",
            Self::ToolWorkflow => "tool_workflow",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThinkingLevel {
    Low,
    Medium,
    High,
    ExtraHigh,
}

impl ThinkingLevel {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::ExtraHigh => "extra_high",
        }
    }

    pub const fn backend_effort(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::ExtraHigh => "xhigh",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoutingReason {
    ExplicitBackendModel,
    ExplicitAlias,
    RoutingModeEconomy,
    RoutingModeQuality,
    ComplexTaskKind,
    MediumTaskKind,
    LongConversation,
    ExpandedLocalContext,
    LargeContext,
    ToolsPresent,
    MultiFileContext,
    MultipleCodeBlocks,
    EscalatedAfterWeakResponse,
}

impl RoutingReason {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ExplicitBackendModel => "explicit_backend_model",
            Self::ExplicitAlias => "explicit_alias",
            Self::RoutingModeEconomy => "routing_mode_economy",
            Self::RoutingModeQuality => "routing_mode_quality",
            Self::ComplexTaskKind => "complex_task_kind",
            Self::MediumTaskKind => "medium_task_kind",
            Self::LongConversation => "long_conversation",
            Self::ExpandedLocalContext => "expanded_local_context",
            Self::LargeContext => "large_context",
            Self::ToolsPresent => "tools_present",
            Self::MultiFileContext => "multi_file_context",
            Self::MultipleCodeBlocks => "multiple_code_blocks",
            Self::EscalatedAfterWeakResponse => "escalated_after_weak_response",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverrideSource {
    Policy,
    ClientAlias,
    ClientModel,
    ExecutionManager,
}

impl OverrideSource {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Policy => "policy",
            Self::ClientAlias => "client_alias",
            Self::ClientModel => "client_model",
            Self::ExecutionManager => "execution_manager",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EscalationReason {
    WeakInitialResponse,
}

impl EscalationReason {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::WeakInitialResponse => "weak_initial_response",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RoutingDecision {
    pub selected_alias: String,
    pub backend_model: String,
    pub thinking_level: ThinkingLevel,
    pub task_kind: TaskKind,
    pub reason_codes: Vec<RoutingReason>,
    pub override_source: OverrideSource,
}
