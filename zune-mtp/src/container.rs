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
    DeleteObject = 0x100b,
    GetDevicePropValue = 0x1015,
    SetDevicePropValue = 0x1016,
    SetObjectPropValue = 0x9804,
    GetObjectPropList = 0x9805,
    GetObjectPropsSupported = 0x9806,
    SendObjectPropList = 0x9808,
    GetObjectReferences = 0x9810,
    SetObjectReferences = 0x9811,
    SendWMDRMPDAppRequest = 0x9212,
    GetWMDRMPDAppResponse = 0x9213,
    EnableTrustedFilesOperations = 0x9214,
    DisableTrustedFilesOperations = 0x9215,
    EndTrustedAppSession = 0x9216,
    /// Retrieve the Zune's internal metadata database (ZMDB).
    /// First parameter is the content type (1 = music library).
    GetZuneMetadataDatabase = 0x9217,
    /// Retrieve items the device acquired on its own (podcast downloads,
    /// Zune-to-Zune sharing). Returns a u32 count followed by PUOIDs.
    GetAcquiredItems = 0x9219,
    /// Write sync progress state to the device (530-byte payload).
    SetDeviceSyncProgress = 0x922a,
    /// Read sync progress state from the device (returns 1036 bytes).
    GetDeviceSyncProgress = 0x922f,
    /// Reboot and apply pending firmware update.
    CommitFirmware = 0x9204,
    /// Set WiFi sync profile on the device (324-byte payload).
    SetDeviceWlanProfiles = 0x9227,
    /// Test WiFi hardware capability. Params: 0=test, 1=disassoc?, 2=reset?.
    AsyncTestDeviceWlan = 0x9228,
    /// Control wireless/cloud sync.
    CloudSyncControl = 0x9230,
    /// Format a storage area.
    FormatStore = 0x100f,
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

/// MTP root object handle (parent of all top-level objects).
pub const MTP_ROOT: u32 = 0xFFFFFFFF;

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
    /// Parse a container header from raw bytes, returning None if too short.
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

    /// Returns true if this is a response container.
    pub fn is_response(&self) -> bool {
        self.container_type == ContainerType::Response as u16
    }

    /// Returns true if this is a data container.
    pub fn is_data(&self) -> bool {
        self.container_type == ContainerType::Data as u16
    }

    /// Returns true if the response code indicates success.
    pub fn is_ok(&self) -> bool {
        self.code == ResponseCode::Ok as u16 || self.code == ResponseCode::SessionAlreadyOpen as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_command_no_params() {
        let cmd = build_command(OperationCode::GetDeviceInfo, 1, &[]);
        assert_eq!(cmd.len(), CONTAINER_HEADER_SIZE);
        let hdr = ContainerHeader::parse(&cmd).unwrap();
        assert_eq!(hdr.length, 12);
        assert_eq!(hdr.container_type, ContainerType::Command as u16);
        assert_eq!(hdr.code, OperationCode::GetDeviceInfo as u16);
        assert_eq!(hdr.transaction_id, 1);
    }

    #[test]
    fn build_command_with_params() {
        let cmd = build_command(OperationCode::OpenSession, 2, &[1]);
        assert_eq!(cmd.len(), CONTAINER_HEADER_SIZE + 4);
        let hdr = ContainerHeader::parse(&cmd).unwrap();
        assert_eq!(hdr.length, 16);
        let param = u32::from_le_bytes(cmd[12..16].try_into().unwrap());
        assert_eq!(param, 1);
    }

    #[test]
    fn build_data_container() {
        let payload = b"hello";
        let data = build_data(OperationCode::SendObject, 5, payload);
        assert_eq!(data.len(), CONTAINER_HEADER_SIZE + 5);
        let hdr = ContainerHeader::parse(&data).unwrap();
        assert_eq!(hdr.container_type, ContainerType::Data as u16);
        assert_eq!(&data[CONTAINER_HEADER_SIZE..], b"hello");
    }

    #[test]
    fn parse_header_too_short() {
        assert!(ContainerHeader::parse(&[0; 4]).is_none());
        assert!(ContainerHeader::parse(&[]).is_none());
    }

    #[test]
    fn response_type_checks() {
        let mut buf = vec![0u8; 12];
        // Response type = 3, code = Ok (0x2001)
        buf[4..6].copy_from_slice(&(ContainerType::Response as u16).to_le_bytes());
        buf[6..8].copy_from_slice(&(ResponseCode::Ok as u16).to_le_bytes());
        let hdr = ContainerHeader::parse(&buf).unwrap();
        assert!(hdr.is_response());
        assert!(!hdr.is_data());
        assert!(hdr.is_ok());
    }

    #[test]
    fn data_type_check() {
        let mut buf = vec![0u8; 12];
        buf[4..6].copy_from_slice(&(ContainerType::Data as u16).to_le_bytes());
        buf[6..8].copy_from_slice(&0x1009u16.to_le_bytes()); // GetObject
        let hdr = ContainerHeader::parse(&buf).unwrap();
        assert!(hdr.is_data());
        assert!(!hdr.is_response());
        assert!(!hdr.is_ok());
    }

    #[test]
    fn build_command_zero_params_bytes() {
        let cmd = build_command(OperationCode::GetDeviceInfo, 7, &[]);
        // 12 bytes: length(4) + type(2) + code(2) + tid(4)
        assert_eq!(
            cmd,
            [
                12, 0, 0, 0, // length = 12
                0x01, 0x00, // type = Command (1)
                0x01, 0x10, // code = GetDeviceInfo (0x1001)
                7, 0, 0, 0, // transaction_id = 7
            ]
        );
    }

    #[test]
    fn build_command_two_params_bytes() {
        let cmd = build_command(
            OperationCode::GetObjectHandles,
            3,
            &[0x00010001, 0x00000000],
        );
        assert_eq!(cmd.len(), 20); // 12 header + 8 params
                                   // Header
        assert_eq!(&cmd[0..4], &20u32.to_le_bytes());
        assert_eq!(&cmd[4..6], &(ContainerType::Command as u16).to_le_bytes());
        assert_eq!(
            &cmd[6..8],
            &(OperationCode::GetObjectHandles as u16).to_le_bytes()
        );
        assert_eq!(&cmd[8..12], &3u32.to_le_bytes());
        // Params
        assert_eq!(&cmd[12..16], &0x00010001u32.to_le_bytes());
        assert_eq!(&cmd[16..20], &0u32.to_le_bytes());
    }

    #[test]
    fn build_data_bytes() {
        let payload = [0xDE, 0xAD, 0xBE, 0xEF];
        let data = build_data(OperationCode::SendObject, 10, &payload);
        assert_eq!(
            data,
            [
                16, 0, 0, 0, // length = 16 (12 + 4)
                0x02, 0x00, // type = Data (2)
                0x0d, 0x10, // code = SendObject (0x100d)
                10, 0, 0, 0, // transaction_id = 10
                0xDE, 0xAD, 0xBE, 0xEF, // payload
            ]
        );
    }

    #[test]
    fn session_already_open_is_ok() {
        let mut buf = vec![0u8; 12];
        buf[6..8].copy_from_slice(&(ResponseCode::SessionAlreadyOpen as u16).to_le_bytes());
        let hdr = ContainerHeader::parse(&buf).unwrap();
        assert!(hdr.is_ok());
    }
}
