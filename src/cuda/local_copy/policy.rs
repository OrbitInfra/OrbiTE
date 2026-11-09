use super::*;

const INTENTS: [IntentType; 7] = [
    IntentType::Unspec,
    IntentType::ForegroundGet,
    IntentType::BackgroundPrefetch,
    IntentType::Migration,
    IntentType::Checkpoint,
    IntentType::WeightLoading,
    IntentType::StagingInternal,
];
const PRIORITIES: [Priority; 3] = [Priority::High, Priority::Medium, Priority::Low];
const DIRECTIONS: [CopyDirection; 2] = [CopyDirection::HostToDevice, CopyDirection::DeviceToHost];
pub(super) const CLASSES: usize = 2 * 7 * 3;

pub(super) struct Policy {
    backend: LocalCopyBackend,
    normal: usize,
    thresholds: [usize; 14],
    pub kernel_unavailable: LocalCopyFallback,
}
impl Policy {
    pub fn new(config: LocalCopyConfig, kernel_unavailable: LocalCopyFallback) -> Self {
        Self {
            backend: config.backend,
            normal: config.kernel_threshold,
            thresholds: std::array::from_fn(|i| {
                if DIRECTIONS[i / 7] == CopyDirection::DeviceToHost
                    && matches!(
                        INTENTS[i % 7],
                        IntentType::Checkpoint
                            | IntentType::Migration
                            | IntentType::BackgroundPrefetch
                    )
                {
                    config.kernel_threshold_background
                } else {
                    config.kernel_threshold
                }
            }),
            kernel_unavailable,
        }
    }

    /// `None` means eligible for the kernel. This is typed table lookup only;
    /// priority remains advisory because the caller owns stream ordering.
    pub fn dma_reason(&self, class: usize, plan: &plan::Plan) -> Result<Option<LocalCopyFallback>> {
        if self.backend == LocalCopyBackend::Dma || plan.rows.is_empty() {
            return Ok(Some(LocalCopyFallback::Threshold));
        }
        if !plan.independent {
            if self.backend == LocalCopyBackend::Kernel {
                return Err(Error::InvalidConfig(
                    "local copy kernel requires disjoint destinations",
                ));
            }
            return Ok(Some(LocalCopyFallback::Overlap));
        }
        if self.backend == LocalCopyBackend::Kernel {
            return Ok(None);
        }
        Ok(if plan.dma.len() < self.normal {
            Some(LocalCopyFallback::Threshold)
        } else if plan.dma.len() < self.thresholds[class / 3] {
            Some(LocalCopyFallback::Intent)
        } else {
            None
        })
    }
}

pub(super) fn class(dir: CopyDirection, options: &LocalCopyOptions) -> usize {
    let direction = match dir {
        CopyDirection::HostToDevice => 0,
        CopyDirection::DeviceToHost => 1,
    };
    let intent = match options.intent {
        IntentType::Unspec => 0,
        IntentType::ForegroundGet => 1,
        IntentType::BackgroundPrefetch => 2,
        IntentType::Migration => 3,
        IntentType::Checkpoint => 4,
        IntentType::WeightLoading => 5,
        IntentType::StagingInternal => 6,
    };
    let priority = match options.priority {
        Priority::High => 0,
        Priority::Medium => 1,
        Priority::Low => 2,
    };
    (direction * 7 + intent) * 3 + priority
}

pub(super) fn counters() -> [LocalCopyClassStats; CLASSES] {
    std::array::from_fn(|i| LocalCopyClassStats {
        direction: DIRECTIONS[i / 21],
        intent: INTENTS[(i / 3) % 7],
        priority: PRIORITIES[i % 3],
        dma_submissions: 0,
        dma_batch_calls: 0,
        kernel_submissions: 0,
        kernel_ctas: 0,
        last_kernel_ctas: 0,
        last_backend: LocalCopyBackend::Dma,
        last_fallback: None,
    })
}
