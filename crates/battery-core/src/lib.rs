//! Battery prediction core.
//!
//! Deliberately free of any Windows dependency so the whole prediction path can
//! be driven from recorded traces in tests. The platform layer feeds it
//! [`Sample`]s and renders the [`Estimates`] it returns.

pub mod alerts;
pub mod curve;
pub mod estimator;
pub mod glyph;
pub mod filter;
pub mod model;
pub mod seed;
pub mod settings;
pub mod store;
pub mod types;

pub use estimator::{Estimator, HistPoint};
pub use model::{Kind, Model};
pub use alerts::{Alert, Alerts};
pub use settings::{GraphKind, PanelTheme, Settings, TrayMode};
pub use types::{fmt_duration, Estimates, Phase, Prediction, Sample};
