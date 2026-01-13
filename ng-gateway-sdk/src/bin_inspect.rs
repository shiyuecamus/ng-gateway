use goblin::{
    elf::{
        self,
        header::{EM_AARCH64, EM_ARM, EM_X86_64},
    },
    mach, Object,
};
use serde_repr::{Deserialize_repr, Serialize_repr};
use std::{ffi::OsStr, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum BinaryOsType {
    Windows,
    Linux,
    Mac,
    Unknown,
}

impl BinaryOsType {
    #[inline]
    pub fn current() -> Self {
        if cfg!(target_os = "windows") {
            BinaryOsType::Windows
        } else if cfg!(target_os = "linux") {
            BinaryOsType::Linux
        } else if cfg!(target_os = "macos") {
            BinaryOsType::Mac
        } else {
            BinaryOsType::Unknown
        }
    }

    #[inline]
    pub fn expected_driver_ext(&self) -> &str {
        match self {
            BinaryOsType::Windows => "dll",
            BinaryOsType::Linux => "so",
            BinaryOsType::Mac => "dylib",
            BinaryOsType::Unknown => "",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(i16)]
pub enum BinaryArch {
    X86_64,
    Arm64,
    Arm,
    Unknown,
}

impl BinaryArch {
    #[inline]
    pub fn current() -> Self {
        if cfg!(target_arch = "x86_64") {
            BinaryArch::X86_64
        } else if cfg!(target_arch = "aarch64") {
            BinaryArch::Arm64
        } else if cfg!(target_arch = "arm") {
            BinaryArch::Arm
        } else {
            BinaryArch::Unknown
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BinaryInfo {
    pub os_type: BinaryOsType,
    pub arch: BinaryArch,
}

#[inline]
pub fn inspect_binary(bytes: &[u8]) -> BinaryInfo {
    match Object::parse(bytes) {
        Ok(Object::Elf(elf_obj)) => map_elf(&elf_obj),
        Ok(Object::Mach(mach_obj)) => map_mach(&mach_obj),
        Ok(Object::PE(_)) => BinaryInfo {
            os_type: BinaryOsType::Windows,
            arch: BinaryArch::Unknown,
        },
        _ => BinaryInfo {
            os_type: BinaryOsType::Unknown,
            arch: BinaryArch::Unknown,
        },
    }
}

#[inline]
fn map_elf(elf_obj: &elf::Elf) -> BinaryInfo {
    let arch = match elf_obj.header.e_machine {
        EM_X86_64 => BinaryArch::X86_64,
        EM_AARCH64 => BinaryArch::Arm64,
        EM_ARM => BinaryArch::Arm,
        _ => BinaryArch::Unknown,
    };
    BinaryInfo {
        os_type: BinaryOsType::Linux,
        arch,
    }
}

#[inline]
fn map_mach(mach_obj: &mach::Mach) -> BinaryInfo {
    match mach_obj {
        mach::Mach::Binary(single) => BinaryInfo {
            os_type: BinaryOsType::Mac,
            arch: map_mach_arch(single.header.cputype()),
        },
        mach::Mach::Fat(fat) => {
            // iterate arches and choose priority: Arm64 > Arm
            let mut found_arm64 = false;
            let mut found_arm = false;
            for arch in fat.iter_arches() {
                let arch = match arch {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                match map_mach_arch(arch.cputype()) {
                    BinaryArch::Arm64 => found_arm64 = true,
                    BinaryArch::Arm => found_arm = true,
                    _ => {}
                }
            }
            BinaryInfo {
                os_type: BinaryOsType::Mac,
                arch: if found_arm64 {
                    BinaryArch::Arm64
                } else if found_arm {
                    BinaryArch::Arm
                } else {
                    BinaryArch::Unknown
                },
            }
        }
    }
}

#[inline]
fn map_mach_arch(cpu_type: u32) -> BinaryArch {
    use goblin::mach::cputype::*;
    match cpu_type {
        CPU_TYPE_X86_64 => BinaryArch::X86_64,
        CPU_TYPE_ARM64 => BinaryArch::Arm64,
        CPU_TYPE_ARM => BinaryArch::Arm,
        _ => BinaryArch::Unknown,
    }
}

#[inline]
pub fn ensure_current_platform_from_bytes(bytes: &[u8]) -> crate::DriverResult<()> {
    let info = inspect_binary(bytes);

    let expect_os = BinaryOsType::current();
    let expect_arch = BinaryArch::current();

    if info.os_type != expect_os {
        return Err(crate::DriverError::ValidationError(format!(
            "Driver OS mismatch: binary={:?}, gateway={:?}",
            info.os_type, expect_os
        )));
    }
    if info.arch != expect_arch {
        return Err(crate::DriverError::ValidationError(format!(
            "Driver CPU arch mismatch: binary={:?}, gateway={:?}",
            info.arch, expect_arch
        )));
    }

    Ok(())
}

#[inline]
pub fn ensure_current_platform_from_path(path: &Path) -> crate::DriverResult<()> {
    // Extension gate first to avoid loading clearly incompatible binaries.
    let current_os = BinaryOsType::current();
    let expected_ext = current_os.expected_driver_ext();
    match path.extension().and_then(OsStr::to_str) {
        Some(ext) if expected_ext.is_empty() || ext == expected_ext => {}
        Some(ext) => {
            return Err(crate::DriverError::ValidationError(format!(
                "Driver file extension mismatch: file ext='{}', expect='{}'",
                ext, expected_ext
            )))
        }
        None => {
            return Err(crate::DriverError::ValidationError(
                "Driver path has no file extension".to_string(),
            ))
        }
    }

    // Read bytes once and validate OS/Arch strictly.
    let bytes = std::fs::read(path).map_err(|e| {
        crate::DriverError::LoadError(format!("Failed to read library {}: {}", path.display(), e))
    })?;
    ensure_current_platform_from_bytes(&bytes)
}
