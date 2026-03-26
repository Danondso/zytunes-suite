# Porting aft-mtp-cli from C++ to Native Rust via IOKit

## Why Port?

zytunes originally communicated with the Microsoft Zune over USB by shelling out to `aft-mtp-cli`, a fork of [android-file-transfer-linux](https://github.com/whoozle/android-file-transfer-linux). This C++ subprocess handled all MTP (Media Transfer Protocol) and MTPZ (encrypted MTP) communication. While functional, this architecture had significant drawbacks:

**Subprocess overhead.** Every MTP operation — listing files, uploading tracks, deleting objects — went through a text-based IPC protocol. The Rust TUI spawned `aft-mtp-cli` as a child process, wrote commands to its stdin, parsed text responses from stdout, and monitored stderr for errors via background threads. This added latency, complexity, and fragility.

**The libusb problem.** The reason for the subprocess in the first place: Rust's `rusb` crate (which wraps libusb) fails on macOS for USB bulk-OUT transfers to the Zune. The MTPZ handshake requires sending an encrypted certificate to the device, which consistently returned `GeneralError (0x2002)` via libusb. Apple's IOKit framework handles the same transfers successfully — but IOKit is a macOS-only C API with COM-like vtable interfaces, not something you can use from Rust without significant FFI work.

**Build complexity.** Users needed to compile the C++ aft-mtp-cli from source (requiring CMake, OpenSSL, TagLib, and IOKit headers), maintain the vendored fork in `aft/`, and ensure the binary was discoverable at runtime.

**Debugging opacity.** When things went wrong (and they did — the Zune is finicky), the text-based IPC made it hard to diagnose whether the issue was in the Rust code, the subprocess communication, or the C++ MTP implementation.

## What Was Done

### Phase 1: IOKit FFI (the hard part)

The core challenge was replacing libusb with native IOKit USB communication. This required:

**Raw FFI declarations** for IOKit's COM-like USB interfaces. IOKit uses a vtable pattern where each "interface" is a pointer-to-pointer-to-struct-of-function-pointers. We declared Rust `#[repr(C)]` structs matching the exact vtable layouts of `IOUSBDeviceInterface` and `IOUSBInterfaceInterface`, including every method at the correct offset.

Getting the vtable layout right was the hardest part. The IOKit headers define multiple versions of each interface (100, 182, 187, 300, 320, 500), each extending the previous with more methods. We needed version 182 for `USBDeviceSuspend` (critical for waking the Zune from USB suspend) while keeping the base vtable methods at correct offsets.

**UUID-based interface lookup.** IOKit uses CoreFoundation UUIDs to identify interface versions. We had to get the exact UUID bytes for `kIOUSBDeviceInterfaceID182` and `kIOUSBInterfaceInterfaceID100` — a typo in even one byte would cause `E_NOINTERFACE` or a bus error from calling the wrong vtable slot.

**Device wakeup.** A critical discovery: the Zune enters USB suspend after being plugged in, and `GetDeviceInfo` fails with `0x2006` (ParameterNotSupported) until the device is woken via `USBDeviceSuspend(dev, 0)`. Without this, the MTPZ handshake would succeed for the certificate exchange but fail on the confirmation step — a bug that took hours of USB trace comparison to diagnose.

### Phase 2: MTP Protocol

With working USB bulk I/O, we reimplemented the MTP protocol layer:

- **Container format** — MTP uses a container-based protocol with 12-byte headers (length, type, code, transaction ID). Commands, data, and responses each have their own container type.
- **Session management** — OpenSession, CloseSession, GetDeviceInfo, with the Zune-specific quirk that OpenSession must use transaction ID 0 (matching aft's behavior).
- **Object operations** — GetObjectHandles, GetObjectInfo, DeleteObject, SendObjectPropList, SendObject, GetObjectReferences, SetObjectReferences.
- **Microsoft bulk write splitting** — The Zune (and other Microsoft MTP devices) requires data containers to be sent as two separate USB writes: the 12-byte header first, then the payload. Sending them as one combined write causes `GeneralError`.

### Phase 3: MTPZ Cryptography

The Zune requires MTPZ authentication before any file operations. This is a 5-step handshake:

1. **EndTrustedAppSession** — Clear any stale MTPZ state
2. **SendWMDRMPDAppRequest** — Send our RSA-signed certificate with a random challenge
3. **GetWMDRMPDAppResponse** — Receive the device's response (RSA-encrypted, contains AES key)
4. **SendWMDRMPDAppRequest** — Send AES-CMAC confirmation
5. **EnableTrustedFilesOperations** — Enable secure file operations with session CMAC

The crypto implementation includes:
- **RSA-1024** raw operations via `num-bigint` (modular exponentiation)
- **SHA-1 based HKDF** for key derivation (counter-mode)
- **AES-128-CBC** decryption for the device response payload
- **AES-128-CMAC** for the confirmation message and session enablement

All crypto was verified against both RFC 4493 test vectors and OpenSSL command-line output.

### Phase 4: Import Flow (SendObjectPropList)

The Zune doesn't accept standard MTP `SendObjectInfo` + `SendObject` for importing music. Instead, it requires `SendObjectPropList` (opcode 0x9808), which creates an object with metadata properties in a single operation. The property list uses a binary format:

```
[u32 count]
[per property: u32 handle=0, u16 prop_code, u16 data_type, value]
```

Strings use MTP's `[u8 num_chars] [u16 UCS-2LE chars...] [u16 null]` encoding — getting this wrong (e.g., using u32 for the length prefix) produces `InvalidObjectPropCode (0xa801)`.

### Phase 5: Track Caching

MTP over USB 1.1 is slow — listing 176 tracks takes ~80 seconds due to individual `GetObjectInfo` round-trips. We implemented a disk cache (`~/.zytunes-track-cache`) that saves the track listing after the first scan. Subsequent connections load from cache instantly. The cache updates incrementally: imports append entries, removals filter them out.

## Debugging War Stories

**The confirmation mystery.** The MTPZ certificate exchange succeeded (device verified our cert, echoed our challenge), but the confirmation step consistently returned `0x2002`. We proved every crypto component correct individually — CMAC matched OpenSSL, RSA roundtripped perfectly, key extraction from the decrypted payload was byte-accurate. The fix turned out to be `USBDeviceSuspend(0)` — without it, `GetDeviceInfo` silently fails, and the subsequent MTPZ state is subtly wrong even though the handshake appears to work up to the confirmation.

**The vtable bus error.** Early attempts crashed with `SIGBUS` because the IOKit vtable struct included `IOCFPLUGINBASE` fields (version, revision, Probe, Start, Stop) that don't exist on interfaces obtained via `QueryInterface`. These 5 extra pointer-sized entries shifted every subsequent method by 40 bytes, causing `USBDeviceOpen` to actually call whatever function lived at the `GetDeviceProtocol` offset.

**UUID typos.** The `kIOUSBDeviceInterfaceID100` UUID was initially transcribed with wrong bytes in the last segment (`065314` instead of `052861`), causing `QueryInterface` to return `E_NOINTERFACE (-2147483644)`.

## Architecture

```
zune-mtp/                    # Standalone workspace crate
├── src/
│   ├── lib.rs                # Public API: MtpSession, MtpzKeys
│   ├── iokit_ffi.rs          # Raw IOKit FFI declarations
│   ├── transport.rs          # IOKit USB bulk I/O (WritePipe/ReadPipe)
│   ├── container.rs          # MTP container format and operation codes
│   ├── session.rs            # MTP session operations
│   ├── mtpz.rs               # MTPZ authentication handshake
│   └── proplist.rs           # SendObjectPropList property builder
└── examples/
    ├── test_connect.rs       # Connection + MTPZ test
    ├── test_import.rs        # Track import test
    └── test_tracks.rs        # Track listing performance test
```

The crate is macOS-only (IOKit is Apple's framework) and links against `IOKit.framework` and `CoreFoundation.framework` via `#[link(kind = "framework")]`.

## Results

- **No subprocess.** Direct in-process USB communication via IOKit
- **No C++ dependency.** Pure Rust (plus IOKit FFI)
- **No OpenSSL.** Crypto via `aes`, `cmac`, `sha1`, `num-bigint` crates
- **Automatic fallback.** If the native backend fails, aft-mtp-cli is used as a seamless fallback
- **113 tests** covering MTP containers, property lists, CMAC/HKDF crypto, response parsing, and metadata handling
