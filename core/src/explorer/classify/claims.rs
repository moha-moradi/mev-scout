//! One tx index, one P&L-bearing kind.
//!
//! Every classify pass may emit. [`TxClaims`] is the authority that keeps the
//! winner. Lower [`kind_precedence`](super::kind_precedence) wins, independent
//! of claim order.
use std::collections::HashMap;

use crate::explorer::types::MevKind;

use super::kind_precedence;

#[derive(Debug, Default)]
pub(super) struct TxClaims {
    claimed: HashMap<u64, MevKind>,
}

impl TxClaims {
    /// Record `kind` for `tx_index`. Returns whether this claim is the winner
    /// after the call (a lower-precedence claim is a no-op and returns false).
    pub(super) fn claim(&mut self, tx_index: u64, kind: MevKind) -> bool {
        match self.claimed.get(&tx_index).copied() {
            Some(existing) if kind_precedence(existing) <= kind_precedence(kind) => false,
            _ => {
                self.claimed.insert(tx_index, kind);
                true
            }
        }
    }

    pub(super) fn winner(&self, tx_index: u64) -> Option<MevKind> {
        self.claimed.get(&tx_index).copied()
    }

    pub(super) fn is_claimed(&self, tx_index: u64) -> bool {
        self.claimed.contains_key(&tx_index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquidation_jit_and_skim_beat_an_inferred_cycle() {
        for kind in [MevKind::Liquidation, MevKind::Jit, MevKind::Skim] {
            let mut claims = TxClaims::default();
            claims.claim(4, MevKind::ArbAtomic);
            assert!(
                claims.claim(4, kind),
                "{kind:?} must take the tx from ArbAtomic"
            );
            assert_eq!(claims.winner(4), Some(kind));
        }
    }
}
