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
        self.execute_data_in_raw(code as u16, params)
    }

    /// Variant of `execute_data_in` that takes a raw u16 operation code.
    /// Used by probe tooling to call vendor operations not in the
    /// `OperationCode` enum (avoids the UB of transmuting an arbitrary u16
    /// to an enum). Same data-in semantics; same error handling.
    pub fn execute_data_in_raw(
        &mut self,
        op_code: u16,
        params: &[u32],
    ) -> Result<Vec<u8>, MtpError> {
        let tid = self.next_transaction();
        let cmd = crate::container::build_command_raw(op_code, tid, params);
        self.transport.write(&cmd)?;

        let data = self.transport.read_container()?;
        let hdr = ContainerHeader::parse(&data)
            .ok_or(MtpError::Protocol("Bad data response".to_string()))?;
        if !hdr.is_data() {
            // The device skipped the data phase and went straight to a
            // response container — this is how it signals "operation not
            // supported" (0x2005) and similar rejections for vendor ops.
            return Err(MtpError::DeviceRejected(hdr.code));
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

        // SendObject commits the payload to flash before ACKing; on the Zune
        // 30's USB 1.1 link a multi-MB track can take tens of seconds,
        // especially when it lands right after an art commit. Use the same
        // 45s window we give SetObjectPropValue so transient slow commits
        // don't cascade into a wedged session.
        let resp = self.transport.read_container_with_timeout(45)?;
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
    ///
    /// Common parameter forms:
    ///   - One object, all properties: `(handle, 0, 0xFFFFFFFF, 0, 0)`
    ///   - All objects of a format, one property: `(0xFFFFFFFF, format, prop, 0, 0)`
    ///   - One object, one property: `(handle, 0, prop, 0, 0)`
    ///
    /// Devices commonly serve properties via this op even when they don't
    /// advertise them through `GetObjectPropsSupported` — useful on Zune
    /// v1.4 where standard MTP-AAS props aren't all listed.
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

    /// Read a single property value for a specific object (0x9803).
    /// Returns the raw value bytes — caller decodes per the property's MTP
    /// data type (u16, u32, string, etc.). For reading many props from many
    /// objects, prefer `get_object_prop_list` to batch over the wire.
    pub fn get_object_prop_value(
        &mut self,
        object_id: u32,
        prop: u16,
    ) -> Result<Vec<u8>, MtpError> {
        self.execute_data_in(OperationCode::GetObjectPropValue, &[object_id, prop as u32])
    }

    /// Convenience: read a u32-typed object property. Returns `Ok(None)` when
    /// the device returns no value bytes (interpreted as "unset"); returns
    /// an error for any other short read.
    pub fn get_object_prop_u32(
        &mut self,
        object_id: u32,
        prop: u16,
    ) -> Result<Option<u32>, MtpError> {
        let bytes = self.get_object_prop_value(object_id, prop)?;
        if bytes.is_empty() {
            return Ok(None);
        }
        if bytes.len() < 4 {
            return Err(MtpError::Protocol(format!(
                "GetObjectPropValue(0x{prop:04x}) returned {} bytes; need 4 for u32",
                bytes.len()
            )));
        }
        Ok(Some(le_u32(&bytes, 0)))
    }

    /// Convenience: read a u16-typed object property. Same `Ok(None)` /
    /// short-read semantics as `get_object_prop_u32`.
    pub fn get_object_prop_u16(
        &mut self,
        object_id: u32,
        prop: u16,
    ) -> Result<Option<u16>, MtpError> {
        let bytes = self.get_object_prop_value(object_id, prop)?;
        if bytes.is_empty() {
            return Ok(None);
        }
        if bytes.len() < 2 {
            return Err(MtpError::Protocol(format!(
                "GetObjectPropValue(0x{prop:04x}) returned {} bytes; need 2 for u16",
                bytes.len()
            )));
        }
        Ok(Some(le_u16(&bytes, 0)))
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

        // Large prop writes (album art in particular) can take many seconds
        // to commit to flash under load. 30s (the default) is too tight, but
        // 90s is excruciating when the device has wedged and isn't coming
        // back. 45s is a workable compromise.
        let resp = self.transport.read_container_with_timeout(45)?;
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

    /// Retrieve the device's sync progress state (vendor op 0x922f).
    /// Returns the raw 1036-byte payload. The first u32 is a version/status flag;
    /// the remainder contains sync counters and timestamps (mostly zeros when idle).
    pub fn get_sync_progress(&mut self) -> Result<Vec<u8>, MtpError> {
        self.execute_data_in(OperationCode::GetDeviceSyncProgress, &[])
    }

    /// Write sync progress state back to the device.
    /// Payload must be exactly 530 bytes.
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

/// One element of a parsed `GetObjectPropList` response: which object,
/// which property, the property's MTP data type, and the raw value bytes.
/// Callers decode `value` per `datatype` (see MTP-AAS spec table 9.5.6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropListElement {
    pub object_handle: u32,
    pub prop_code: u16,
    pub datatype: u16,
    pub value: Vec<u8>,
}

impl PropListElement {
    /// Decode the value as a u32 if the datatype is UINT32 (`0x0006`).
    pub fn as_u32(&self) -> Option<u32> {
        if self.datatype == 0x0006 && self.value.len() >= 4 {
            Some(le_u32(&self.value, 0))
        } else {
            None
        }
    }

    /// Decode the value as a u16 if the datatype is UINT16 (`0x0004`).
    pub fn as_u16(&self) -> Option<u16> {
        if self.datatype == 0x0004 && self.value.len() >= 2 {
            Some(le_u16(&self.value, 0))
        } else {
            None
        }
    }

    /// Decode the value as a string if the datatype is STR (`0xFFFF`).
    pub fn as_string(&self) -> Option<String> {
        if self.datatype != 0xFFFF {
            return None;
        }
        let mut offset = 0;
        Some(read_mtp_string(&self.value, &mut offset))
    }
}

/// Parse a GetObjectPropList (`0x9805`) response payload into individual
/// property elements. Best-effort: stops at the first element it can't
/// decode (e.g. an unknown variable-length type) instead of erroring, so
/// partial responses are still useful for diagnostics.
pub fn parse_object_prop_list(data: &[u8]) -> Vec<PropListElement> {
    if data.len() < 4 {
        return Vec::new();
    }
    let count = le_u32(data, 0) as usize;
    // `count` is attacker-controlled (especially via `probe vendor-op`,
    // which interprets arbitrary opcode responses through this parser).
    // Each element occupies at least 9 bytes (8-byte header + ≥1-byte
    // value), so cap the initial allocation at what the buffer could
    // possibly hold. Without this clamp, `count = 0xFFFFFFFF` would
    // attempt a multi-GB up-front allocation.
    let cap = count.min(data.len().saturating_sub(4) / 9);
    let mut out = Vec::with_capacity(cap);
    let mut offset = 4;
    for _ in 0..count {
        // Header: handle (4) + prop_code (2) + datatype (2) = 8 bytes.
        if offset + 8 > data.len() {
            break;
        }
        let object_handle = le_u32(data, offset);
        let prop_code = le_u16(data, offset + 4);
        let datatype = le_u16(data, offset + 6);
        offset += 8;
        let Some((value, consumed)) = read_typed_value(&data[offset..], datatype) else {
            break;
        };
        offset += consumed;
        out.push(PropListElement {
            object_handle,
            prop_code,
            datatype,
            value,
        });
    }
    out
}

/// Read a fixed/variable-length MTP value of `datatype` from the head of
/// `data`. Returns the raw value bytes and how many input bytes were
/// consumed, or `None` if the datatype is unknown or the buffer is short.
fn read_typed_value(data: &[u8], datatype: u16) -> Option<(Vec<u8>, usize)> {
    // Fixed-width scalar types — table from MTP-AAS spec §9.5.6.1.
    let scalar_len = match datatype {
        0x0001 | 0x0002 => Some(1),  // INT8 / UINT8
        0x0003 | 0x0004 => Some(2),  // INT16 / UINT16
        0x0005 | 0x0006 => Some(4),  // INT32 / UINT32
        0x0007 | 0x0008 => Some(8),  // INT64 / UINT64
        0x0009 | 0x000A => Some(16), // INT128 / UINT128
        _ => None,
    };
    if let Some(n) = scalar_len {
        if data.len() < n {
            return None;
        }
        return Some((data[..n].to_vec(), n));
    }

    // Array types: [u32 count] followed by `count` elements of the
    // matching scalar size. 0x4000 base + scalar code's low nibble.
    if (0x4001..=0x400A).contains(&datatype) {
        let elem_len = match datatype {
            0x4001 | 0x4002 => 1,
            0x4003 | 0x4004 => 2,
            0x4005 | 0x4006 => 4,
            0x4007 | 0x4008 => 8,
            0x4009 | 0x400A => 16,
            _ => return None,
        };
        if data.len() < 4 {
            return None;
        }
        let count = le_u32(data, 0) as usize;
        // Checked arithmetic — `count` is read off the wire and unbounded;
        // on 32-bit targets `count * elem_len` (up to 16) can wrap to a
        // small value, which would let the subsequent length check pass
        // and slice past end of buffer.
        let total = count.checked_mul(elem_len)?.checked_add(4)?;
        if data.len() < total {
            return None;
        }
        return Some((data[..total].to_vec(), total));
    }

    // STR: [u8 num_chars] [u16 chars...]. Always UCS-2LE, including the
    // null terminator in the count when present.
    if datatype == 0xFFFF {
        if data.is_empty() {
            return None;
        }
        let num_chars = data[0] as usize;
        let total = 1 + num_chars * 2;
        if data.len() < total {
            return None;
        }
        return Some((data[..total].to_vec(), total));
    }

    None
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

    /// Guard against accidental edits to the playcount-related MTP property
    /// codes — Phase 4a probes and Phase 4b production code both depend on
    /// these matching the libmtp / MTP spec values exactly.
    #[test]
    fn playcount_prop_codes_match_mtp_spec() {
        assert_eq!(crate::proplist::PROP_USE_COUNT, 0xDC91);
        assert_eq!(crate::proplist::PROP_SKIP_COUNT, 0xDC92);
        assert_eq!(crate::proplist::PROP_LAST_ACCESSED, 0xDC93);
        assert_eq!(crate::proplist::PROP_RATING, 0xDC8A);
        assert_eq!(crate::proplist::PROP_DATE_ADDED, 0xDC4E);
    }

    #[test]
    fn get_object_prop_value_op_code_matches_mtp_spec() {
        assert_eq!(OperationCode::GetObjectPropValue as u16, 0x9803);
    }

    /// Build a minimal `GetObjectPropList` payload with one UINT32 element
    /// (handle=0x100, prop=0xDC91 UseCount, value=42) and confirm the
    /// parser surfaces it.
    #[test]
    fn parse_object_prop_list_decodes_uint32() {
        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_le_bytes()); // count
        data.extend_from_slice(&0x100u32.to_le_bytes()); // handle
        data.extend_from_slice(&0xDC91u16.to_le_bytes()); // prop
        data.extend_from_slice(&0x0006u16.to_le_bytes()); // datatype UINT32
        data.extend_from_slice(&42u32.to_le_bytes()); // value
        let elements = parse_object_prop_list(&data);
        assert_eq!(elements.len(), 1);
        let e = &elements[0];
        assert_eq!(e.object_handle, 0x100);
        assert_eq!(e.prop_code, 0xDC91);
        assert_eq!(e.datatype, 0x0006);
        assert_eq!(e.as_u32(), Some(42));
        assert_eq!(e.as_u16(), None);
        assert_eq!(e.as_string(), None);
    }

    #[test]
    fn parse_object_prop_list_decodes_string() {
        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_le_bytes()); // count
        data.extend_from_slice(&0x200u32.to_le_bytes()); // handle
        data.extend_from_slice(&0xDC44u16.to_le_bytes()); // prop Name
        data.extend_from_slice(&0xFFFFu16.to_le_bytes()); // datatype STR
                                                          // String: "Hi" + null terminator = 3 chars.
        data.push(3);
        data.extend_from_slice(&0x0048u16.to_le_bytes()); // 'H'
        data.extend_from_slice(&0x0069u16.to_le_bytes()); // 'i'
        data.extend_from_slice(&0x0000u16.to_le_bytes()); // null
        let elements = parse_object_prop_list(&data);
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].as_string().as_deref(), Some("Hi"));
    }

    #[test]
    fn parse_object_prop_list_decodes_multiple_elements() {
        let mut data = Vec::new();
        data.extend_from_slice(&3u32.to_le_bytes()); // count
                                                     // (handle=10, prop=0xDC91 UseCount UINT32, value=7)
        data.extend_from_slice(&10u32.to_le_bytes());
        data.extend_from_slice(&0xDC91u16.to_le_bytes());
        data.extend_from_slice(&0x0006u16.to_le_bytes());
        data.extend_from_slice(&7u32.to_le_bytes());
        // (handle=10, prop=0xDC8B Track UINT16, value=3)
        data.extend_from_slice(&10u32.to_le_bytes());
        data.extend_from_slice(&0xDC8Bu16.to_le_bytes());
        data.extend_from_slice(&0x0004u16.to_le_bytes());
        data.extend_from_slice(&3u16.to_le_bytes());
        // (handle=10, prop=0xDC93 LastAccessed UINT64, value=1234567890)
        data.extend_from_slice(&10u32.to_le_bytes());
        data.extend_from_slice(&0xDC93u16.to_le_bytes());
        data.extend_from_slice(&0x0008u16.to_le_bytes());
        data.extend_from_slice(&1234567890u64.to_le_bytes());

        let elements = parse_object_prop_list(&data);
        assert_eq!(elements.len(), 3);
        assert_eq!(elements[0].as_u32(), Some(7));
        assert_eq!(elements[1].as_u16(), Some(3));
        // u64 value not in our convenience accessors; check raw bytes.
        assert_eq!(elements[2].value.len(), 8);
        assert_eq!(
            u64::from_le_bytes(elements[2].value.as_slice().try_into().unwrap()),
            1234567890
        );
    }

    #[test]
    fn parse_object_prop_list_stops_on_truncation() {
        // Header claims 2 elements but the second is truncated mid-value.
        let mut data = Vec::new();
        data.extend_from_slice(&2u32.to_le_bytes()); // count
        data.extend_from_slice(&0x100u32.to_le_bytes());
        data.extend_from_slice(&0xDC91u16.to_le_bytes());
        data.extend_from_slice(&0x0006u16.to_le_bytes());
        data.extend_from_slice(&42u32.to_le_bytes());
        // Second element header but only 1 byte of the 4-byte UINT32 value.
        data.extend_from_slice(&0x100u32.to_le_bytes());
        data.extend_from_slice(&0xDC92u16.to_le_bytes());
        data.extend_from_slice(&0x0006u16.to_le_bytes());
        data.push(0xFF);

        let elements = parse_object_prop_list(&data);
        assert_eq!(elements.len(), 1, "must return only the complete element");
        assert_eq!(elements[0].as_u32(), Some(42));
    }

    #[test]
    fn parse_object_prop_list_returns_empty_for_short_input() {
        assert!(parse_object_prop_list(&[]).is_empty());
        assert!(parse_object_prop_list(&[0, 0, 0]).is_empty());
    }

    /// A malformed payload claiming `0xFFFFFFFF` elements in a tiny buffer
    /// must not attempt a multi-GB allocation. The capacity clamp keeps the
    /// up-front allocation proportional to the input size; the per-element
    /// loop then bails on the first short read.
    #[test]
    fn parse_object_prop_list_clamps_capacity_on_giant_count() {
        let mut data = Vec::new();
        data.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes()); // claimed count
                                                              // First element header but no value bytes — read_typed_value
                                                              // returns None, the loop breaks out.
        data.extend_from_slice(&0x100u32.to_le_bytes());
        data.extend_from_slice(&0xDC91u16.to_le_bytes());
        data.extend_from_slice(&0x0006u16.to_le_bytes());

        let elements = parse_object_prop_list(&data);
        assert!(
            elements.is_empty(),
            "must not surface bogus elements from malformed count"
        );
    }

    /// An array-typed value (`AUINT32`) claiming `0xFFFFFFFF` elements in a
    /// tiny buffer must reject via checked arithmetic instead of wrapping
    /// to a small `total` that would let the subsequent bounds check pass.
    #[test]
    fn parse_object_prop_list_rejects_giant_array_length() {
        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_le_bytes()); // one prop element
        data.extend_from_slice(&0x100u32.to_le_bytes()); // handle
        data.extend_from_slice(&0xDC91u16.to_le_bytes()); // prop
        data.extend_from_slice(&0x4006u16.to_le_bytes()); // datatype AUINT32
        data.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes()); // claimed array count
                                                              // No actual array bytes follow.

        let elements = parse_object_prop_list(&data);
        assert!(
            elements.is_empty(),
            "must not slice past end of buffer when array length is bogus"
        );
    }
}
