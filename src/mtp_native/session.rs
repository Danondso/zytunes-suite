use crate::mtp::container::*;
use crate::mtp::transport::UsbTransport;

/// An active MTP session with a device.
pub struct MtpSession {
    pub transport: UsbTransport,
    session_id: u32,
    transaction_id: u32,
}

/// Decoded MTP ObjectInfo dataset.
#[derive(Debug)]
pub struct ObjectInfo {
    pub storage_id: u32,
    pub object_format: u16,
    pub compressed_size: u32,
    pub filename: String,
    pub capture_date: String,
    pub modification_date: String,
    pub association_type: u16,
}

impl MtpSession {
    /// Open an MTP session on the device.
    /// Follows the android-file-transfer-linux initialization order:
    /// 1. GetDeviceInfo (before session, tid=1)
    /// 2. OpenSession (tid=2)
    pub fn open(vendor_id: u16, product_id: u16) -> Result<Self, String> {
        let transport = UsbTransport::open(vendor_id, product_id)?;
        let mut session = MtpSession {
            transport,
            session_id: 1,
            transaction_id: 0,
        };

        // GetDeviceInfo (tid=1 — C++ uses Transaction class starting at 1).
        match session.execute_data_in(OperationCode::GetDeviceInfo, &[]) {
            Ok(data) => {
                eprintln!("  GetDeviceInfo: {} bytes", data.len());
                parse_device_info_ops(&data);
            }
            Err(e) => eprintln!("  GetDeviceInfo: {e} (continuing)"),
        }

        // OpenSession (tid=2).
        let resp = session.execute_simple(OperationCode::OpenSession, &[1])?;
        eprintln!("  OpenSession response: 0x{:04x}", resp);
        Ok(session)
    }

    fn next_transaction(&mut self) -> u32 {
        self.transaction_id += 1;
        self.transaction_id
    }

    /// Send a command with parameters, read the response. Returns response code.
    pub fn execute_simple(
        &mut self,
        code: OperationCode,
        params: &[u32],
    ) -> Result<u16, String> {
        let tid = self.next_transaction();
        let cmd = build_command(code, tid, params);
        self.transport.write(&cmd)?;

        let resp = self.transport.read_container()?;
        let hdr = ContainerHeader::parse(&resp).ok_or("Bad response")?;

        // If we got data first, read the actual response after it.
        if hdr.is_data() {
            let resp2 = self.transport.read_container()?;
            let hdr2 = ContainerHeader::parse(&resp2).ok_or("Bad response after data")?;
            return Ok(hdr2.code);
        }

        Ok(hdr.code)
    }

    /// Send a command, receive data phase + response. Returns the data payload.
    pub fn execute_data_in(
        &mut self,
        code: OperationCode,
        params: &[u32],
    ) -> Result<Vec<u8>, String> {
        let tid = self.next_transaction();
        let cmd = build_command(code, tid, params);
        self.transport.write(&cmd)?;

        // Read data phase.
        let data = self.transport.read_container()?;
        let hdr = ContainerHeader::parse(&data).ok_or("Bad data response")?;
        if !hdr.is_data() {
            return Err(format!(
                "Expected data container, got type={} code=0x{:04x}",
                hdr.container_type, hdr.code
            ));
        }
        let payload = data[CONTAINER_HEADER_SIZE..].to_vec();

        // Read response phase.
        let resp = self.transport.read_container()?;
        let resp_hdr = ContainerHeader::parse(&resp).ok_or("Bad response")?;
        if !resp_hdr.is_ok() {
            return Err(format!("Operation failed: 0x{:04x}", resp_hdr.code));
        }

        Ok(payload)
    }

    /// Send data out using the Microsoft/Zune split-write protocol:
    /// Data container header (12 bytes), then raw payload — no command container.
    pub fn execute_data_out(
        &mut self,
        code: OperationCode,
        payload: &[u8],
    ) -> Result<u16, String> {
        let tid = self.next_transaction();

        let total_size = (CONTAINER_HEADER_SIZE + payload.len()) as u32;
        let mut header = Vec::with_capacity(CONTAINER_HEADER_SIZE);
        header.extend_from_slice(&total_size.to_le_bytes());
        header.extend_from_slice(&(ContainerType::Data as u16).to_le_bytes());
        header.extend_from_slice(&(code as u16).to_le_bytes());
        header.extend_from_slice(&tid.to_le_bytes());

        self.transport.write(&header)?;
        self.transport.write(payload)?;

        let resp = self.transport.read_container()?;
        let hdr = ContainerHeader::parse(&resp).ok_or("Bad response")?;
        Ok(hdr.code)
    }

    /// Generic operation: send command, optionally send data, receive response + optional data.
    /// Matches the android-file-transfer-linux GenericOperation pattern.
    pub fn generic_operation_send(
        &mut self,
        code: OperationCode,
        data: &[u8],
    ) -> Result<(), String> {
        let resp_code = self.execute_data_out(code, data)?;
        if resp_code != ResponseCode::Ok as u16 {
            return Err(format!("Operation 0x{:04x} failed: 0x{:04x}", code as u16, resp_code));
        }
        Ok(())
    }

    /// Generic operation: send command, receive data.
    pub fn generic_operation_receive(
        &mut self,
        code: OperationCode,
    ) -> Result<Vec<u8>, String> {
        self.execute_data_in(code, &[])
    }

    /// EnableSecureFileOperations — sends 4 u32 CMAC values as parameters.
    pub fn enable_secure_file_operations(&mut self, cmac: [u32; 4]) -> Result<(), String> {
        let resp = self.execute_simple(
            OperationCode::EnableTrustedFilesOperations,
            &cmac,
        )?;
        if resp != ResponseCode::Ok as u16 {
            return Err(format!("EnableSecureFileOperations failed: 0x{:04x}", resp));
        }
        Ok(())
    }

    /// Get list of storage IDs on the device.
    pub fn get_storage_ids(&mut self) -> Result<Vec<u32>, String> {
        let data = self.execute_data_in(OperationCode::GetStorageIDs, &[])?;
        Ok(parse_u32_array(&data))
    }

    /// Get object handles in a given storage, optionally filtered by parent.
    pub fn get_object_handles(
        &mut self,
        storage_id: u32,
        parent: u32,
    ) -> Result<Vec<u32>, String> {
        // params: storage_id, format (0=all), parent (0xffffffff=root)
        let data = self.execute_data_in(
            OperationCode::GetObjectHandles,
            &[storage_id, 0x00000000, parent],
        )?;
        Ok(parse_u32_array(&data))
    }

    /// Get info about a specific object.
    pub fn get_object_info(&mut self, handle: u32) -> Result<ObjectInfo, String> {
        let data = self.execute_data_in(OperationCode::GetObjectInfo, &[handle])?;
        parse_object_info(&data)
    }

    pub fn close(mut self) {
        let _ = self.execute_simple(OperationCode::CloseSession, &[]);
    }
}

/// Parse DeviceInfo and print supported operations.
/// DeviceInfo layout:
///   u16 std_version, u32 vendor_ext_id, u16 vendor_ext_version,
///   string vendor_ext_desc, u16 functional_mode,
///   u16_array operations_supported, u16_array events_supported, ...
fn parse_device_info_ops(data: &[u8]) {
    if data.len() < 8 {
        return;
    }
    let std_ver = u16::from_le_bytes(data[0..2].try_into().unwrap());
    eprintln!("    MTP version: {}.{}", std_ver / 100, std_ver % 100);

    // Skip: std_version(2) + vendor_ext_id(4) + vendor_ext_version(2) = 8 bytes.
    let mut offset = 8;

    // Skip vendor_ext_desc (MTP string).
    if offset < data.len() {
        let num_chars = data[offset] as usize;
        offset += 1 + num_chars * 2;
    }

    // Skip functional_mode (u16).
    offset += 2;

    // Read operations_supported (u16 array): [u32 count] [u16 values...]
    if offset + 4 <= data.len() {
        let count = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        let mut ops = Vec::new();
        for _ in 0..count {
            if offset + 2 > data.len() {
                break;
            }
            let op = u16::from_le_bytes(data[offset..offset + 2].try_into().unwrap());
            ops.push(op);
            offset += 2;
        }
        eprintln!("    Supported operations ({}):", ops.len());
        for op in &ops {
            eprintln!("      0x{:04x}", op);
        }
    }
}

/// Parse MTP u32 array: [u32 count] [u32 values...]
fn parse_u32_array(data: &[u8]) -> Vec<u32> {
    if data.len() < 4 {
        return vec![];
    }
    let count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let mut result = Vec::with_capacity(count);
    for i in 0..count {
        let offset = 4 + i * 4;
        if offset + 4 > data.len() {
            break;
        }
        result.push(u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()));
    }
    result
}

/// Read a MTP string: [u8 num_chars] [u16 chars...] (UCS-2LE, null terminated)
fn read_mtp_string(data: &[u8], offset: &mut usize) -> String {
    if *offset >= data.len() {
        return String::new();
    }
    let num_chars = data[*offset] as usize;
    *offset += 1;
    if num_chars == 0 {
        return String::new();
    }
    let mut chars = Vec::with_capacity(num_chars);
    for _ in 0..num_chars {
        if *offset + 2 > data.len() {
            break;
        }
        let ch = u16::from_le_bytes(data[*offset..*offset + 2].try_into().unwrap());
        *offset += 2;
        if ch != 0 {
            chars.push(ch);
        }
    }
    String::from_utf16_lossy(&chars)
}

/// Parse the MTP ObjectInfo dataset.
fn parse_object_info(data: &[u8]) -> Result<ObjectInfo, String> {
    if data.len() < 52 {
        return Err("ObjectInfo too short".to_string());
    }

    let storage_id = u32::from_le_bytes(data[0..4].try_into().unwrap());
    let object_format = u16::from_le_bytes(data[4..6].try_into().unwrap());
    // data[6..8] = protection status
    let compressed_size = u32::from_le_bytes(data[8..12].try_into().unwrap());
    // data[12..14] = thumb format
    // data[14..18] = thumb compressed size
    // data[18..22] = thumb pix width
    // data[22..26] = thumb pix height
    // data[26..30] = image pix width
    // data[30..34] = image pix height
    // data[34..38] = image bit depth
    // data[38..42] = parent object
    let association_type = u16::from_le_bytes(data[42..44].try_into().unwrap());
    // data[44..48] = association desc
    // data[48..52] = sequence number

    let mut offset = 52;
    let filename = read_mtp_string(data, &mut offset);
    let capture_date = read_mtp_string(data, &mut offset);
    let modification_date = read_mtp_string(data, &mut offset);

    Ok(ObjectInfo {
        storage_id,
        object_format,
        compressed_size,
        filename,
        capture_date,
        modification_date,
        association_type,
    })
}
