//! VISCA over IP framing (ADR 0018 § 1), from its public documentation: an 8-byte header
//! (payload type, payload length, sequence number, big-endian) before each VISCA message, in
//! one UDP datagram.

/// Size of the header.
pub const HEADER: usize = 8;

/// Longest datagram VMA sends or reads.
pub const MAX_DATAGRAM: usize = HEADER + crate::visca::MAX_MESSAGE;

/// Default UDP port of VISCA over IP.
pub const DEFAULT_PORT: u16 = 52381;

/// Payload of the control message that resets the sequence number, and of its reply.
pub const RESET: u8 = 0x01;
/// Control reply payloads: the sequence number, or the message, was not the one expected.
pub const SEQUENCE_ERROR: [u8; 2] = [0x0F, 0x01];
pub const MESSAGE_ERROR: [u8; 2] = [0x0F, 0x02];

/// Payload type of a datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadType {
    /// `0x0100`: a VISCA command.
    Command,
    /// `0x0110`: a VISCA inquiry.
    Inquiry,
    /// `0x0111`: a VISCA reply (accusé, fin, réponse, erreur).
    Reply,
    /// `0x0120`: a VISCA device setting command (not sent by VMA).
    DeviceSetting,
    /// `0x0200`: a control command (reset of the sequence number).
    Control,
    /// `0x0201`: the reply to a control command.
    ControlReply,
}

impl PayloadType {
    pub fn code(self) -> u16 {
        match self {
            Self::Command => 0x0100,
            Self::Inquiry => 0x0110,
            Self::Reply => 0x0111,
            Self::DeviceSetting => 0x0120,
            Self::Control => 0x0200,
            Self::ControlReply => 0x0201,
        }
    }

    pub fn from_code(code: u16) -> Option<Self> {
        match code {
            0x0100 => Some(Self::Command),
            0x0110 => Some(Self::Inquiry),
            0x0111 => Some(Self::Reply),
            0x0120 => Some(Self::DeviceSetting),
            0x0200 => Some(Self::Control),
            0x0201 => Some(Self::ControlReply),
            _ => None,
        }
    }
}

/// Writes `payload` behind its header into `out`; returns the datagram (`None`: too long).
pub fn encode<'a>(
    out: &'a mut [u8; MAX_DATAGRAM],
    ty: PayloadType,
    sequence: u32,
    payload: &[u8],
) -> Option<&'a [u8]> {
    let len = u16::try_from(payload.len()).ok()?;
    let total = HEADER + payload.len();
    let datagram = out.get_mut(..total)?;
    let (header, body) = datagram.split_at_mut(HEADER);
    let (ty_bytes, rest) = header.split_at_mut(2);
    let (len_bytes, sequence_bytes) = rest.split_at_mut(2);
    ty_bytes.copy_from_slice(&ty.code().to_be_bytes());
    len_bytes.copy_from_slice(&len.to_be_bytes());
    sequence_bytes.copy_from_slice(&sequence.to_be_bytes());
    body.copy_from_slice(payload);
    out.get(..total)
}

/// A datagram read back: its type, sequence number and payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Datagram<'a> {
    pub ty: PayloadType,
    pub sequence: u32,
    pub payload: &'a [u8],
}

/// `None`: shorter than its header says, or of an unknown type.
pub fn decode(datagram: &[u8]) -> Option<Datagram<'_>> {
    let ty = u16::from_be_bytes(datagram.get(0..2)?.try_into().ok()?);
    let len = u16::from_be_bytes(datagram.get(2..4)?.try_into().ok()?);
    let sequence = u32::from_be_bytes(datagram.get(4..8)?.try_into().ok()?);
    let payload = datagram.get(HEADER..HEADER + usize::from(len))?;
    Some(Datagram {
        ty: PayloadType::from_code(ty)?,
        sequence,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_datagram_is_framed_to_the_byte() {
        let mut out = [0; MAX_DATAGRAM];
        let recall = [0x81, 0x01, 0x04, 0x3F, 0x02, 0x03, 0xFF];
        let datagram = encode(&mut out, PayloadType::Command, 0x0102_0304, &recall).unwrap();
        assert_eq!(
            datagram,
            [
                0x01, 0x00, 0x00, 0x07, 0x01, 0x02, 0x03, 0x04, 0x81, 0x01, 0x04, 0x3F, 0x02, 0x03,
                0xFF
            ]
        );
        let back = decode(datagram).unwrap();
        assert_eq!(back.ty, PayloadType::Command);
        assert_eq!(back.sequence, 0x0102_0304);
        assert_eq!(back.payload, recall);
    }

    #[test]
    fn the_reset_and_its_reply() {
        let mut out = [0; MAX_DATAGRAM];
        let reset = encode(&mut out, PayloadType::Control, 0, &[RESET]).unwrap();
        assert_eq!(reset, [0x02, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0x01]);
        let reply = [0x02, 0x01, 0x00, 0x02, 0, 0, 0, 9, 0x0F, 0x01];
        let back = decode(&reply).unwrap();
        assert_eq!(back.ty, PayloadType::ControlReply);
        assert_eq!(back.sequence, 9);
        assert_eq!(back.payload, SEQUENCE_ERROR);
    }

    #[test]
    fn truncated_or_unknown_datagrams_are_refused() {
        assert_eq!(
            decode(&[0x01, 0x11, 0x00, 0x03, 0, 0, 0, 1, 0x90, 0x41]),
            None
        );
        assert_eq!(decode(&[0x09, 0x99, 0x00, 0x00, 0, 0, 0, 1]), None);
        assert_eq!(decode(&[0x01]), None);
        let mut out = [0; MAX_DATAGRAM];
        assert_eq!(encode(&mut out, PayloadType::Command, 0, &[0; 17]), None);
    }
}
