use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::model::{ActionContext, AgentState, ScreenSnapshot, Session, StyledCell};

pub const MAGIC: [u8; 4] = *b"PANE";
pub const PROTOCOL_VERSION: u16 = 2;
pub const HEADER_LEN: usize = 12;
pub const MAX_PAYLOAD_SIZE: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    HelloAck {
        daemon_epoch: Uuid,
    },
    SessionList(Vec<Session>),
    FullSnapshot(ScreenSnapshot),
    FullSnapshotBegin {
        snapshot: ScreenSnapshot,
        chunk_count: u32,
    },
    FullSnapshotChunk {
        session_id: Uuid,
        snapshot_epoch: Uuid,
        chunk_index: u32,
        contents: String,
        styled_cells: Vec<StyledCell>,
    },
    FullSnapshotEnd {
        session_id: Uuid,
        snapshot_epoch: Uuid,
        sequence_number: u64,
    },
    ScreenDiff {
        snapshot: ScreenSnapshot,
        base_sequence_number: u64,
        changed_cells: Vec<StyledCell>,
        cleared_cells: Vec<(u16, u16)>,
    },
    SessionState(Session),
    SessionDeleted {
        session_id: Uuid,
    },
    AgentState {
        session_id: Uuid,
        state: AgentState,
    },
    ActionContext(ActionContext),
    ActionContextCleared {
        session_id: Uuid,
    },
    InputLeaseState {
        session_id: Uuid,
        owned: bool,
    },
    ResizeLeaseState {
        session_id: Uuid,
        owned: bool,
        available: bool,
    },
    ProtocolError {
        message: String,
    },
    Heartbeat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    Hello {
        protocol_version: u16,
    },
    Subscribe {
        session_ids: Vec<Uuid>,
    },
    InputLeaseRequest {
        session_id: Uuid,
        #[serde(default)]
        force: bool,
    },
    ResizeLeaseRequest {
        session_id: Uuid,
        #[serde(default)]
        force: bool,
    },
    Resize {
        session_id: Uuid,
        cols: u16,
        rows: u16,
        pixel_width: u16,
        pixel_height: u16,
    },
    Input {
        session_id: Uuid,
        input_sequence: u64,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    ResyncRequest {
        session_id: Uuid,
    },
    SnapshotAck {
        session_id: Uuid,
        snapshot_epoch: Uuid,
        sequence_number: u64,
    },
    Heartbeat,
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("message is shorter than the envelope header")]
    TooShort,
    #[error("invalid protocol magic")]
    BadMagic,
    #[error("unsupported protocol version {0}")]
    BadVersion(u16),
    #[error("payload exceeds the allowed size")]
    TooLarge,
    #[error("payload length does not match the envelope")]
    LengthMismatch,
    #[error("invalid payload: {0}")]
    InvalidPayload(String),
}

pub fn encode<T: Serialize>(message_type: u8, value: &T) -> Result<Vec<u8>, ProtocolError> {
    let mut payload = Vec::new();
    value
        .serialize(
            &mut rmp_serde::Serializer::new(&mut payload)
                .with_struct_map()
                .with_human_readable(),
        )
        .map_err(|error| ProtocolError::InvalidPayload(error.to_string()))?;
    if payload.len() > MAX_PAYLOAD_SIZE {
        return Err(ProtocolError::TooLarge);
    }

    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(&MAGIC);
    frame.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    frame.push(message_type);
    frame.push(0);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub fn decode_client(frame: &[u8]) -> Result<ClientMessage, ProtocolError> {
    if frame.len() < HEADER_LEN {
        return Err(ProtocolError::TooShort);
    }
    if frame[..4] != MAGIC {
        return Err(ProtocolError::BadMagic);
    }
    let version = u16::from_be_bytes([frame[4], frame[5]]);
    if version != PROTOCOL_VERSION {
        return Err(ProtocolError::BadVersion(version));
    }
    let payload_len = u32::from_be_bytes([frame[8], frame[9], frame[10], frame[11]]) as usize;
    if payload_len > MAX_PAYLOAD_SIZE {
        return Err(ProtocolError::TooLarge);
    }
    if frame.len() != HEADER_LEN + payload_len {
        return Err(ProtocolError::LengthMismatch);
    }
    let mut deserializer = rmp_serde::Deserializer::new(&frame[HEADER_LEN..]).with_human_readable();
    ClientMessage::deserialize(&mut deserializer)
        .map_err(|error| ProtocolError::InvalidPayload(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_uses_network_byte_order_and_round_trips() {
        let payload = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
        };
        let frame = encode(1, &payload).unwrap();
        assert_eq!(&frame[..4], b"PANE");
        assert_eq!(&frame[4..6], &[0, 2]);
        assert!(matches!(
            decode_client(&frame).unwrap(),
            ClientMessage::Hello {
                protocol_version: 2
            }
        ));
    }

    #[test]
    fn oversized_declared_payload_is_rejected_before_decode() {
        let mut frame = vec![0; HEADER_LEN];
        frame[..4].copy_from_slice(b"PANE");
        frame[4..6].copy_from_slice(&PROTOCOL_VERSION.to_be_bytes());
        frame[8..12].copy_from_slice(&((MAX_PAYLOAD_SIZE + 1) as u32).to_be_bytes());
        assert!(matches!(
            decode_client(&frame),
            Err(ProtocolError::TooLarge)
        ));
    }
}
