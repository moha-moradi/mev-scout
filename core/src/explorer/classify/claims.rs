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
    fn sandwich_beats_arb_regardless_of_claim_order() {
        let mut a = TxClaims::default();
        assert!(a.claim(1, MevKind::Sandwich));
        assert!(!a.claim(1, MevKind::ArbAtomic));
        assert_eq!(a.winner(1), Some(MevKind::Sandwich));

        let mut b = TxClaims::default();
        assert!(b.claim(1, MevKind::ArbAtomic));
        assert!(b.claim(1, MevKind::Sandwich));
        assert_eq!(b.winner(1), Some(MevKind::Sandwich));
    }

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

    /// JitArb is an upgrade of Jit on a tx that also closed a cycle. The arb
    /// must not win that three-way, or the flow is counted twice.
    #[test]
    fn jit_arb_upgrade_beats_jit_and_arb() {
        let mut claims = TxClaims::default();
        claims.claim(2, MevKind::Jit);
        claims.claim(2, MevKind::ArbAtomic);
        assert!(claims.claim(2, MevKind::JitArb));
        assert_eq!(claims.winner(2), Some(MevKind::JitArb));
        assert!(claims.is_claimed(2));
        assert!(!claims.is_claimed(9));
    }
}
