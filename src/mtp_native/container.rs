/// MTP/PTP container types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ContainerType {
    Command = 1,
    Data = 2,
    Response = 3,
    Event = 4,
}

/// MTP operation codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum OperationCode {
    GetDeviceInfo = 0x1001,
    OpenSession = 0x1002,
    CloseSession = 0x1003,
    GetStorageIDs = 0x1004,
    GetStorageInfo = 0x1005,
    GetObjectHandles = 0x1007,
    GetObjectInfo = 0x1008,
    GetObject = 0x1009,
    SendObjectInfo = 0x100c,
    SendObject = 0x100d,
    SetDevicePropValue = 0x1016,
    SendWMDRMPDAppRequest = 0x9212,
    GetWMDRMPDAppResponse = 0x9213,
    EnableTrustedFilesOperations = 0x9215,
    DisableTrustedFilesOperations = 0x9214,
    EndTrustedAppSession = 0x9216,
}

/// MTP response codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ResponseCode {
    Ok = 0x2001,
    SessionAlreadyOpen = 0x201e,
}

/// Header size for MTP containers: length(4) + type(2) + code(2) + transaction(4) = 12.
pub const CONTAINER_HEADER_SIZE: usize = 12;

/// Build a command container with up to 5 u32 parameters.
pub fn build_command(code: OperationCode, transaction_id: u32, params: &[u32]) -> Vec<u8> {
    let payload_size = params.len() * 4;
    let total_size = CONTAINER_HEADER_SIZE + payload_size;
    let mut buf = Vec::with_capacity(total_size);

    buf.extend_from_slice(&(total_size as u32).to_le_bytes());
    buf.extend_from_slice(&(ContainerType::Command as u16).to_le_bytes());
    buf.extend_from_slice(&(code as u16).to_le_bytes());
    buf.extend_from_slice(&transaction_id.to_le_bytes());
    for p in params {
        buf.extend_from_slice(&p.to_le_bytes());
    }
    buf
}

/// Build a data container with an arbitrary payload.
pub fn build_data(code: OperationCode, transaction_id: u32, payload: &[u8]) -> Vec<u8> {
    let total_size = CONTAINER_HEADER_SIZE + payload.len();
    let mut buf = Vec::with_capacity(total_size);

    buf.extend_from_slice(&(total_size as u32).to_le_bytes());
    buf.extend_from_slice(&(ContainerType::Data as u16).to_le_bytes());
    buf.extend_from_slice(&(code as u16).to_le_bytes());
    buf.extend_from_slice(&transaction_id.to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// Parsed MTP container header.
#[derive(Debug)]
pub struct ContainerHeader {
    pub length: u32,
    pub container_type: u16,
    pub code: u16,
    pub transaction_id: u32,
}

impl ContainerHeader {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < CONTAINER_HEADER_SIZE {
            return None;
        }
        Some(ContainerHeader {
            length: u32::from_le_bytes(data[0..4].try_into().ok()?),
            container_type: u16::from_le_bytes(data[4..6].try_into().ok()?),
            code: u16::from_le_bytes(data[6..8].try_into().ok()?),
            transaction_id: u32::from_le_bytes(data[8..12].try_into().ok()?),
        })
    }

    pub fn is_response(&self) -> bool {
        self.container_type == ContainerType::Response as u16
    }

    pub fn is_data(&self) -> bool {
        self.container_type == ContainerType::Data as u16
    }

    pub fn is_ok(&self) -> bool {
        self.code == ResponseCode::Ok as u16
            || self.code == ResponseCode::SessionAlreadyOpen as u16
    }
}
