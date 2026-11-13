#![deny(missing_docs)]

mod admission;
mod config;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::runtime::admission_queue::LocalTransferAdmissionQueue;
use crate::{Error, Priority, Result, SourceEpoch};

use admission::QosQuota;
pub(crate) use admission::{QosCharge, QosPermit, QosReservation};

const MAX_CONTRACTS: usize = 4096;

/// Actions allowed under quota pressure. Empty or absent actions preserve rejection;
/// allowing delay requires EngineConfig::runtime_queue. If both are allowed,
/// temporary quota pressure delays; impossible groups still fail admission.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct QosDegradedActions {
    /// Allow waiting for temporary quota pressure; default false, requires runtime_queue.
    pub delay: bool,
    /// Allow admission rejection; default false. Absent actions still reject pressure.
    pub reject: bool,
}

/// None inherits the less specific layer. An effective None is unlimited;
/// Some(0) is a hard zero budget. Counts cover admitted physical data requests
/// until their actual owner drains.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct QosPolicyFields {
    /// Priority override; None (default) inherits the less specific layer.
    pub priority: Option<Priority>,
    /// Retained payload byte ceiling; None (default) inherits, Some(0) forbids payload.
    pub max_inflight_bytes: Option<usize>,
    /// Physical request ceiling; None (default) inherits, Some(0) forbids requests.
    pub max_inflight_requests: Option<usize>,
    /// Admission action override; None (default) inherits. Validated at construction.
    pub allowed_degraded_actions: Option<QosDegradedActions>,
}

impl QosPolicyFields {
    fn overlay(self, specific: Self) -> Self {
        Self {
            priority: specific.priority.or(self.priority),
            max_inflight_bytes: specific.max_inflight_bytes.or(self.max_inflight_bytes),
            max_inflight_requests: specific
                .max_inflight_requests
                .or(self.max_inflight_requests),
            allowed_degraded_actions: specific
                .allowed_degraded_actions
                .or(self.allowed_degraded_actions),
        }
    }

    fn bounded(self) -> bool {
        self.max_inflight_bytes.is_some() || self.max_inflight_requests.is_some()
    }
}

/// Tenant defaults and intent overrides, compiled at engine construction.
/// All names are normalized to lowercase ASCII and must be unique within their scope.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct QosTenantContract {
    /// Nonempty tenant name, at most 256 bytes; the empty default must be replaced.
    pub name: String,
    /// Inherited policy defaults; all fields None unless overridden.
    pub defaults: QosPolicyFields,
    /// Intent-name overrides; empty by default. Each name must fit 256 bytes.
    pub intents: BTreeMap<String, QosPolicyFields>,
}

/// Global -> tenant -> intent policy inheritance. EngineConfig::qos
/// opts in; budgets aggregate by the resolved contract, not by a parent tree.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct QosConfig {
    /// QoS schema selector; keep the default, the only supported schema at construction.
    pub version: u32,
    /// Inherited policy defaults; all fields None unless overridden.
    pub defaults: QosPolicyFields,
    /// Tenant contracts; empty by default. The compiled table is limited to 4096 entries.
    pub tenants: Vec<QosTenantContract>,
}

impl Default for QosConfig {
    fn default() -> Self {
        Self {
            version: 1,
            defaults: QosPolicyFields::default(),
            tenants: Vec::new(),
        }
    }
}

/// Inputs for engine-bound QoS resolution; strings are resolved before submission.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct QosRequestContext {
    /// Tenant name, default `default`; at most 256 bytes before normalization.
    pub tenant_id: String,
    /// QoS intent name, default `unspec`; independent of transport-selection intent.
    pub intent: String,
    /// Used only when no contract layer supplies a priority.
    pub requested_priority: Priority,
}

impl Default for QosRequestContext {
    fn default() -> Self {
        Self {
            tenant_id: "default".into(),
            intent: "unspec".into(),
            requested_priority: Priority::High,
        }
    }
}

/// Current retained charges for one bounded contract; not a drain receipt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct QosUsage {
    /// Admission groups still holding this contract.
    pub owners: usize,
    /// Payload bytes retained by those groups.
    pub inflight_bytes: usize,
    /// Physical requests retained by those groups; merged aliases are charged once.
    pub inflight_requests: usize,
}

#[derive(Debug)]
pub(crate) struct CompiledContract {
    name: String,
    pub(crate) fields: QosPolicyFields,
    slot: usize,
}

#[derive(Debug)]
struct Resolution {
    engine: SourceEpoch,
    tenant_id: String,
    intent: String,
    requested_priority: Priority,
    matched: bool,
    contract: Option<Arc<CompiledContract>>,
    quota: Option<Arc<QosQuota>>,
}

/// Immutable engine-bound resolution. Clone it into requests to reuse the
/// compiled policy and budget slot without resolving names on each submission.
#[derive(Clone, Debug)]
pub struct EffectiveQosPolicy(Arc<Resolution>);

impl EffectiveQosPolicy {
    /// Whether a compiled contract is attached.
    pub fn enabled(&self) -> bool {
        self.0.contract.is_some()
    }
    /// Whether resolution found a tenant-specific or intent-specific contract.
    pub fn matched(&self) -> bool {
        self.0.matched
    }
    /// Normalized tenant name used for resolution.
    pub fn tenant_id(&self) -> &str {
        &self.0.tenant_id
    }
    /// Normalized QoS intent name used for resolution.
    pub fn intent(&self) -> &str {
        &self.0.intent
    }
    /// Caller priority before contract overrides.
    pub fn requested_priority(&self) -> Priority {
        self.0.requested_priority
    }
    /// Priority after the most specific contract override.
    pub fn effective_priority(&self) -> Priority {
        self.fields().priority.unwrap_or(self.0.requested_priority)
    }
    /// Compiled contract identifier, or the fallback label when QoS is disabled.
    pub fn matched_contract(&self) -> &str {
        self.0
            .contract
            .as_ref()
            .map_or("compatibility_default", |contract| &contract.name)
    }
    /// Resolved limits and actions; None means an unlimited effective limit.
    pub fn fields(&self) -> QosPolicyFields {
        self.0
            .contract
            .as_ref()
            .map_or(QosPolicyFields::default(), |contract| contract.fields)
    }
    /// Unbounded policies are not charged and return None.
    pub fn usage(&self) -> Option<QosUsage> {
        let contract = self.0.contract.as_ref()?;
        if !contract.fields.bounded() {
            return None;
        }
        let quota = self.0.quota.as_ref()?;
        Some(quota.usage(contract.slot))
    }

    pub(crate) fn contract(&self, engine: SourceEpoch) -> Result<Option<&CompiledContract>> {
        if self.0.engine != engine {
            return Err(Error::ForeignQosPolicy);
        }
        Ok(self.0.contract.as_deref())
    }
}

struct TenantContract {
    defaults: Arc<CompiledContract>,
    intents: HashMap<String, Arc<CompiledContract>>,
}

/// Owns the immutable resolution table and a single atomic admission ledger.
pub(crate) struct QosContractResolver {
    global: Arc<CompiledContract>,
    tenants: HashMap<String, TenantContract>,
    default: Arc<CompiledContract>,
    quota: Option<Arc<QosQuota>>,
}

fn normalize(value: &str, fallback: &str) -> Result<String> {
    if value.len() > 256 {
        return Err(Error::InvalidConfig("QoS identifiers must fit 256 bytes"));
    }
    let value = value.trim_matches(|c: char| c.is_ascii_whitespace());
    let value = if value.is_empty() { fallback } else { value };
    if value.is_empty() {
        return Err(Error::InvalidConfig("empty QoS contract name"));
    }
    Ok(value.to_ascii_lowercase())
}

impl QosContractResolver {
    pub(crate) fn new(
        config: &QosConfig,
        queue: Option<&Arc<LocalTransferAdmissionQueue>>,
    ) -> Result<Self> {
        if config.version != 1 {
            return Err(Error::InvalidConfig("unsupported QoS version"));
        }
        let mut limits = Vec::new();
        let mut compile =
            |name: String, fields: QosPolicyFields| -> Result<Arc<CompiledContract>> {
                if limits.len() == MAX_CONTRACTS {
                    return Err(Error::InvalidConfig("at most 4096 QoS contracts"));
                }
                let slot = limits.len();
                limits.push(fields);
                Ok(Arc::new(CompiledContract { name, fields, slot }))
            };
        let global = compile("global_default".into(), config.defaults)?;
        let mut tenants = HashMap::new();
        for tenant in &config.tenants {
            let name = normalize(&tenant.name, "")?;
            if tenants.contains_key(&name) {
                return Err(Error::InvalidConfig("duplicate QoS tenant"));
            }
            let fields = config.defaults.overlay(tenant.defaults);
            let defaults = compile(format!("{name}.default"), fields)?;
            let mut intents = HashMap::new();
            for (intent, policy) in &tenant.intents {
                let intent = normalize(intent, "")?;
                if intents.contains_key(&intent) {
                    return Err(Error::InvalidConfig("duplicate QoS intent"));
                }
                let contract = compile(format!("{name}.{intent}"), fields.overlay(*policy))?;
                intents.insert(intent, contract);
            }
            tenants.insert(name, TenantContract { defaults, intents });
        }
        let delay = limits.iter().any(|policy| {
            policy
                .allowed_degraded_actions
                .is_some_and(|actions| actions.delay)
        });
        if delay && queue.is_none() {
            return Err(Error::InvalidConfig(
                "qos.allowed_degraded_actions: delay requires runtime_queue",
            ));
        }
        let quota = limits
            .iter()
            .any(|policy| policy.bounded())
            .then(|| Arc::new(QosQuota::new(limits, queue.filter(|_| delay))));
        let mut resolver = Self {
            default: Arc::clone(&global),
            global,
            tenants,
            quota,
        };
        resolver.default = Arc::clone(resolver.lookup("default", "unspec").0);
        Ok(resolver)
    }

    fn lookup(&self, tenant: &str, intent: &str) -> (&Arc<CompiledContract>, bool) {
        match self.tenants.get(tenant) {
            None => (&self.global, false),
            Some(tenant) => match tenant.intents.get(intent) {
                Some(contract) => (contract, true),
                None => (&tenant.defaults, false),
            },
        }
    }

    #[cfg(feature = "metrics")]
    pub(crate) fn metrics_sampler(&self) -> impl Fn() -> QosUsage + Send + Sync + 'static {
        let quota = self.quota.clone();
        move || {
            quota
                .as_ref()
                .map_or_else(QosUsage::default, |q| q.total_usage())
        }
    }

    pub(crate) fn default_contract(&self) -> &CompiledContract {
        &self.default
    }

    pub(crate) fn resolve(
        resolver: Option<&Self>,
        engine: SourceEpoch,
        context: &QosRequestContext,
    ) -> Result<EffectiveQosPolicy> {
        let tenant_id = normalize(&context.tenant_id, "default")?;
        let intent = normalize(&context.intent, "unspec")?;
        let (contract, matched) = match resolver {
            Some(resolver) => {
                let (contract, matched) = resolver.lookup(&tenant_id, &intent);
                (Some(Arc::clone(contract)), matched)
            }
            None => (None, false),
        };
        Ok(EffectiveQosPolicy(Arc::new(Resolution {
            engine,
            tenant_id,
            intent,
            requested_priority: context.requested_priority,
            contract,
            matched,
            quota: resolver.and_then(|resolver| resolver.quota.clone()),
        })))
    }

    pub(crate) fn admit(&self, charges: Vec<Option<QosCharge>>) -> Result<Vec<Option<QosPermit>>> {
        match &self.quota {
            Some(quota) => quota.admit(charges),
            None => Ok(charges.into_iter().map(|_| None).collect()),
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/runtime/qos_contract.rs"]
mod tests;
