//! Reranker execution-provider capabilities and deterministic selection policy.

#[cfg(any(feature = "reranker", test))]
use super::RerankerError;
use crate::config::RerankerExecutionProvider;

const NONE: &[RerankerExecutionProvider] = &[];
const CPU: &[RerankerExecutionProvider] = &[RerankerExecutionProvider::Cpu];
const CUDA_THEN_CPU: &[RerankerExecutionProvider] = &[RerankerExecutionProvider::Cuda, RerankerExecutionProvider::Cpu];
#[cfg(any(feature = "reranker", test))]
const CUDA: &[RerankerExecutionProvider] = &[RerankerExecutionProvider::Cuda];

/// Execution providers compiled into this binary, in preference order.
#[must_use]
pub const fn compiled_execution_providers() -> &'static [RerankerExecutionProvider] {
    if cfg!(feature = "reranker-cuda") {
        CUDA_THEN_CPU
    } else if cfg!(feature = "reranker") {
        CPU
    } else {
        NONE
    }
}

/// Return concrete providers to attempt for the requested policy.
///
/// `auto` is the only policy that can return more than one candidate. Explicit
/// `cpu` and `cuda` never fall back to a different provider.
#[cfg(any(feature = "reranker", test))]
pub(crate) fn execution_provider_candidates(requested: RerankerExecutionProvider) -> Result<&'static [RerankerExecutionProvider], RerankerError> {
    match requested {
        RerankerExecutionProvider::Auto if cfg!(feature = "reranker-cuda") => Ok(CUDA_THEN_CPU),
        RerankerExecutionProvider::Auto | RerankerExecutionProvider::Cpu if cfg!(feature = "reranker") => Ok(CPU),
        RerankerExecutionProvider::Cuda if cfg!(feature = "reranker-cuda") => Ok(CUDA),
        RerankerExecutionProvider::Cuda => Err(RerankerError::ProviderUnavailable(
            "CUDA was requested but this binary was compiled without the `reranker-cuda` feature".into(),
        )),
        RerankerExecutionProvider::Auto | RerankerExecutionProvider::Cpu => Err(RerankerError::ProviderUnavailable(
            "reranking was requested but this binary was compiled without the `reranker` feature".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{compiled_execution_providers, execution_provider_candidates};
    use crate::config::RerankerExecutionProvider::{Auto, Cpu, Cuda};

    #[test]
    fn compiled_provider_report_matches_features() {
        let expected: &[crate::config::RerankerExecutionProvider] = if cfg!(feature = "reranker-cuda") {
            &[Cuda, Cpu]
        } else if cfg!(feature = "reranker") {
            &[Cpu]
        } else {
            &[]
        };
        assert_eq!(compiled_execution_providers(), expected);
    }

    #[test]
    fn explicit_cpu_never_selects_cuda() {
        if cfg!(feature = "reranker") {
            assert_eq!(execution_provider_candidates(Cpu).unwrap(), &[Cpu]);
        } else {
            let _error = execution_provider_candidates(Cpu).unwrap_err();
        }
    }

    #[test]
    fn explicit_cuda_never_falls_back() {
        if cfg!(feature = "reranker-cuda") {
            assert_eq!(execution_provider_candidates(Cuda).unwrap(), &[Cuda]);
        } else {
            let _error = execution_provider_candidates(Cuda).unwrap_err();
        }
    }

    #[test]
    fn auto_uses_compiled_preference_order() {
        if cfg!(feature = "reranker-cuda") {
            assert_eq!(execution_provider_candidates(Auto).unwrap(), &[Cuda, Cpu]);
        } else if cfg!(feature = "reranker") {
            assert_eq!(execution_provider_candidates(Auto).unwrap(), &[Cpu]);
        } else {
            let _error = execution_provider_candidates(Auto).unwrap_err();
        }
    }
}
