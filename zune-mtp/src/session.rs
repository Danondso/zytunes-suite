use crate::container::*;
use crate::transport::Transport;
use crate::MtpError;

/// An active MTP session with a device.
pub struct MtpSession {
    pub transport: Transport,
    #[allow(dead_code)]
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
    pub fn open(vendor_id: u16, product_id: u16) -> Result<Self, MtpError> {
        let transport = Transport::open(vendor_id, product_id)?;
        let mut session = MtpSession {
            transport,
            session_id: 1,
            transaction_id: 0,
        };

        // OpenSession with tid=0 (matching aft's Device::OpenSession).
        let open_cmd = build_command(OperationCode::OpenSession, 0, &[1]);
        session.transport.write(&open_cmd)?;
        // Use read_container which has a 30s timeout via read_with_timeout.
        let open_resp = session.transport.read_container()?;
        let _open_hdr = ContainerHeader::parse(&open_resp)
            .ok_or(MtpError::Protocol("Bad OpenSession response".to_string()))?;

        // GetDeviceInfo with tid=1 (matching aft's Session constructor).
        let _ = session.execute_data_in(OperationCode::GetDeviceInfo, &[]);

        Ok(session)
    }

    fn next_transaction(&mut self) -> u32 {
        self.transaction_id += 1;
        self.transaction_id
    }

    /// Send a command with parameters, read the response. Returns response code.
    pub fn execute_simple(&mut self, code: OperationCode, params: &[u32]) -> Result<u16, MtpError> {
        let tid = self.next_transaction();
        let cmd = build_command(code, tid, params);
        self.transport.write(&cmd)?;

        let resp = self.transport.read_container()?;
        let hdr =
            ContainerHeader::parse(&resp).ok_or(MtpError::Protocol("Bad response".to_string()))?;

        if hdr.is_data() {
            let resp2 = self.transport.read_container()?;
            let hdr2 = ContainerHeader::parse(&resp2)
                .ok_or(MtpError::Protocol("Bad response after data".to_string()))?;
            return Ok(hdr2.code);
        }

        Ok(hdr.code)
    }

    /// Send a command, receive data phase + response. Returns the data payload.
    pub fn execute_data_in(
        &mut self,
        code: OperationCode,
        params: &[u32],
    ) -> Result<Vec<u8>, MtpError> {
        let tid = self.next_transaction();
        let cmd = build_command(code, tid, params);
        self.transport.write(&cmd)?;

        let data = self.transport.read_container()?;
        let hdr = ContainerHeader::parse(&data)
            .ok_or(MtpError::Protocol("Bad data response".to_string()))?;
        if !hdr.is_data() {
            return Err(MtpError::Protocol(format!(
                "Expected data container, got type={} code=0x{:04x}",
                hdr.container_type, hdr.code
            )));
        }
        let payload = data[CONTAINER_HEADER_SIZE..].to_vec();

        let resp = self.transport.read_container()?;
        let resp_hdr =
            ContainerHeader::parse(&resp).ok_or(MtpError::Protocol("Bad response".to_string()))?;
        if !resp_hdr.is_ok() {
            return Err(MtpError::Protocol(format!(
                "Operation failed: 0x{:04x}",
                resp_hdr.code
            )));
        }

        Ok(payload)
    }

    /// Send data out: data container header + payload, then read response.
    #[allow(dead_code)]
    pub fn execute_data_out(
        &mut self,
        code: OperationCode,
        payload: &[u8],
    ) -> Result<u16, MtpError> {
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
        let hdr =
            ContainerHeader::parse(&resp).ok_or(MtpError::Protocol("Bad response".to_string()))?;
        Ok(hdr.code)
    }

    /// Send a generic MTP operation with a data payload and read the response.
    pub fn generic_operation_send(
        &mut self,
        code: OperationCode,
        data: &[u8],
        _log: &dyn Fn(&str),
    ) -> Result<(), MtpError> {
        let tid = self.next_transaction();

        // Send command container.
        let cmd = build_command(code, tid, &[]);
        self.transport.write(&cmd)?;

        // Send data container (header and payload as separate writes for Microsoft).
        let data_container = build_data(code, tid, data);
        self.transport
            .write(&data_container[..CONTAINER_HEADER_SIZE])?;
        if !data.is_empty() {
            self.transport
                .write(&data_container[CONTAINER_HEADER_SIZE..])?;
        }

        // Read response (may get data container first, then response).
        let resp = self.transport.read_container()?;
        let hdr =
            ContainerHeader::parse(&resp).ok_or(MtpError::Protocol("Bad response".to_string()))?;
        let response_code = if hdr.is_data() {
            let resp2 = self.transport.read_container()?;
            let hdr2 = ContainerHeader::parse(&resp2)
                .ok_or(MtpError::Protocol("Bad response after data".to_string()))?;
            hdr2.code
        } else {
            hdr.code
        };
        if response_code != ResponseCode::Ok as u16 {
            return Err(MtpError::Protocol(format!(
                "Operation 0x{:04x} failed: 0x{:04x}",
                code as u16, response_code
            )));
        }
        Ok(())
    }

    /// Receive data from a generic MTP operation.
    pub fn generic_operation_receive(
        &mut self,
        code: OperationCode,
        _log: &dyn Fn(&str),
    ) -> Result<Vec<u8>, MtpError> {
        self.execute_data_in(code, &[])
    }

    /// Enable MTPZ secure file operations using a CMAC-signed session token.
    pub fn enable_secure_file_operations(&mut self, cmac: [u32; 4]) -> Result<(), MtpError> {
        let resp = self.execute_simple(OperationCode::EnableTrustedFilesOperations, &cmac)?;
        if resp != ResponseCode::Ok as u16 {
            return Err(MtpError::Protocol(format!(
                "EnableSecureFileOperations failed: 0x{:04x}",
                resp
            )));
        }
        Ok(())
    }

    /// Get list of storage IDs on the device.
    pub fn get_storage_ids(&mut self) -> Result<Vec<u32>, MtpError> {
        let data = self.execute_data_in(OperationCode::GetStorageIDs, &[])?;
        Ok(parse_u32_array(&data))
    }

    /// Get storage info: returns (max_capacity, free_space) in bytes.
    pub fn get_storage_info(&mut self, storage_id: u32) -> Result<(u64, u64), MtpError> {
        let data = self.execute_data_in(OperationCode::GetStorageInfo, &[storage_id])?;
        // StorageInfo dataset: type(2) + fs_type(2) + access(2) + max_cap(8) + free(8) + ...
        if data.len() < 22 {
            return Err(MtpError::Protocol("StorageInfo too short".to_string()));
        }
        let max_capacity = le_u64(&data, 6);
        let free_space = le_u64(&data, 14);
        Ok((max_capacity, free_space))
    }

    /// Get object handles in a given storage, optionally filtered by parent.
    pub fn get_object_handles(
        &mut self,
        storage_id: u32,
        parent: u32,
    ) -> Result<Vec<u32>, MtpError> {
        let data = self.execute_data_in(
            OperationCode::GetObjectHandles,
            &[storage_id, 0x00000000, parent],
        )?;
        Ok(parse_u32_array(&data))
    }

    /// Get info about a specific object.
    pub fn get_object_info(&mut self, handle: u32) -> Result<ObjectInfo, MtpError> {
        let data = self.execute_data_in(OperationCode::GetObjectInfo, &[handle])?;
        parse_object_info(&data)
    }

    /// Delete an object by handle.
    pub fn delete_object(&mut self, handle: u32) -> Result<(), MtpError> {
        let resp = self.execute_simple(OperationCode::DeleteObject, &[handle])?;
        if resp != ResponseCode::Ok as u16 {
            return Err(MtpError::Protocol(format!(
                "DeleteObject failed: 0x{:04x}",
                resp
            )));
        }
        Ok(())
    }

    /// Send object info (prepare for upload). Returns (storage_id, parent_handle, object_handle).
    pub fn send_object_info(
        &mut self,
        storage_id: u32,
        parent: u32,
        info_dataset: &[u8],
    ) -> Result<(u32, u32, u32), MtpError> {
        let tid = self.next_transaction();
        let cmd = build_command(OperationCode::SendObjectInfo, tid, &[storage_id, parent]);
        self.transport.write(&cmd)?;

        let data_container = build_data(OperationCode::SendObjectInfo, tid, info_dataset);
        self.transport.write(&data_container)?;

        let resp = self.transport.read_container()?;
        let hdr = ContainerHeader::parse(&resp).ok_or(MtpError::Protocol(
            "Bad SendObjectInfo response".to_string(),
        ))?;
        if !hdr.is_ok() {
            return Err(MtpError::Protocol(format!(
                "SendObjectInfo failed: 0x{:04x}",
                hdr.code
            )));
        }

        // Response params: storage_id, parent_handle, object_handle
        let params = parse_response_params(&resp);
        Ok((
            params.first().copied().unwrap_or(0),
            params.get(1).copied().unwrap_or(0),
            params.get(2).copied().unwrap_or(0),
        ))
    }

    /// Send object data (upload file content).
    pub fn send_object(&mut self, data: &[u8]) -> Result<(), MtpError> {
        let tid = self.next_transaction();

        // Command container first.
        let cmd = build_command(OperationCode::SendObject, tid, &[]);
        self.transport.write(&cmd)?;

        // Data container (split header and payload for Microsoft).
        let data_container = build_data(OperationCode::SendObject, tid, data);
        self.transport
            .write(&data_container[..CONTAINER_HEADER_SIZE])?;
        if !data.is_empty() {
            self.transport
                .write(&data_container[CONTAINER_HEADER_SIZE..])?;
        }

        let resp = self.transport.read_container()?;
        let hdr = ContainerHeader::parse(&resp)
            .ok_or(MtpError::Protocol("Bad SendObject response".to_string()))?;
        if !hdr.is_ok() {
            return Err(MtpError::Protocol(format!(
                "SendObject failed: 0x{:04x}",
                hdr.code
            )));
        }
        Ok(())
    }

    /// Read a string device property (e.g., DeviceFirmwareVersion 0xD404).
    pub fn get_device_prop_string(&mut self, prop: u32) -> Result<String, MtpError> {
        let data = self.execute_data_in(OperationCode::GetDevicePropValue, &[prop])?;
        let mut offset = 0;
        Ok(read_mtp_string(&data, &mut offset))
    }

    /// Read the Device Version string from GetDeviceInfo (the firmware version).
    pub fn get_device_version(&mut self) -> Result<String, MtpError> {
        let data = self.execute_data_in(OperationCode::GetDeviceInfo, &[])?;
        if data.len() < 8 {
            return Err(MtpError::Protocol("DeviceInfo too short".to_string()));
        }
        let mut offset = 8; // skip: u16 standard ver + u32 vendor ext id + u16 vendor ext ver
                            // Skip: vendor extension description string
        read_mtp_string(&data, &mut offset);
        if offset + 2 > data.len() {
            return Err(MtpError::Protocol("DeviceInfo truncated".to_string()));
        }
        offset += 2; // skip: u16 functional mode
                     // Skip 5 u16-element arrays: operations, events, device props, capture formats, playback formats.
                     // Each array is: u32 count + count * u16 elements.
        for _ in 0..5 {
            if offset + 4 > data.len() {
                return Err(MtpError::Protocol("DeviceInfo truncated".to_string()));
            }
            let count = le_u32(&data, offset) as usize;
            offset += 4 + count * 2;
        }
        // Skip: manufacturer string, model string
        read_mtp_string(&data, &mut offset);
        read_mtp_string(&data, &mut offset);
        // Device Version string (firmware)
        let version = read_mtp_string(&data, &mut offset);
        Ok(version)
    }

    /// Set a string device property (e.g., SessionInitiatorVersionInfo 0xD406).
    pub fn set_device_prop_string(&mut self, prop: u32, value: &str) -> Result<(), MtpError> {
        let tid = self.next_transaction();
        // Command with property code as parameter.
        let cmd = build_command(OperationCode::SetDevicePropValue, tid, &[prop]);
        self.transport.write(&cmd)?;

        // Build MTP string payload.
        let chars: Vec<u16> = value.encode_utf16().collect();
        let mut payload = Vec::new();
        payload.push((chars.len() + 1) as u8); // num chars including null
        for ch in &chars {
            payload.extend_from_slice(&ch.to_le_bytes());
        }
        payload.extend_from_slice(&0u16.to_le_bytes()); // null terminator

        // Data container with property value.
        let data_container = build_data(OperationCode::SetDevicePropValue, tid, &payload);
        self.transport
            .write(&data_container[..CONTAINER_HEADER_SIZE])?;
        self.transport
            .write(&data_container[CONTAINER_HEADER_SIZE..])?;

        let resp = self.transport.read_container()?;
        let hdr = ContainerHeader::parse(&resp)
            .ok_or(MtpError::Protocol("Bad SetDeviceProp response".to_string()))?;
        if hdr.is_ok() {
            Ok(())
        } else {
            Err(MtpError::Protocol(format!(
                "SetDevicePropValue failed: 0x{:04x}",
                hdr.code
            )))
        }
    }

    /// Get supported properties for an object format (0x9806).
    pub fn get_object_props_supported(&mut self, format: u16) -> Result<Vec<u16>, MtpError> {
        let data =
            self.execute_data_in(OperationCode::GetObjectPropsSupported, &[format as u32])?;
        // Returns u16 array: [u32 count] [u16 values...]
        if data.len() < 4 {
            return Ok(Vec::new());
        }
        let count = le_u32(&data, 0) as usize;
        let mut props = Vec::with_capacity(count);
        for i in 0..count {
            let offset = 4 + i * 2;
            if offset + 2 > data.len() {
                break;
            }
            props.push(le_u16(&data, offset));
        }
        Ok(props)
    }

    /// Get object property list (0x9805) — efficient bulk property query.
    /// Returns raw property list data.
    pub fn get_object_prop_list(
        &mut self,
        object_id: u32,
        format: u32,
        property: u32,
        group_code: u32,
        depth: u32,
    ) -> Result<Vec<u8>, MtpError> {
        self.execute_data_in(
            OperationCode::GetObjectPropList,
            &[object_id, format, property, group_code, depth],
        )
    }

    /// Set a property value on an object (0x9804).
    pub fn set_object_prop_value(
        &mut self,
        object_id: u32,
        prop: u16,
        value: &[u8],
    ) -> Result<(), MtpError> {
        let tid = self.next_transaction();
        let cmd = build_command(
            OperationCode::SetObjectPropValue,
            tid,
            &[object_id, prop as u32],
        );
        self.transport.write(&cmd)?;

        let data_container = build_data(OperationCode::SetObjectPropValue, tid, value);
        self.transport
            .write(&data_container[..CONTAINER_HEADER_SIZE])?;
        if !value.is_empty() {
            self.transport
                .write(&data_container[CONTAINER_HEADER_SIZE..])?;
        }

        let resp = self.transport.read_container()?;
        let hdr = ContainerHeader::parse(&resp).ok_or(MtpError::Protocol(
            "Bad SetObjectPropValue response".to_string(),
        ))?;
        if !hdr.is_ok() {
            return Err(MtpError::Protocol(format!(
                "SetObjectPropValue failed: 0x{:04x}",
                hdr.code
            )));
        }
        Ok(())
    }

    /// Create an object with properties via SendObjectPropList (0x9808).
    /// Returns (storage_id, parent_handle, object_handle).
    pub fn send_object_prop_list(
        &mut self,
        storage_id: u32,
        parent: u32,
        format: u16,
        object_size: u64,
        prop_list: &[u8],
    ) -> Result<(u32, u32, u32), MtpError> {
        let tid = self.next_transaction();
        let size_hi = (object_size >> 32) as u32;
        let size_lo = object_size as u32;
        let cmd = build_command(
            OperationCode::SendObjectPropList,
            tid,
            &[storage_id, parent, format as u32, size_hi, size_lo],
        );
        self.transport.write(&cmd)?;

        // Send property list as data container (split for Microsoft).
        let data_container = build_data(OperationCode::SendObjectPropList, tid, prop_list);
        self.transport
            .write(&data_container[..CONTAINER_HEADER_SIZE])?;
        if !prop_list.is_empty() {
            self.transport
                .write(&data_container[CONTAINER_HEADER_SIZE..])?;
        }

        let resp = self.transport.read_container()?;
        let hdr = ContainerHeader::parse(&resp).ok_or(MtpError::Protocol(
            "Bad SendObjectPropList response".to_string(),
        ))?;
        if !hdr.is_ok() {
            return Err(MtpError::Protocol(format!(
                "SendObjectPropList failed: 0x{:04x}",
                hdr.code
            )));
        }

        let params = parse_response_params(&resp);
        Ok((
            params.first().copied().unwrap_or(0),
            params.get(1).copied().unwrap_or(0),
            params.get(2).copied().unwrap_or(0),
        ))
    }

    /// Retrieve the Zune's internal metadata database (ZMDB).
    /// Returns the raw binary ZMDB blob for the requested content type.
    /// `content_type`: 1 = music library.
    pub fn get_zmdb(&mut self, content_type: u32) -> Result<Vec<u8>, MtpError> {
        self.execute_data_in(OperationCode::GetZuneMetadataDatabase, &[content_type])
    }

    /// Query the number of items the device acquired on its own
    /// (podcast downloads, Zune-to-Zune sharing).
    /// Sends vendor operation 0x9219 (GetAcquiredItems) with no parameters.
    /// The data payload is a u32 count of acquired item IDs.
    pub fn get_acquired_items_count(&mut self) -> Result<u32, MtpError> {
        let data = self.execute_data_in(OperationCode::GetAcquiredItems, &[])?;
        if data.len() < 4 {
            return Ok(0);
        }
        Ok(le_u32(&data, 0))
    }

    /// Read the device's sync progress state (1036 bytes).
    pub fn get_sync_progress(&mut self) -> Result<Vec<u8>, MtpError> {
        self.execute_data_in(OperationCode::GetDeviceSyncProgress, &[])
    }

    /// Write sync progress state back to the device.
    /// Payload must be exactly 530 bytes. The device validates the content
    /// and rejects arbitrary data — only data previously read from the device
    /// (or minor modifications) is accepted.
    pub fn set_sync_progress(&mut self, data: &[u8]) -> Result<(), MtpError> {
        if data.len() != 530 {
            return Err(MtpError::Protocol(format!(
                "Sync progress payload must be 530 bytes, got {}",
                data.len()
            )));
        }
        let resp = self.execute_data_out(OperationCode::SetDeviceSyncProgress, data)?;
        if resp != ResponseCode::Ok as u16 {
            return Err(MtpError::Protocol(format!(
                "SetDeviceSyncProgress failed: 0x{resp:04x}"
            )));
        }
        Ok(())
    }

    /// Get object references (linked objects like album tracks).
    pub fn get_object_references(&mut self, object_id: u32) -> Result<Vec<u32>, MtpError> {
        let data = self.execute_data_in(OperationCode::GetObjectReferences, &[object_id])?;
        Ok(parse_u32_array(&data))
    }

    /// Set object references (link objects together).
    pub fn set_object_references(&mut self, object_id: u32, refs: &[u32]) -> Result<(), MtpError> {
        let tid = self.next_transaction();
        let cmd = build_command(OperationCode::SetObjectReferences, tid, &[object_id]);
        self.transport.write(&cmd)?;

        // Build u32 array payload: [count][values...]
        let mut payload = Vec::with_capacity(4 + refs.len() * 4);
        payload.extend_from_slice(&(refs.len() as u32).to_le_bytes());
        for r in refs {
            payload.extend_from_slice(&r.to_le_bytes());
        }
        let data_container = build_data(OperationCode::SetObjectReferences, tid, &payload);
        self.transport
            .write(&data_container[..CONTAINER_HEADER_SIZE])?;
        self.transport
            .write(&data_container[CONTAINER_HEADER_SIZE..])?;

        let resp = self.transport.read_container()?;
        let hdr = ContainerHeader::parse(&resp).ok_or(MtpError::Protocol(
            "Bad SetObjectReferences response".to_string(),
        ))?;
        if !hdr.is_ok() {
            return Err(MtpError::Protocol(format!(
                "SetObjectReferences failed: 0x{:04x}",
                hdr.code
            )));
        }
        Ok(())
    }
}

impl Drop for MtpSession {
    fn drop(&mut self) {
        let _ = self.execute_simple(OperationCode::CloseSession, &[]);
    }
}

/// Read a little-endian u16 from a byte slice at the given offset.
fn le_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

/// Read a little-endian u32 from a byte slice at the given offset.
fn le_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}

/// Read a little-endian u64 from a byte slice at the given offset.
fn le_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
        data[offset + 4],
        data[offset + 5],
        data[offset + 6],
        data[offset + 7],
    ])
}

/// Parse MTP u32 array: [u32 count] [u32 values...]
fn parse_u32_array(data: &[u8]) -> Vec<u32> {
    if data.len() < 4 {
        return vec![];
    }
    let count = le_u32(data, 0) as usize;
    let mut result = Vec::with_capacity(count);
    for i in 0..count {
        let offset = 4 + i * 4;
        if offset + 4 > data.len() {
            break;
        }
        result.push(le_u32(data, offset));
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
        let ch = le_u16(data, *offset);
        *offset += 2;
        if ch != 0 {
            chars.push(ch);
        }
    }
    String::from_utf16_lossy(&chars)
}

/// Parse the MTP ObjectInfo dataset.
fn parse_object_info(data: &[u8]) -> Result<ObjectInfo, MtpError> {
    if data.len() < 52 {
        return Err(MtpError::Protocol("ObjectInfo too short".to_string()));
    }

    let storage_id = le_u32(data, 0);
    let object_format = le_u16(data, 4);
    let compressed_size = le_u32(data, 8);
    let association_type = le_u16(data, 42);

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

/// Parse response parameters from a response container.
fn parse_response_params(resp: &[u8]) -> Vec<u32> {
    let mut params = Vec::new();
    let mut offset = CONTAINER_HEADER_SIZE;
    while offset + 4 <= resp.len() {
        params.push(le_u32(resp, offset));
        offset += 4;
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_u32_array_valid() {
        // count=3, values=[10, 20, 30]
        let mut data = Vec::new();
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&10u32.to_le_bytes());
        data.extend_from_slice(&20u32.to_le_bytes());
        data.extend_from_slice(&30u32.to_le_bytes());
        assert_eq!(parse_u32_array(&data), vec![10, 20, 30]);
    }

    #[test]
    fn parse_u32_array_empty() {
        let data = 0u32.to_le_bytes();
        assert_eq!(parse_u32_array(&data), vec![]);
    }

    #[test]
    fn parse_u32_array_too_short() {
        assert_eq!(parse_u32_array(&[1, 2]), vec![]);
    }

    #[test]
    fn parse_u32_array_truncated() {
        // count says 2 but only 1 value present
        let mut data = Vec::new();
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(&42u32.to_le_bytes());
        assert_eq!(parse_u32_array(&data), vec![42]);
    }

    #[test]
    fn read_mtp_string_valid() {
        // "Hi" in UCS-2LE: num_chars=3, 'H'=0x0048, 'i'=0x0069, null=0x0000
        let data = [3, 0x48, 0x00, 0x69, 0x00, 0x00, 0x00];
        let mut offset = 0;
        assert_eq!(read_mtp_string(&data, &mut offset), "Hi");
        assert_eq!(offset, 7);
    }

    #[test]
    fn read_mtp_string_empty() {
        let data = [0u8]; // num_chars = 0
        let mut offset = 0;
        assert_eq!(read_mtp_string(&data, &mut offset), "");
        assert_eq!(offset, 1);
    }

    #[test]
    fn read_mtp_string_out_of_bounds() {
        let data: [u8; 0] = [];
        let mut offset = 0;
        assert_eq!(read_mtp_string(&data, &mut offset), "");
    }

    #[test]
    fn read_mtp_string_truncated() {
        // Says 5 chars but only has data for 1
        let data = [5, 0x41, 0x00];
        let mut offset = 0;
        let result = read_mtp_string(&data, &mut offset);
        assert_eq!(result, "A");
    }

    #[test]
    fn parse_object_info_valid() {
        let mut data = vec![0u8; 52];
        // storage_id = 65537
        data[0..4].copy_from_slice(&65537u32.to_le_bytes());
        // object_format = 0x3001 (Association/folder)
        data[4..6].copy_from_slice(&0x3001u16.to_le_bytes());
        // compressed_size = 1024
        data[8..12].copy_from_slice(&1024u32.to_le_bytes());
        // association_type = 1
        data[42..44].copy_from_slice(&1u16.to_le_bytes());
        // filename: "test.mp3" in MTP string format
        // num_chars=9 (8 chars + null)
        data.push(9);
        for ch in "test.mp3".encode_utf16() {
            data.extend_from_slice(&ch.to_le_bytes());
        }
        data.extend_from_slice(&0u16.to_le_bytes()); // null terminator
                                                     // empty capture_date and modification_date
        data.push(0);
        data.push(0);

        let info = parse_object_info(&data).unwrap();
        assert_eq!(info.storage_id, 65537);
        assert_eq!(info.object_format, 0x3001);
        assert_eq!(info.compressed_size, 1024);
        assert_eq!(info.association_type, 1);
        assert_eq!(info.filename, "test.mp3");
    }

    #[test]
    fn parse_object_info_too_short() {
        assert!(parse_object_info(&[0u8; 10]).is_err());
    }

    #[test]
    fn parse_response_params_valid() {
        // 12-byte header + 2 params
        let mut resp = vec![0u8; 12];
        resp.extend_from_slice(&100u32.to_le_bytes());
        resp.extend_from_slice(&200u32.to_le_bytes());
        let params = parse_response_params(&resp);
        assert_eq!(params, vec![100, 200]);
    }

    #[test]
    fn parse_response_params_none() {
        let resp = vec![0u8; 12]; // header only, no params
        assert!(parse_response_params(&resp).is_empty());
    }
}
