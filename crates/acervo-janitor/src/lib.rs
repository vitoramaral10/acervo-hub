//! Reconciliação da biblioteca: decide o que remover, sem executar nada.
//!
//! O crate é puro — recebe um [`acervo_core::Inventory`] já lido e devolve um
//! [`plan::Plan`]. Os adaptadores que falam com as instâncias `*arr`, com o
//! cliente de download e com o filesystem ficam fora daqui, e é isso que
//! permite testar a decisão sem subir nada.
//!
//! ```
//! use std::time::SystemTime;
//! use acervo_core::{Allocated, Inventory};
//! use acervo_janitor::{Policy, StrikeLedger, reconcile};
//!
//! let inventario = Inventory::new(Allocated::ZERO);
//! let plano = reconcile(
//!     &inventario,
//!     &Policy::default(),
//!     &mut StrikeLedger::new(),
//!     SystemTime::now(),
//! )
//! .expect("inventário vazio não dispara trava");
//!
//! assert!(plano.is_empty());
//! assert!(plano.mode.is_dry_run());
//! ```

pub mod plan;
pub mod policy;
pub mod reconcile;
pub mod strike;

pub use plan::{Abort, Action, Plan, SkipReason, Skipped};
pub use policy::{Guards, Mode, Policy};
pub use reconcile::reconcile;
pub use strike::{StrikeKey, StrikeLedger};
