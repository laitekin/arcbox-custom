//! A host-side read-only boundary; guest uid and mount options are untrusted.

use crate::fuse::{FUSE_SETUPMAPPING_FLAG_WRITE, FuseOpcode, FuseSetupMappingIn};

pub(super) fn mutates(opcode: FuseOpcode, body: &[u8]) -> bool {
    match opcode {
        FuseOpcode::Setattr
        | FuseOpcode::Symlink
        | FuseOpcode::Mknod
        | FuseOpcode::Mkdir
        | FuseOpcode::Unlink
        | FuseOpcode::Rmdir
        | FuseOpcode::Rename
        | FuseOpcode::Rename2
        | FuseOpcode::Link
        | FuseOpcode::Write
        | FuseOpcode::Create
        | FuseOpcode::Setxattr
        | FuseOpcode::Removexattr
        | FuseOpcode::Fallocate
        | FuseOpcode::CopyFileRange => true,
        FuseOpcode::Open => body.get(..4).is_some_and(|bytes| {
            let flags = u32::from_ne_bytes(bytes.try_into().unwrap());
            // These are Linux guest wire flags, not Darwin libc O_* values:
            // O_ACCMODE, O_CREAT, O_TRUNC, O_APPEND.
            flags & (0o3 | 0o100 | 0o1000 | 0o2000) != 0
        }),
        FuseOpcode::Access => body.get(..4).is_some_and(|bytes| {
            u32::from_ne_bytes(bytes.try_into().unwrap()) & 2 != 0 // W_OK
        }),
        FuseOpcode::SetupMapping => {
            let offset = std::mem::offset_of!(FuseSetupMappingIn, flags);
            body.get(offset..offset + 8).is_some_and(|bytes| {
                u64::from_ne_bytes(bytes.try_into().unwrap()) & FUSE_SETUPMAPPING_FLAG_WRITE != 0
            })
        }
        _ => false,
    }
}
