pub mod chain;
pub mod gas;
pub(crate) mod opportunity;
pub mod strategy;
pub mod tags;
pub use chain::{v2_storage_slots_for_factory, ChainName, ProviderEndpoint};
pub use gas::{GasCalibration, GasCalibrationSnapshot};
pub use opportunity::{
    canonical_route_suffix, compute_canonical_id, compute_savax_canonical_id, CanonicalIdParts,
    DetectionPath, MevOpportunity, ResultsFile, SavaxDirection, SavaxRoute,
};
pub use strategy::{FlashLoanProvider, GasConfig, GasModel, OutputFormat, RangeMode, Strategy};
