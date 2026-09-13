//! Commit-path and subsystem fault-injection points. The testkit's injector
//! aborts the process at a configured point; `None` (the default) is a no-op.

/// Crash-injection points on the commit path and the subsystem seams.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultPoint {
    BeforeObjectInstall,
    AfterObjectInstall,
    BeforeFrameAppend,
    AfterFrameAppend,
    BeforeEffectDispatch,
    AfterEffectDispatch,
    BeforeConfigActivation,
    AfterConfigActivation,
    BeforeHeadUpdate,
    AfterHeadUpdate,
    // --- M3 agent spine points ---
    BeforeWakeAccept,
    AfterWakeAccept,
    BeforeRunStart,
    AfterRunStart,
    BeforeModelCall,
    AfterModelCall,
    BeforeToolIntentCommit,
    AfterToolIntentCommit,
    BeforeToolDispatch,
    AfterToolDispatch,
    BeforeToolOutcomeCommit,
    AfterToolOutcomeCommit,
    BeforeRunOutcome,
    AfterRunOutcome,
    // --- M4 memory proposal points ---
    BeforeMemoryProposal,
    AfterMemoryProposal,
    // --- M5 semantic workbench points ---
    BeforeUiReduce,
    AfterUiReduce,
    BeforeUiRender,
    AfterUiRender,
    // --- M6 historical-correction points ---
    BeforeCheckpointCommit,
    AfterCheckpointCommit,
    BeforeBranchTransition,
    AfterBranchTransition,
    BeforeSessionHeadAdvance,
    AfterSessionHeadAdvance,
}

pub trait FaultInjector: Send + Sync {
    fn inject(&self, point: FaultPoint);
}
