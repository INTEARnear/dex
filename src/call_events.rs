use intear_dex_types::{AccountOrDexId, AssetId, BalanceUpdate, DexEventData, IntearDexEvent};
use near_sdk::{
    base64::{Engine, prelude::BASE64_STANDARD},
    json_types::U128,
};

#[derive(Default)]
pub struct CallEvents {
    balance_updates: Vec<BalanceUpdate>,
    dex_events: Vec<DexEventData>,
}

impl CallEvents {
    pub fn balance_updated(&mut self, owner: AccountOrDexId, asset_id: AssetId, balance: U128) {
        match self
            .balance_updates
            .iter_mut()
            .find(|update| update.owner == owner && update.asset_id == asset_id)
        {
            Some(update) => update.balance = balance,
            None => self.balance_updates.push(BalanceUpdate {
                owner,
                asset_id,
                balance,
            }),
        }
    }

    pub fn dex_events_emitted(&mut self, dex_events: Vec<DexEventData>) {
        self.dex_events.extend(dex_events);
    }

    /// Logs the dex events as one compressed `dex_events_borsh` event, and
    /// the final balances as one `balance_updates` event
    pub fn emit(self) {
        if !self.dex_events.is_empty() {
            let dex_events =
                near_sdk::borsh::to_vec(&self.dex_events).expect("Failed to serialize dex events");
            IntearDexEvent::DexEventsBorsh(
                BASE64_STANDARD.encode(lz4_flex::compress_prepend_size(&dex_events)),
            )
            .emit();
        }
        if !self.balance_updates.is_empty() {
            IntearDexEvent::BalanceUpdates(self.balance_updates).emit();
        }
    }
}
