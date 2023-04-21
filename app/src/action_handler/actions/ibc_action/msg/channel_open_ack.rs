use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use ibc_types::core::ics03_connection::connection::ConnectionEnd;
use ibc_types::core::ics03_connection::connection::State as ConnectionState;
use ibc_types::core::ics04_channel::channel::ChannelEnd;
use ibc_types::core::ics04_channel::channel::Counterparty;
use ibc_types::core::ics04_channel::channel::State as ChannelState;
use ibc_types::core::ics04_channel::msgs::chan_open_ack::MsgChannelOpenAck;
use ibc_types::core::ics24_host::identifier::PortId;
use penumbra_storage::{StateRead, StateWrite};
use penumbra_transaction::Transaction;

use crate::action_handler::ActionHandler;
use crate::ibc::component::channel::stateful::proof_verification::ChannelProofVerifier;
use crate::ibc::component::channel::StateReadExt as _;
use crate::ibc::component::channel::StateWriteExt as _;
use crate::ibc::component::connection::StateReadExt as _;
use crate::ibc::event;
use crate::ibc::ibc_handler::{AppHandlerCheck, AppHandlerExecute};
use crate::ibc::transfer::Ics20Transfer;

#[async_trait]
impl ActionHandler for MsgChannelOpenAck {
    async fn check_stateless(&self, _context: Arc<Transaction>) -> Result<()> {
        // NOTE: no additional stateless validation is possible

        Ok(())
    }

    async fn check_stateful<S: StateRead + 'static>(&self, _state: Arc<S>) -> Result<()> {
        // No-op: IBC actions merge check_stateful and execute.
        Ok(())
    }

    async fn execute<S: StateWrite>(&self, mut state: S) -> Result<()> {
        tracing::debug!(msg = ?self);
        let mut channel = state
            .get_channel(&self.chan_id_on_a, &self.port_id_on_a)
            .await?
            .ok_or_else(|| anyhow::anyhow!("channel not found"))?;

        channel_state_is_correct(&channel)?;

        // TODO: capability authentication?

        let connection = verify_channel_connection_open(&state, &channel).await?;

        let expected_counterparty =
            Counterparty::new(self.port_id_on_a.clone(), Some(self.chan_id_on_a.clone()));

        let expected_connection_hops = vec![connection
            .counterparty()
            .connection_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no counterparty connection id provided"))?];

        let expected_channel = ChannelEnd {
            state: ChannelState::TryOpen,
            ordering: channel.ordering,
            remote: expected_counterparty,
            connection_hops: expected_connection_hops,
            version: self.version_on_b.clone(),
        };

        state
            .verify_channel_proof(
                &connection,
                &self.proof_chan_end_on_b,
                &self.proof_height_on_b,
                &self.chan_id_on_b,
                &channel.remote.port_id,
                &expected_channel,
            )
            .await?;

        let transfer = PortId::transfer();
        if self.port_id_on_a == transfer {
            Ics20Transfer::chan_open_ack_check(&mut state, self).await?;
        } else {
            return Err(anyhow::anyhow!("invalid port id"));
        }

        channel.set_state(ChannelState::Open);
        channel.set_version(self.version_on_b.clone());
        channel.set_counterparty_channel_id(self.chan_id_on_b.clone());
        state.put_channel(&self.chan_id_on_a, &self.port_id_on_a, channel.clone());

        state.record(event::channel_open_ack(
            &self.port_id_on_a,
            &self.chan_id_on_a,
            &channel,
        ));

        let transfer = PortId::transfer();
        if self.port_id_on_a == transfer {
            Ics20Transfer::chan_open_ack_execute(state, self).await;
        } else {
            return Err(anyhow::anyhow!("invalid port id"));
        }

        Ok(())
    }
}

fn channel_state_is_correct(channel: &ChannelEnd) -> anyhow::Result<()> {
    if channel.state == ChannelState::Init || channel.state == ChannelState::TryOpen {
        Ok(())
    } else {
        Err(anyhow::anyhow!("channel is not in the correct state"))
    }
}

async fn verify_channel_connection_open<S: StateRead>(
    state: S,
    channel: &ChannelEnd,
) -> anyhow::Result<ConnectionEnd> {
    let connection = state
        .get_connection(&channel.connection_hops[0])
        .await?
        .ok_or_else(|| anyhow::anyhow!("connection not found for channel"))?;

    if connection.state != ConnectionState::Open {
        Err(anyhow::anyhow!("connection for channel is not open"))
    } else {
        Ok(connection)
    }
}
